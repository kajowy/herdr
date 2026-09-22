//! `terminal.attach_stream`: a JSON bridge onto the terminal session protocol.
//!
//! The handler connects to the server's own client socket as a terminal
//! session client, observing (view) or controlling (interactive) the target
//! terminal under the usual single-controller rule, and transcodes between
//! that binary protocol and NDJSON on the API connection.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;

use crate::api::schema::{
    ErrorBody, ErrorResponse, ResponseResult, SuccessResponse, TerminalAttachStreamMode,
    TerminalAttachStreamParams,
};
use crate::ipc::{
    is_connection_closed_error, poll_local_stream_read_count, set_local_stream_polling,
    LocalStream, LocalStreamReadCount,
};
use crate::protocol::{self, ClientMessage, ServerMessage, TerminalFrame, MAX_GRAPHICS_FRAME_SIZE};

use super::{write_json_line, write_json_line_allow_disconnect, APP_RESPONSE_TIMEOUT};

mod codec;

use codec::{parse_command, StreamCommand, StreamEvent};

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MAX_COMMAND_LINE_BYTES: usize = 1024 * 1024;
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLIENT_LISTENER_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) fn serve(
    mut stream: LocalStream,
    request_id: String,
    params: TerminalAttachStreamParams,
    running: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mode = params.mode;
    let mut session = match open_session(&params) {
        Ok(session) => session,
        Err(error) => {
            return write_json_line_allow_disconnect(
                &mut stream,
                &ErrorResponse {
                    id: request_id,
                    error,
                },
            )
        }
    };

    let frames = spawn_session_reader(&session)?;
    let snapshot = match wait_for_first_frame(&frames) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return write_json_line_allow_disconnect(
                &mut stream,
                &ErrorResponse {
                    id: request_id,
                    error,
                },
            )
        }
    };

    let started = write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id,
            result: ResponseResult::AttachStreamStarted {
                terminal_id: params.terminal_id,
                cols: snapshot.width,
                rows: snapshot.height,
            },
        },
    )
    .and_then(|()| StreamEvent::from_frame(&snapshot).write_line(&mut stream));

    let result = match started {
        Ok(()) => relay(&mut stream, &mut session, &frames, mode, running),
        Err(err) => Err(err),
    };
    // Release the terminal session; the server closes it, which ends the reader thread.
    let _ = protocol::write_message(&mut session, &ClientMessage::Detach);
    match result {
        Err(err) if is_connection_closed_error(&err) => Ok(()),
        result => result,
    }
}

/// Connects a terminal session client and requests observe or control.
fn open_session(params: &TerminalAttachStreamParams) -> Result<LocalStream, ErrorBody> {
    let unavailable = |message: String| ErrorBody {
        code: "server_unavailable".into(),
        message,
    };
    wait_for_client_listener();
    let mut session = crate::client::open_terminal_session_stream(
        params.cols.unwrap_or(DEFAULT_COLS),
        params.rows.unwrap_or(DEFAULT_ROWS),
    )
    .map_err(unavailable)?;
    session
        .set_nonblocking(false)
        .map_err(|err| unavailable(err.to_string()))?;

    let target = params.terminal_id.clone();
    let request = match params.mode {
        TerminalAttachStreamMode::View => ClientMessage::ObserveTerminal { target },
        TerminalAttachStreamMode::Interactive => ClientMessage::ControlTerminal {
            target,
            takeover: params.takeover,
        },
    };
    protocol::write_message(&mut session, &request).map_err(|err| unavailable(err.to_string()))?;
    Ok(session)
}

/// Waits briefly for the client socket: the API socket starts before the
/// client listener, so a request can race server startup.
fn wait_for_client_listener() {
    let path = crate::server::socket_paths::client_socket_path();
    let deadline = Instant::now() + APP_RESPONSE_TIMEOUT;
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(CLIENT_LISTENER_POLL_INTERVAL);
    }
}

/// Reads server messages on a cloned session stream until the session ends.
fn spawn_session_reader(session: &LocalStream) -> io::Result<mpsc::Receiver<ServerMessage>> {
    let mut reader = session.try_clone()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        let Ok(message) = protocol::read_message(&mut reader, MAX_GRAPHICS_FRAME_SIZE) else {
            return;
        };
        let shutdown = matches!(message, ServerMessage::ServerShutdown { .. });
        if tx.send(message).is_err() || shutdown {
            return;
        }
    });
    Ok(rx)
}

