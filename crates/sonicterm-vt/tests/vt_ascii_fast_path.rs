//! Tests for the ASCII SWAR fast-path in `sonicterm_vt::vt::Parser::advance`.
//!
//! The fast-path bulk-prints printable-ASCII runs straight to the grid while
//! the vte state machine is in the Ground state, falling back to vte for
//! escapes, controls, and any non-ASCII byte. These tests pin down the
//! behavioural equivalence: any sequence of inputs must produce the same
//! grid (cells, attrs, cursor, scrollback) as if every byte had gone through
//! vte one at a time.

use sonicterm_grid::grid::{Color, Grid};
use sonicterm_vt::vt::Parser;

fn row_text(p: &Parser, r: u16) -> String {
    let row = p.grid().row(r);
    let mut s = String::new();
    for c in row.iter() {
        if c.ch == '\0' {
            break;
        }
        s.push(c.ch);
    }
    s
}

#[test]
fn fast_path_pure_ascii_run() {
    let mut p = Parser::new(Grid::new(80, 2));
    let input = b"Hello, world! 1234567890";
    p.advance(input);
    let g = p.grid();
    for (i, &b) in input.iter().enumerate() {
        assert_eq!(g.row(0)[i].ch, b as char, "mismatch at col {i}");
    }
    assert_eq!(g.cursor.col, input.len() as u16);
    assert_eq!(g.cursor.row, 0);
}

#[test]
fn fast_path_interleaved_with_sgr() {
    // Two printable runs split by a CSI SGR (red) and a CSI SGR (reset).
    // First run "Hello" — fast-path. ESC enters vte, CSI 31 m sets fg=red.
    // "World" — fast-path again, but now with attrs=red. ESC enters vte
    // again, CSI 0 m resets.
    let mut p = Parser::new(Grid::new(80, 1));
    p.advance(b"Hello\x1b[31mWorld\x1b[0m");
    let g = p.grid();
    assert_eq!(&row_text(&p, 0)[..10], "HelloWorld");
    // First 5 cells should be default fg, next 5 should be red (Indexed 1).
    for i in 0..5 {
        assert_eq!(g.row(0)[i].fg, Color::Default, "cell {i} should be default");
    }
    for i in 5..10 {
        assert_eq!(g.row(0)[i].fg, Color::Indexed(1), "cell {i} should be red");
    }
    assert_eq!(g.cursor.col, 10);
}

#[test]
fn fast_path_pure_cjk_skips_fast_path_but_renders_correctly() {
    // Every byte of UTF-8 CJK is >= 0x80 → fast-path scan finds zero
    // printable bytes on the first try and immediately falls back to vte
    // for the entire input. vte's utf8 collector reassembles each
    // 3-byte char and dispatches to print() — so the resulting grid must
    // contain "中文中文" with each glyph occupying 2 columns (wide).
    let mut p = Parser::new(Grid::new(80, 1));
    p.advance("中文中文".as_bytes());
    let g = p.grid();
    assert_eq!(g.row(0)[0].ch, '中');
    assert_eq!(g.row(0)[2].ch, '文');
    assert_eq!(g.row(0)[4].ch, '中');
    assert_eq!(g.row(0)[6].ch, '文');
    // 4 wide glyphs → cursor advanced 8 columns.
    assert_eq!(g.cursor.col, 8);
}

#[test]
fn fast_path_byte_boundary_split_across_advance_calls() {
    // Feed the same payload as `fast_path_interleaved_with_sgr` but split
    // into single-byte advance() calls — the ground-state tracker must
    // survive across calls and produce an identical grid. This is the
    // canonical regression case for "tracker lost between chunks".
    let mut p = Parser::new(Grid::new(80, 1));
    for b in b"Hello\x1b[31mWorld\x1b[0m" {
        p.advance(&[*b]);
    }
    let g = p.grid();
    assert_eq!(&row_text(&p, 0)[..10], "HelloWorld");
    for i in 5..10 {
        assert_eq!(g.row(0)[i].fg, Color::Indexed(1));
    }
}

