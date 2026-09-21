//! JSON wire codec for `terminal.attach_stream`.
//!
//! Maps terminal-session [`ServerMessage`] values onto newline-terminated JSON
//! events for the API client, and parses the client's NDJSON commands back.
//! No socket I/O beyond writing into a caller-provided writer.

use std::io::{self, Write};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::protocol::{ServerMessage, TerminalFrame};

/// JSON events sent from the server to the API client over the attach stream.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum StreamEvent {
    /// A full redraw; the first one after the started response is the initial snapshot.
    Snapshot {
        seq: u64,
        cols: u16,
        rows: u16,
        data: String,
    },
    /// An incremental redraw on top of the previous snapshot or frame.
    Frame {
        seq: u64,
        cols: u16,
        rows: u16,
        data: String,
    },
    /// The server ended the stream, for example on takeover or shutdown.
    Detached { reason: String },
    /// A client command was rejected; the stream stays open.
    Error { code: String, message: String },
}

impl StreamEvent {
    pub(super) fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// Map a terminal frame onto a `snapshot` (full redraw) or `frame` event.
    pub(super) fn from_frame(frame: &TerminalFrame) -> Self {
        let data = base64::engine::general_purpose::STANDARD.encode(&frame.bytes);
        let (seq, cols, rows) = (frame.seq, frame.width, frame.height);
        if frame.full {
            Self::Snapshot {
                seq,
                cols,
                rows,
                data,
            }
        } else {
            Self::Frame {
                seq,
                cols,
                rows,
                data,
            }
        }
    }

    /// Map a terminal-session server message onto a stream event, if it has one.
    pub(super) fn from_server_message(message: &ServerMessage) -> Option<Self> {
        match message {
            ServerMessage::Terminal(frame) => Some(Self::from_frame(frame)),
            ServerMessage::ServerShutdown { reason } => Some(Self::Detached {
                reason: reason.clone().unwrap_or_else(|| "server shutdown".into()),
            }),
            _ => None,
        }
    }

    /// Write this event as one newline-terminated JSON line.
    pub(super) fn write_line(&self, writer: &mut impl Write) -> io::Result<()> {
        serde_json::to_writer(&mut *writer, self).map_err(io::Error::other)?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
}

/// Commands the API client sends over the attach stream, as they appear on the wire.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireCommand {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// A parsed client command.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StreamCommand {
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// Parse one NDJSON command line.
pub(super) fn parse_command(line: &str) -> Result<StreamCommand, String> {
    match serde_json::from_str::<WireCommand>(line).map_err(|err| err.to_string())? {
        WireCommand::Input { data } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map(|data| StreamCommand::Input { data })
            .map_err(|err| format!("invalid input data: {err}")),
        WireCommand::Resize { cols, rows } if cols == 0 || rows == 0 => {
            Err("resize cols and rows must be greater than 0".into())
        }
        WireCommand::Resize { cols, rows } => Ok(StreamCommand::Resize { cols, rows }),
        WireCommand::Detach => Ok(StreamCommand::Detach),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(event: &StreamEvent) -> serde_json::Value {
        let mut bytes = Vec::new();
        event.write_line(&mut bytes).expect("event writes");
        let text = String::from_utf8(bytes).expect("utf8 line");
        assert!(text.ends_with('\n'), "events are newline framed: {text:?}");
        serde_json::from_str(text.trim_end()).expect("json line")
    }

    fn terminal_frame(full: bool) -> ServerMessage {
        ServerMessage::Terminal(TerminalFrame {
            seq: 1,
            width: 200,
            height: 48,
            full,
            bytes: b"hi".to_vec(),
        })
    }

    #[test]
    fn full_terminal_frame_encodes_as_snapshot() {
        let event = line(&StreamEvent::from_server_message(&terminal_frame(true)).expect("event"));
        assert_eq!(event["type"], "snapshot");
        assert_eq!(event["cols"], 200);
        assert_eq!(event["rows"], 48);
        assert_eq!(event["seq"], 1);
        assert_eq!(event["data"], "aGk=");
    }

    #[test]
    fn partial_terminal_frame_encodes_as_frame() {
        let event = line(&StreamEvent::from_server_message(&terminal_frame(false)).expect("event"));
        assert_eq!(event["type"], "frame");
        assert_eq!(event["data"], "aGk=");
    }

    #[test]
    fn shutdown_encodes_as_detached() {
        let event = line(
            &StreamEvent::from_server_message(&ServerMessage::ServerShutdown {
                reason: Some("terminal attach taken over".into()),
            })
            .expect("event"),
        );
        assert_eq!(event["type"], "detached");
        assert_eq!(event["reason"], "terminal attach taken over");
    }

    #[test]
    fn messages_without_stream_mapping_are_dropped() {
        assert!(StreamEvent::from_server_message(&ServerMessage::ReloadSoundConfig).is_none());
    }

    #[test]
    fn error_event_encodes_code_and_message() {
        let event = line(&StreamEvent::error("input_not_allowed", "view stream"));
        assert_eq!(event["type"], "error");
        assert_eq!(event["code"], "input_not_allowed");
        assert_eq!(event["message"], "view stream");
    }

    #[test]
    fn commands_parse() {
        assert_eq!(
            parse_command(r#"{"type":"input","data":"aGk="}"#).expect("input"),
            StreamCommand::Input {
                data: b"hi".to_vec()
            }
        );
        assert_eq!(
            parse_command(r#"{"type":"resize","cols":100,"rows":30}"#).expect("resize"),
            StreamCommand::Resize {
                cols: 100,
                rows: 30
            }
        );
        assert_eq!(
            parse_command(r#"{"type":"detach"}"#).expect("detach"),
            StreamCommand::Detach
        );
    }

    #[test]
    fn malformed_commands_report_an_error() {
        assert!(parse_command("not json").is_err());
        assert!(parse_command(r#"{"type":"input","data":"!!not base64!!"}"#).is_err());
        assert!(parse_command(r#"{"type":"resize","cols":0,"rows":30}"#).is_err());
    }
}
