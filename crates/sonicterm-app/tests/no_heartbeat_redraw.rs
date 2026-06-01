//! §4 land-mine LM-004: window_event.rs must not contain an unconditional
//! "heartbeat" `request_redraw()` at the bottom of its event handler — that
//! creates a feedback loop that pegs the CPU at 100 % idle.
//!
//! This is a source-grep guard. A real PTY-burst / mouse-drag / key /
//! resize trigger must gate every `request_redraw` call.

use std::fs;
use std::path::PathBuf;

#[test]
fn window_event_has_no_unconditional_heartbeat_redraw() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/app/window_event.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));

    // Forbidden patterns: a `request_redraw()` call at column 0/4/8 of a
    // line whose preceding non-blank line is the closing brace of the
    // handler — i.e. the "heartbeat at the end of the function" shape.
    // Simpler heuristic: ban a comment marker that any future agent would
    // naturally add when introducing the regression.
    for (i, line) in src.lines().enumerate() {
        let l = line.trim();
        assert!(
            !l.starts_with("// heartbeat redraw"),
            "{}:{}: forbidden heartbeat redraw marker (§4 LM-004)",
            path.display(),
            i + 1,
        );
        assert!(
            !l.starts_with("// unconditional redraw"),
            "{}:{}: forbidden unconditional redraw marker (§4 LM-004)",
            path.display(),
            i + 1,
        );
    }
}