#[test]
fn fast_path_handles_lf_via_vte() {
    // LF (0x0A) must be treated as a control (linefeed), NOT printed as
    // ASCII — i.e. the run before the LF goes via fast-path, the LF itself
    // goes via vte execute(), and the run after the LF lands on row 1.
    let mut p = Parser::new(Grid::new(80, 4));
    p.advance(b"first\r\nsecond");
    assert_eq!(&row_text(&p, 0)[..5], "first");
    assert_eq!(&row_text(&p, 1)[..6], "second");
}

#[test]
fn fast_path_equivalent_to_byte_at_a_time() {
    // Strong equivalence check: feed the same mixed payload two ways and
    // assert the resulting grids match cell-for-cell, attribute-for-
    // attribute, including the cursor.
    let payload: Vec<u8> = {
        let mut v = Vec::new();
        v.extend_from_slice(b"prompt $ ");
        v.extend_from_slice(b"\x1b[1mecho\x1b[0m hello");
        v.extend_from_slice("中文".as_bytes());
        v.extend_from_slice(" 🎉 done\n".as_bytes());
        v.extend_from_slice(b"next line\r\n");
        v
    };
    let mut bulk = Parser::new(Grid::new(80, 5));
    bulk.advance(&payload);
    let mut drip = Parser::new(Grid::new(80, 5));
    for b in &payload {
        drip.advance(&[*b]);
    }
    for r in 0..5 {
        for c in 0..80 {
            let a = &bulk.grid().row(r)[c as usize];
            let b = &drip.grid().row(r)[c as usize];
            assert_eq!(a.ch, b.ch, "ch mismatch row {r} col {c}");
            assert_eq!(a.fg, b.fg, "fg mismatch row {r} col {c}");
            assert_eq!(a.bg, b.bg, "bg mismatch row {r} col {c}");
            assert_eq!(a.flags, b.flags, "flags mismatch row {r} col {c}");
        }
    }
    assert_eq!(bulk.grid().cursor, drip.grid().cursor);
}

#[test]
fn fast_path_does_not_resume_inside_dcs() {
    // Regression for PR #138: execute() used to set ground=true unconditionally,
    // but vte dispatches C0 controls (BEL/LF/CR/HT/BS) even while the state
    // machine is inside an ESC/CSI/OSC/DCS sequence. If ground flipped back to
    // true mid-escape, the next byte of the DCS payload would be consumed by
    // the SWAR fast-path and printed verbatim instead of being swallowed by
    // vte's DCS state. Here we feed a DCS XTGETTCAP query with printable bytes
    // inside the payload — none of those payload bytes must land on the grid.
    let mut p = Parser::new(Grid::new(80, 2));
    // hello, then DCS + q 544e (= "TN") ST, then world.
    p.advance(b"hello\x1bP+q544e\x1b\\world");
    let text = row_text(&p, 0);
    // "hello" and "world" should be adjacent — the DCS payload contributed
    // nothing to the visible grid.
    assert_eq!(&text[..10], "helloworld", "got {text:?}");
}

#[test]
fn fast_path_does_not_resume_after_bel_inside_csi() {
    // CSI parameter bytes followed by BEL (0x07): vte calls execute(BEL) while
    // still parsing the CSI. The pre-fix code would set ground=true here, so
    // the next byte ("c" — the CSI final byte) would slip into the SWAR fast-
    // path and be printed as ASCII instead of completing the CSI. Verify that
    // does not happen by feeding a CSI with an embedded BEL and a final byte
    // that, if mishandled, would visibly land on the grid.
    let mut p = Parser::new(Grid::new(80, 1));
    // "a" then ESC [ 3 1 BEL m "b" — the BEL inside CSI must not break the
    // sequence into "31" + "m" + "b" via fast-path. After vte processes the
    // (possibly-malformed) CSI, "b" should print normally.
    p.advance(b"a\x1b[31\x07mb");
    let text = row_text(&p, 0);
    // 'a' must be present and 'b' must be present; no stray '3', '1', or 'm'
    // from the CSI payload should appear in the grid.
    assert!(text.starts_with('a'), "got {text:?}");
    assert!(text.contains('b'), "got {text:?}");
    assert!(!text.contains('3'), "CSI payload leaked: {text:?}");
    assert!(!text.contains('1'), "CSI payload leaked: {text:?}");
}
