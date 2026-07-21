//! Pure JSON wire codec for the `terminal.attach_stream` API.
//!
//! Maps [`crate::protocol::ServerMessage`] values onto newline-terminated
//! JSON event lines for the browser client, and parses the client's NDJSON
//! commands back. No I/O: encoding and parsing only.

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::protocol::ServerMessage;

/// Whether an attached seat may send input or only observe the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StreamMode {
    #[default]
    Interactive,
    View,
}

/// JSON events sent from server to client over the attach stream.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent<'a> {
    Snapshot {
        seq: u64,
        cols: u16,
        rows: u16,
        data: String,
    },
    Frame {
        seq: u64,
        cols: u16,
        rows: u16,
        data: String,
    },
    Detached {
        reason: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Error {
        code: &'a str,
        message: &'a str,
    },
}

/// Serialize `event` to a newline-terminated JSON line.
fn encode_event(event: &StreamEvent) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(event).expect("StreamEvent serializes");
    bytes.push(b'\n');
    bytes
}

/// Map a server message onto a wire event, or `None` if it has no stream mapping.
pub(crate) fn encode_server_message(msg: &ServerMessage) -> Option<Vec<u8>> {
    let event = match msg {
        ServerMessage::Terminal(frame) => {
            let data = base64::engine::general_purpose::STANDARD.encode(&frame.bytes);
            if frame.full {
                StreamEvent::Snapshot {
                    seq: frame.seq,
                    cols: frame.width,
                    rows: frame.height,
                    data,
                }
            } else {
                StreamEvent::Frame {
                    seq: frame.seq,
                    cols: frame.width,
                    rows: frame.height,
                    data,
                }
            }
        }
        ServerMessage::ServerShutdown { reason } => StreamEvent::Detached {
            reason: reason.clone().unwrap_or_else(|| "server shutdown".into()),
        },
        _ => return None,
    };
    Some(encode_event(&event))
}

/// Encode a `resize` event announcing the current terminal dimensions.
pub(crate) fn encode_resize(cols: u16, rows: u16) -> Vec<u8> {
    encode_event(&StreamEvent::Resize { cols, rows })
}

/// Encode an `error` event describing a rejected command.
pub(crate) fn encode_error(code: &str, message: &str) -> Vec<u8> {
    encode_event(&StreamEvent::Error { code, message })
}

/// Commands the browser client sends to the server over the attach stream.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireCommand {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// Parsed client command, ready for the server to act on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamCommand {
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Detach,
}

/// Parse one NDJSON line into a [`StreamCommand`].
pub(crate) fn parse_command(line: &str) -> Result<StreamCommand, String> {
    let command: WireCommand = serde_json::from_str(line).map_err(|err| err.to_string())?;
    match command {
        WireCommand::Input { data } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map(|data| StreamCommand::Input { data })
            .map_err(|err| err.to_string()),
        WireCommand::Resize { cols, rows } => Ok(StreamCommand::Resize { cols, rows }),
        WireCommand::Detach => Ok(StreamCommand::Detach),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ServerMessage, TerminalFrame};

    fn line(bytes: Vec<u8>) -> serde_json::Value {
        let text = String::from_utf8(bytes).expect("utf8 line");
        assert!(text.ends_with('\n'), "events are newline framed: {text:?}");
        serde_json::from_str(text.trim_end()).expect("json line")
    }

    #[test]
    fn full_terminal_frame_encodes_as_snapshot() {
        let event = line(
            encode_server_message(&ServerMessage::Terminal(TerminalFrame {
                seq: 1,
                width: 200,
                height: 48,
                full: true,
                bytes: b"hi".to_vec(),
            }))
            .expect("encoded"),
        );
        assert_eq!(event["type"], "snapshot");
        assert_eq!(event["cols"], 200);
        assert_eq!(event["rows"], 48);
        assert_eq!(event["seq"], 1);
        assert_eq!(event["data"], "aGk=");
    }

    #[test]
    fn partial_terminal_frame_encodes_as_frame() {
        let event = line(
            encode_server_message(&ServerMessage::Terminal(TerminalFrame {
                seq: 2,
                width: 200,
                height: 48,
                full: false,
                bytes: b"hi".to_vec(),
            }))
            .expect("encoded"),
        );
        assert_eq!(event["type"], "frame");
    }

    #[test]
    fn shutdown_encodes_as_detached() {
        let event = line(
            encode_server_message(&ServerMessage::ServerShutdown {
                reason: Some("terminal attach taken over".into()),
            })
            .expect("encoded"),
        );
        assert_eq!(event["type"], "detached");
        assert_eq!(event["reason"], "terminal attach taken over");
    }

    #[test]
    fn messages_without_json_mapping_are_dropped() {
        assert!(encode_server_message(&ServerMessage::ReloadSoundConfig).is_none());
    }

    #[test]
    fn resize_and_error_events_encode() {
        let resize = line(encode_resize(120, 40));
        assert_eq!(resize["type"], "resize");
        assert_eq!(resize["cols"], 120);
        assert_eq!(resize["rows"], 40);

        let error = line(encode_error("input_not_allowed", "seat is view-only"));
        assert_eq!(error["type"], "error");
        assert_eq!(error["code"], "input_not_allowed");
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
    }
}
