//! Alt-screen private-mode coverage: DEC ?1047, ?1048, and the land-mine
//! `?1049h repeated must not clobber saved cursor` (CLAUDE.md §4).
//!
//! Filed in step-1 instrumentation PR for issue #414. These tests do NOT
//! attempt to fix the user-visible overlay smear — they pin down the four
//! private-mode semantics so a future fix has regression cover.

use sonic_grid::grid::Grid;
use sonic_vt::vt::Parser;

fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

fn row_text(p: &Parser, r: u16) -> String {
    let row = p.grid().row(r);
    let mut s = String::new();
    for c in row.iter() {
        s.push(c.ch);
    }
    s
}

#[test]
fn dec_1047h_enters_alt_screen_with_blank_grid() {
    let mut p = parser(8, 3);
    p.advance(b"HELLO");
    assert!(!p.grid().is_alt());
    p.advance(b"\x1b[?1047h");
    assert!(p.grid().is_alt());
    // Alt screen is blank.
    assert_eq!(row_text(&p, 0).trim_end(), "");
    assert_eq!(row_text(&p, 1).trim_end(), "");
}

#[test]
fn dec_1047l_exits_alt_screen_and_restores_primary_content() {
    let mut p = parser(8, 3);
    p.advance(b"HELLO");
    p.advance(b"\x1b[?1047h");
    p.advance(b"ALT");
    assert!(p.grid().is_alt());
    p.advance(b"\x1b[?1047l");
    assert!(!p.grid().is_alt());
    // Primary content visible again.
    assert_eq!(&row_text(&p, 0)[..5], "HELLO");
}

#[test]
fn dec_1048_round_trips_cursor_without_touching_screen() {
    let mut p = parser(8, 3);
    p.advance(b"HELLO");
    let before = p.grid().cursor;
    // Save cursor.
    p.advance(b"\x1b[?1048h");
    // Move cursor and write more.
    p.advance(b"\x1b[1;1H");
    p.advance(b"X");
    assert_ne!(p.grid().cursor, before);
    // Restore.
    p.advance(b"\x1b[?1048l");
    assert_eq!(p.grid().cursor, before);
    // ?1048 must not have entered alt screen.
    assert!(!p.grid().is_alt());
    // Screen content is whatever we wrote — X at top-left is preserved.
    assert_eq!(&row_text(&p, 0)[..1], "X");
}

#[test]
fn dec_1049h_repeated_does_not_clobber_saved_cursor() {
    // Mirrors the existing land-mine in CLAUDE.md §4 — `?1049h` must be a
    // no-op when already in alt screen so vim/fzf re-entry doesn't lose the
    // primary-screen cursor.
    let mut p = parser(8, 3);
    p.advance(b"AB");
    let primary_cursor = p.grid().cursor;
    p.advance(b"\x1b[?1049h");
    assert!(p.grid().is_alt());
    // Move cursor inside alt screen.
    p.advance(b"\x1b[2;5H");
    // Second ?1049h while already in alt — must be a no-op.
    p.advance(b"\x1b[?1049h");
    // Leave alt screen.
    p.advance(b"\x1b[?1049l");
    assert!(!p.grid().is_alt());
    // Primary-screen cursor restored, not the alt-screen one.
    assert_eq!(p.grid().cursor, primary_cursor);
}
