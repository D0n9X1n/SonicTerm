//! Spec-discriminator tests for ED (CSI J) and DECSTBM (CSI r).
//!
//! Filed under #414: "Claude in nvim leaves stale overlay row after fzf-style
//! exit". Diagnosis on the issue (round 1) hypothesised that either
//!   R1: cursor is at the wrong row when ED0 fires (so erase_below misses the
//!       trailing row), or
//!   R2: ED is correct and the renderer cache is the culprit, or
//!   R3: Claude sends a different sequence to SonicTerm vs WezTerm based on
//!       terminfo (most likely DECSTBM-then-print without ED at all).
//!
//! These tests document the SPEC behavior of ED0 + DECSTBM that the fix loop
//! depends on. If any FAIL on main, the diagnosis is wrong and a real ED/STBM
//! bug exists in `vt.rs` that must be fixed before chasing the overlay symptom.

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

fn row_text(p: &Parser, r: u16) -> String {
    p.grid().row(r).iter().map(|c| c.ch).collect()
}

fn fill(p: &mut Parser, ch: u8) {
    let (rows, cols) = (p.grid().rows, p.grid().cols);
    for r in 0..rows {
        p.advance(format!("\x1b[{};1H", r + 1).as_bytes());
        for _ in 0..cols {
            p.advance(&[ch]);
        }
    }
}

#[test]
fn ed0_from_home_clears_all_rows() {
    // Cursor at home (0,0) + CSI 0 J  ⇒ erase from cursor to end-of-screen,
    // which is the entire grid. Every row must be blank.
    let mut p = parser(8, 4);
    fill(&mut p, b'X');
    p.advance(b"\x1b[H"); // CUP home
    p.advance(b"\x1b[0J"); // ED0
    for r in 0..4 {
        assert_eq!(row_text(&p, r), "        ", "row {r} should be blank");
    }
}

#[test]
fn ed0_from_bottom_only_clears_bottom_partial() {
    // Cursor at last row, mid-column. ED0 erases cursor → eos, which is just
    // the tail of the last row. Rows above must be untouched.
    let mut p = parser(8, 4);
    fill(&mut p, b'X');
    // CUP to last row (1-based row 4), col 5 (1-based).
    p.advance(b"\x1b[4;5H");
    p.advance(b"\x1b[0J"); // ED0
    assert_eq!(row_text(&p, 0), "XXXXXXXX");
    assert_eq!(row_text(&p, 1), "XXXXXXXX");
    assert_eq!(row_text(&p, 2), "XXXXXXXX");
    // Last row: cols 0..4 keep 'X', cols 4..8 are blanked (cursor at col 4 0-based).
    assert_eq!(row_text(&p, 3), "XXXX    ");
}

#[test]
fn decstbm_full_screen_does_not_clear() {
    // Per spec: DECSTBM sets margins + homes the cursor, but does NOT clear
    // the screen. If Claude is sending CSI 1;<rows>r and we ever start
    // clearing on STBM, that's the overlay bug.
    let mut p = parser(8, 4);
    fill(&mut p, b'X');
    p.advance(b"\x1b[1;4r"); // DECSTBM full screen
    for r in 0..4 {
        assert_eq!(row_text(&p, r), "XXXXXXXX", "row {r} must be unchanged by DECSTBM");
    }
    // Cursor moved to home per spec.
    assert_eq!((p.grid().cursor.row, p.grid().cursor.col), (0, 0));
}

#[test]
fn shell_prompt_redraw_preserves_above_cursor() {
    // The real-world bug the e2e test caught: a shell that runs `ls`,
    // sees the output, then redraws its prompt via ED 0 should NOT
    // wipe prior output.
    let mut p = Parser::new(Grid::new(20, 4));
    p.advance(b"prompt$ ls\r\nfile1 file2\r\nprompt$ ");
    p.advance(b"\x1b[0J");
    assert_eq!(p.grid().row(0)[0].ch, 'p');
    assert_eq!(p.grid().row(1)[0].ch, 'f');
    assert_eq!(p.grid().row(2)[0].ch, 'p');
}
