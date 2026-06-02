//! Replies to host queries: DSR (5/6n), DA (primary + secondary), XTVERSION,
//! and DECSET ?1004 focus-reporting flag.
//!
//! These regressions cover the nvim hang where the editor blocked at "Did not
//! detect DSR response from terminal" because SonicTerm never answered the
//! VT220-standard status queries.

use crossbeam_channel::unbounded;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{Parser, SONIC_VERSION};

fn parser_with_reply() -> (Parser, crossbeam_channel::Receiver<Vec<u8>>) {
    let (tx, rx) = unbounded();
    (Parser::new_with_reply(Grid::new(80, 24), tx), rx)
}

fn drain(rx: &crossbeam_channel::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        out.extend_from_slice(&chunk);
    }
    out
}

#[test]
fn dsr_5n_returns_ok() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[5n");
    assert_eq!(drain(&rx), b"\x1b[0n");
}

#[test]
fn dsr_6n_returns_cursor_row_col() {
    let (mut p, rx) = parser_with_reply();
    // Move cursor to row 3, col 7 (1-indexed in CUP).
    p.advance(b"\x1b[3;7H");
    p.advance(b"\x1b[6n");
    assert_eq!(drain(&rx), b"\x1b[3;7R");
}

#[test]
fn dsr_6n_at_origin_returns_1_1() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[6n");
    assert_eq!(drain(&rx), b"\x1b[1;1R");
}

#[test]
fn da_primary_returns_vt220_with_columns() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[c");
    assert_eq!(drain(&rx), b"\x1b[?62;c");
}

#[test]
fn da_primary_with_zero_returns_vt220() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[0c");
    assert_eq!(drain(&rx), b"\x1b[?62;c");
}

#[test]
fn da_secondary_returns_vt220_firmware() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[>c");
    assert_eq!(drain(&rx), b"\x1b[>1;0;0c");
}

#[test]
fn da_secondary_with_zero_returns_vt220_firmware() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[>0c");
    assert_eq!(drain(&rx), b"\x1b[>1;0;0c");
}

#[test]
fn xtversion_returns_sonic_version() {
    let (mut p, rx) = parser_with_reply();
    p.advance(b"\x1b[>q");
    let reply = drain(&rx);
    let mut expected = Vec::new();
    expected.extend_from_slice(b"\x1bP>|");
    expected.extend_from_slice(SONIC_VERSION.as_bytes());
    expected.extend_from_slice(b"\x1b\\");
    assert_eq!(reply, expected);
}

#[test]
fn decset_1004_sets_focus_reporting_flag() {
    let (mut p, _rx) = parser_with_reply();
    assert!(!p.focus_reporting_enabled());
    p.advance(b"\x1b[?1004h");
    assert!(p.focus_reporting_enabled());
    p.advance(b"\x1b[?1004l");
    assert!(!p.focus_reporting_enabled());
}

#[test]
fn parser_without_reply_channel_does_not_panic_on_query() {
    // Backward-compat: `Parser::new` (no sender) must silently drop replies.
    let mut p = Parser::new(Grid::new(80, 24));
    p.advance(b"\x1b[5n\x1b[c\x1b[>c\x1b[>q\x1b[6n");
}