/// Waits for the session's first frame, or maps the server's refusal to an API error.
fn wait_for_first_frame(
    frames: &mpsc::Receiver<ServerMessage>,
) -> Result<TerminalFrame, ErrorBody> {
    loop {
        let message = frames
            .recv_timeout(APP_RESPONSE_TIMEOUT)
            .map_err(|_| ErrorBody {
                code: "server_unavailable".into(),
                message: "terminal session ended before its first frame".into(),
            })?;
        match message {
            ServerMessage::ServerShutdown { reason } => {
                let message = reason.unwrap_or_else(|| "terminal session refused".into());
                return Err(ErrorBody {
                    code: refusal_code(&message).into(),
                    message,
                });
            }
            ServerMessage::Terminal(frame) => return Ok(frame),
            _ => {}
        }
    }
}

/// Classifies the server's terminal session refusal reason into an API error code.
fn refusal_code(reason: &str) -> &'static str {
    if reason.contains("not found") {
        "terminal_not_found"
    } else if reason.contains("already has an attached client") {
        "terminal_busy"
    } else {
        "attach_failed"
    }
}

/// Moves frames to the API client and client commands to the session until either side ends.
fn relay(
    stream: &mut LocalStream,
    session: &mut LocalStream,
    frames: &mpsc::Receiver<ServerMessage>,
    mode: TerminalAttachStreamMode,
    running: &Arc<AtomicBool>,
) -> io::Result<()> {
    let mut pending = Vec::new();
    let mut chunk = [0_u8; 4096];
    while running.load(Ordering::Relaxed) {
        loop {
            match frames.try_recv() {
                Ok(message) => {
                    if !forward_server_message(stream, &message)? {
                        return Ok(());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        set_local_stream_polling(stream, true)?;
        let read = poll_local_stream_read_count(stream, &mut chunk);
        set_local_stream_polling(stream, false)?;
        match read? {
            LocalStreamReadCount::Closed => return Ok(()),
            LocalStreamReadCount::Data(count) => pending.extend_from_slice(&chunk[..count]),
            LocalStreamReadCount::Pending => {
                match frames.recv_timeout(STREAM_POLL_INTERVAL) {
                    Ok(message) => {
                        if !forward_server_message(stream, &message)? {
                            return Ok(());
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
                continue;
            }
        }

        while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let message = match parse_command(line) {
                Ok(StreamCommand::Detach) => return Ok(()),
                Ok(StreamCommand::Input { .. }) if mode == TerminalAttachStreamMode::View => {
                    StreamEvent::error("input_not_allowed", "view streams cannot send input")
                        .write_line(stream)?;
                    continue;
                }
                Ok(StreamCommand::Input { data }) => ClientMessage::Input { data },
                Ok(StreamCommand::Resize { cols, rows }) => ClientMessage::Resize {
                    cols,
                    rows,
                    cell_width_px: 0,
                    cell_height_px: 0,
                    pixel_mouse: false,
                },
                Err(err) => {
                    StreamEvent::error("invalid_command", err).write_line(stream)?;
                    continue;
                }
            };
            if protocol::write_message(session, &message).is_err() {
                return Ok(());
            }
        }
        if pending.len() > MAX_COMMAND_LINE_BYTES {
            StreamEvent::error("invalid_command", "command line is too long").write_line(stream)?;
            return Ok(());
        }
    }
    Ok(())
}

/// Writes one session message to the API client. Returns false once the session has ended.
fn forward_server_message(stream: &mut LocalStream, message: &ServerMessage) -> io::Result<bool> {
    if let Some(event) = StreamEvent::from_server_message(message) {
        event.write_line(stream)?;
    }
    Ok(!matches!(message, ServerMessage::ServerShutdown { .. }))
}

#[cfg(test)]
mod tests {
    use super::refusal_code;

    #[test]
    fn refusal_reasons_map_to_api_error_codes() {
        assert_eq!(
            refusal_code("terminal session observe failed: terminal target t9 not found"),
            "terminal_not_found"
        );
        assert_eq!(
            refusal_code(
                "terminal attach failed: terminal t1 already has an attached client; retry with --takeover"
            ),
            "terminal_busy"
        );
        assert_eq!(
            refusal_code("terminal attach failed: terminal t1 has a read in progress; retry"),
            "attach_failed"
        );
    }
}
