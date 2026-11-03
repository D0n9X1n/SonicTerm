//! Pins the JSON-RPC framing: lines survive read timeouts, an over-long line is refused rather
//! than cut, the preamble names the caller's pane, and `initialize` negotiates a version.

use std::io::{BufReader, Read};

use super::*;

/// A reader that hands out one scripted step per read: bytes, or a read timeout.
struct ScriptedReader {
    steps: Vec<Option<Vec<u8>>>,
}

impl Read for ScriptedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.steps.first().cloned() {
            None => Ok(0),
            Some(None) => {
                self.steps.remove(0);
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            Some(Some(bytes)) => {
                // A step larger than the caller's buffer is handed out over several reads.
                let count = bytes.len().min(buf.len());
                buf[..count].copy_from_slice(&bytes[..count]);
                if count == bytes.len() {
                    self.steps.remove(0);
                } else {
                    self.steps[0] = Some(bytes[count..].to_vec());
                }
                Ok(count)
            }
        }
    }
}

/// A line split across reads with a timeout between its parts arrives whole, CRLF stripped,
/// and the reader then reports the peer's close.
#[test]
fn a_line_split_by_a_timeout_arrives_whole() {
    let steps = vec![Some(b"{\"a\":".to_vec()), None, Some(b"1}\r\n".to_vec())];
    let mut reader = BufReader::new(ScriptedReader { steps });
    let mut partial = Vec::new();
    assert_eq!(read_frame(&mut reader, &mut partial), Frame::Idle, "the first part times out");
    assert_eq!(read_frame(&mut reader, &mut partial), Frame::Line("{\"a\":1}".into()));
    assert_eq!(read_frame(&mut reader, &mut partial), Frame::Closed);
}

/// A line longer than the cap is refused, never cut into a shorter message.
#[test]
fn an_over_long_line_is_refused() {
    let long = vec![b'x'; MAX_LINE_BYTES + 10];
    let mut reader = BufReader::new(ScriptedReader { steps: vec![Some(long)] });
    let mut partial = Vec::new();
    // The scripted reader hands the whole buffer in one read; BufReader splits it.
    let mut frame = read_frame(&mut reader, &mut partial);
    while frame == Frame::Idle {
        frame = read_frame(&mut reader, &mut partial);
    }
    assert_eq!(frame, Frame::TooLong);
}

/// The preamble names the caller's pane, as a number or a string; requests, notifications and
/// malformed lines are told apart.
#[test]
fn lines_are_classified() {
    assert_eq!(
        parse_line(r#"{"sonicterm_bridge":1,"pane_id":"42"}"#),
        Incoming::Preamble { pane_id: Some(42) }
    );
    assert_eq!(
        parse_line(r#"{"sonicterm_bridge":1,"pane_id":null}"#),
        Incoming::Preamble { pane_id: None }
    );
    assert_eq!(
        parse_line(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#),
        Incoming::Request { id: json!(3), method: "ping".into(), params: Value::Null }
    );
    assert_eq!(
        parse_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        Incoming::Notification { method: "notifications/initialized".into() }
    );
    assert!(matches!(parse_line("not json"), Incoming::Invalid { code: PARSE_ERROR, .. }));
    assert!(matches!(parse_line(r#"{"id":1}"#), Incoming::Invalid { code: INVALID_REQUEST, .. }));
}

/// `initialize` echoes a revision this server speaks and otherwise offers its newest; the client
/// name is capped and only read for display.
#[test]
fn initialize_negotiates_a_version() {
    let offered =
        json!({ "protocolVersion": "2025-06-18", "clientInfo": { "name": "claude-code" } });
    assert_eq!(initialize_result(&offered)["protocolVersion"], "2025-06-18");
    let unknown = json!({ "protocolVersion": "1999-01-01" });
    assert_eq!(initialize_result(&unknown)["protocolVersion"], PROTOCOL_VERSIONS[0]);
    assert_eq!(client_name(&offered).as_deref(), Some("claude-code"));
    let long_name = json!({ "clientInfo": { "name": "x".repeat(200) } });
    assert_eq!(client_name(&long_name).map(|name| name.len()), Some(64));
}

/// Tool results carry structured content and a summary; tool errors carry a stable code.
#[test]
fn tool_results_and_errors_have_their_shape() {
    let ok = tool_result("1 pane", json!({ "panes": [] }));
    assert_eq!(
        (ok["isError"].as_bool(), ok["structuredContent"]["panes"].is_array()),
        (Some(false), true)
    );
    let refused = tool_error("readonly", "the window is in READONLY");
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["error"], "readonly");
}

/// The client name shown to the user loses control characters and is capped at 64 characters.
#[test]
fn client_names_are_sanitized() {
    let params = serde_json::json!({ "clientInfo": { "name": "evil\u{1b}[2J\nname" } });
    assert_eq!(client_name(&params).as_deref(), Some("evil[2Jname"));
    let long = serde_json::json!({ "clientInfo": { "name": "x".repeat(100) } });
    assert_eq!(client_name(&long).map(|name| name.len()), Some(64));
}

/// Claude Code probes with `server/discover` and then offers `2025-11-25` to `initialize`; the
/// server echoes it, and never answers `initialize` with a revision that has no `initialize`.
#[test]
fn initialize_answers_a_handshake_revision() {
    let claude_code = json!({ "protocolVersion": "2025-11-25", "capabilities": {} });
    assert_eq!(initialize_result(&claude_code)["protocolVersion"], "2025-11-25");
    let stateless = json!({ "protocolVersion": "2026-07-28" });
    assert_eq!(initialize_result(&stateless)["protocolVersion"], "2025-11-25");
    assert!(!PROTOCOL_VERSIONS.contains(&"2026-07-28"));
}
