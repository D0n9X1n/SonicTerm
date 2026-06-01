//! Regression tests for OSC 10/11/12 color-query replies (issue #369).
//!
//! nvim sends `OSC 11 ; ? ST` at startup to learn the terminal's default
//! background so it can render cells declared with `bg=NONE` (e.g.
//! neo-tree icon cells) using the same colour as the theme clear. Before
//! this fix Sonic silently dropped the query and nvim fell back to the
//! NeoTreeNormal default `(27,29,30)`, which differs from Sonic's actual
//! theme bg `(20,22,23)` — the visible darker rectangles in #369.

use crossbeam_channel::unbounded;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

fn drain(rx: &crossbeam_channel::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        out.extend_from_slice(&chunk);
    }
    out
}

#[test]
fn osc_11_query_replies_with_theme_background() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    // (20, 22, 23) = 0x14, 0x16, 0x17 — Sonic's default theme bg.
    p.set_theme_bg(0x14, 0x16, 0x17);
    p.advance(b"\x1b]11;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]11;rgb:1414/1616/1717\x1b\\");
}

#[test]
fn osc_11_query_uses_updated_theme_background() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_bg(0x14, 0x16, 0x17);
    p.advance(b"\x1b]11;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]11;rgb:1414/1616/1717\x1b\\");

    p.set_theme_bg(0x28, 0x2a, 0x36);
    p.advance(b"\x1b]11;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]11;rgb:2828/2a2a/3636\x1b\\");
}

#[test]
fn osc_10_query_replies_with_theme_foreground() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_fg(0xab, 0xcd, 0xef);
    p.advance(b"\x1b]10;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]10;rgb:abab/cdcd/efef\x1b\\");
}

#[test]
fn osc_12_query_replies_with_cursor_color() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_cursor(0x10, 0x20, 0x30);
    p.advance(b"\x1b]12;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]12;rgb:1010/2020/3030\x1b\\");
}

#[test]
fn osc_12_query_falls_back_to_foreground_when_cursor_unset() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_fg(0x11, 0x22, 0x33);
    p.advance(b"\x1b]12;?\x1b\\");
    assert_eq!(drain(&rx), b"\x1b]12;rgb:1111/2222/3333\x1b\\");
}

#[test]
fn osc_11_query_with_bel_terminator_uses_bel_reply() {
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_bg(0x14, 0x16, 0x17);
    // BEL (0x07) terminator instead of ST (ESC \).
    p.advance(b"\x1b]11;?\x07");
    assert_eq!(drain(&rx), b"\x1b]11;rgb:1414/1616/1717\x07");
}

#[test]
fn osc_11_query_without_theme_is_silent() {
    // If no theme has been provided, we must not invent a colour — silently
    // drop the query rather than lie to the shell.
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.advance(b"\x1b]11;?\x1b\\");
    assert!(drain(&rx).is_empty());
}

#[test]
fn osc_11_set_payload_is_ignored_not_replied() {
    // `OSC 11 ; #RRGGBB ST` is a *set*, not a query — we must NOT reply.
    // (Set support itself is out of scope for #369.)
    let (tx, rx) = unbounded();
    let mut p = Parser::new_with_reply(Grid::new(10, 5), tx);
    p.set_theme_bg(0x14, 0x16, 0x17);
    p.advance(b"\x1b]11;#123456\x1b\\");
    assert!(drain(&rx).is_empty());
}
