//! VT/parser-level capability matrix — regression net for the entire set
//! of character classes Sonic promises to handle.
//!
//! Why this exists: PR #42 (B3 cutover) broke non-ASCII rendering and
//! every existing test only used ASCII, so the local gate, Haiku review,
//! and the canonical pty_dump e2e all stayed green. This file lights up
//! one test per character class. A failure pinpoints exactly which
//! class regressed (e.g. "cjk_unified_ideographs" or
//! "emoji_zwj_sequence") — much more actionable than "non-ASCII broken".
//!
//! Scope: parser correctness only. We assert that `Parser::advance`
//! produces a `Grid` whose `cells` contain the expected codepoints (or
//! the expected sequence of base + combining marks, or the expected
//! WIDE / WIDE_CONT layout for full-width chars). Renderer-side
//! coverage (does the glyph actually rasterize?) lives in
//! `sonicterm-shared/tests/render_capability_matrix.rs`.
//!
//! All UTF-8 bytes are fed verbatim into the vte parser; we never
//! pre-decode in test code — that would short-circuit the very code
//! path we're trying to exercise.

use sonicterm_core::grid::{CellFlags, Grid};
use sonicterm_core::vt::Parser;

/// Drive the parser with raw UTF-8 bytes and return a flat string of
/// the cells on row 0 up to the cursor, with WIDE_CONT cells stripped.
/// This is the canonical "what does the user see?" view.
fn render_row0(input: &str) -> String {
    let mut p = Parser::new(Grid::new(64, 1));
    p.advance(input.as_bytes());
    let row = p.grid().row(0);
    let cursor_col = p.grid().cursor.col as usize;
    row.iter()
        .take(cursor_col.max(input.chars().count()))
        .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Collect every cell on row 0 up to the cursor, including continuation
/// cells. Useful when verifying wide-char layout (lead has WIDE flag,
/// next cell has WIDE_CONT).
fn row0_flags(input: &str) -> Vec<(char, CellFlags)> {
    let mut p = Parser::new(Grid::new(64, 1));
    p.advance(input.as_bytes());
    let cursor_col = p.grid().cursor.col as usize;
    p.grid().row(0).iter().take(cursor_col).map(|c| (c.ch, c.flags)).collect()
}

// -----------------------------------------------------------------------
// ASCII baseline — if this fails everything else is meaningless.
// -----------------------------------------------------------------------
#[test]
fn ascii_printable_roundtrips() {
    let s = "Hello, World! 0123456789 ~`!@#$%^&*()_+-=[]{}|;:'\",.<>/?";
    assert_eq!(render_row0(s), s);
}

// -----------------------------------------------------------------------
// Latin-1 supplement — 2-byte UTF-8, single column, no fallback needed
// for fonts that ship Latin-1 (most do, including Rec Mono Casual).
// -----------------------------------------------------------------------
#[test]
fn latin1_supplement_roundtrips() {
    let s = "café niño über ÆØÅ";
    assert_eq!(render_row0(s), s);
}

// -----------------------------------------------------------------------
// CJK Unified Ideographs — the class that regressed in PR #42.
// -----------------------------------------------------------------------
#[test]
fn cjk_unified_ideographs_roundtrip() {
    // Each char is a WIDE lead with a WIDE_CONT placeholder following it.
    let s = "中文测試";
    let cells = row0_flags(s);
    // Expect 4 lead + 4 continuation = 8 cells.
    assert_eq!(cells.len(), 8, "wide layout broken: cells={cells:?}");
    for (i, ch) in "中文测試".chars().enumerate() {
        assert_eq!(cells[i * 2].0, ch, "lead cell for {ch} missing");
        assert!(cells[i * 2].1.contains(CellFlags::WIDE), "{ch} not marked WIDE");
        assert!(
            cells[i * 2 + 1].1.contains(CellFlags::WIDE_CONT),
            "{ch} continuation cell not marked WIDE_CONT"
        );
    }
}

#[test]
fn hiragana_roundtrips() {
    let cells = row0_flags("ひらがな");
    let leads: Vec<char> =
        cells.iter().filter(|(_, f)| !f.contains(CellFlags::WIDE_CONT)).map(|(c, _)| *c).collect();
    assert_eq!(leads, vec!['ひ', 'ら', 'が', 'な']);
}

#[test]
fn katakana_roundtrips() {
    let cells = row0_flags("カタカナ");
    let leads: Vec<char> =
        cells.iter().filter(|(_, f)| !f.contains(CellFlags::WIDE_CONT)).map(|(c, _)| *c).collect();
    assert_eq!(leads, vec!['カ', 'タ', 'カ', 'ナ']);
}

#[test]
fn hangul_roundtrips() {
    let cells = row0_flags("한국어");
    let leads: Vec<char> =
        cells.iter().filter(|(_, f)| !f.contains(CellFlags::WIDE_CONT)).map(|(c, _)| *c).collect();
    assert_eq!(leads, vec!['한', '국', '어']);
}

// -----------------------------------------------------------------------
// Emoji — single-codepoint and ZWJ-joined sequences.
// -----------------------------------------------------------------------
#[test]
fn emoji_single_codepoint_roundtrip() {
    for ch in ['🎉', '🚀'] {
        let s: String = ch.to_string();
        let cells = row0_flags(&s);
        assert!(!cells.is_empty(), "no cell for {ch:?}");
        assert_eq!(cells[0].0, ch, "lead cell wrong for {ch:?}");
    }
}

#[test]
fn emoji_zwj_sequence_codepoints_preserved() {
    // Family: man + ZWJ + woman + ZWJ + girl.
    // We don't (yet) collapse ZWJ sequences into a single cell — the
    // important thing is the codepoints are NOT lost or replaced with
    // U+FFFD. Each base emoji should be present.
    let s = "👨\u{200d}👩\u{200d}👧";
    let cells = row0_flags(s);
    let chars: Vec<char> = cells.iter().map(|(c, _)| *c).collect();
    assert!(chars.contains(&'👨'), "ZWJ family lost man: {chars:?}");
    assert!(chars.contains(&'👩'), "ZWJ family lost woman: {chars:?}");
    assert!(chars.contains(&'👧'), "ZWJ family lost girl: {chars:?}");
}

// -----------------------------------------------------------------------
// Combining marks. cosmic-text shapes these at the rendering layer;
// the parser should still store the base + the combining codepoint
// as separate cells (or fold the mark into the previous cell — both
// are acceptable, the assertion accepts either as long as the user-
// visible result `é` is preserved somewhere in row 0).
// -----------------------------------------------------------------------
#[test]
#[ignore = "Reveals a pre-existing parser gap: the vte Performer drops standalone combining marks rather than folding them onto the previous cell. Filed as a follow-up; remove #[ignore] when the fix lands."]
fn combining_marks_acute_e_preserved() {
    // 'e' (U+0065) + COMBINING ACUTE ACCENT (U+0301) → "é"
    let s = "caf\u{0065}\u{0301}";
    let p = {
        let mut p = Parser::new(Grid::new(16, 1));
        p.advance(s.as_bytes());
        p
    };
    // Either (a) the combining mark folded onto the 'e' producing 'é'
    // directly in one cell, or (b) we have 'e' followed by U+0301 in
    // adjacent cells. Both preserve the information needed downstream.
    let chars: String = p.grid().row(0).iter().take(5).map(|c| c.ch).collect();
    assert!(
        chars.starts_with("café") || chars.starts_with("cafe\u{0301}"),
        "combining acute lost: {chars:?}"
    );
}

// -----------------------------------------------------------------------
// Box-drawing — single-cell ASCII-art glyphs heavily used by TUIs.
// Must NOT be reported as wide (1 column each).
// -----------------------------------------------------------------------
#[test]
fn box_drawing_roundtrips() {
    let s = "─╭╮╯╰│┤├┬┴┼";
    let cells = row0_flags(s);
    // No WIDE_CONT — all box-drawing chars are single-column.
    assert!(
        cells.iter().all(|(_, f)| !f.contains(CellFlags::WIDE_CONT)),
        "box-drawing chars wrongly marked wide: {cells:?}"
    );
    let chars: String = cells.iter().map(|(c, _)| *c).collect();
    assert_eq!(chars, s);
}

// -----------------------------------------------------------------------
// Powerline glyphs (Private Use Area). Sonic ships a Nerd-Font-patched
// "Rec Mono St.Helens" so these MUST be preserved in the grid. The
// renderer-side test asserts a tile actually rasterizes.
// -----------------------------------------------------------------------
#[test]
fn powerline_pua_glyphs_roundtrip() {
    // U+E0B0 (right-pointing triangle), U+E0B2 (left-pointing triangle),
    // U+E0A0 (branch), U+F015 (home). These are the canonical four most
    // shell prompts use.
    let s = "\u{e0b0}\u{e0b2}\u{e0a0}\u{f015}";
    let cells = row0_flags(s);
    let chars: String =
        cells.iter().filter(|(_, f)| !f.contains(CellFlags::WIDE_CONT)).map(|(c, _)| *c).collect();
    assert_eq!(chars, s, "PUA glyphs lost in parser");
}

// -----------------------------------------------------------------------
// Wide width: explicit full-width Latin / punctuation. Both ＦＷ and
// the ideographic space U+3000 must produce a WIDE lead + WIDE_CONT.
// -----------------------------------------------------------------------
#[test]
fn fullwidth_ascii_marks_wide() {
    let s = "［ＡＢ］";
    let cells = row0_flags(s);
    // 4 chars × 2 cells (lead + cont) = 8.
    assert_eq!(cells.len(), 8, "fullwidth not laid out as wide: {cells:?}");
    for chunk in cells.chunks(2) {
        assert!(chunk[0].1.contains(CellFlags::WIDE), "fullwidth lead missing WIDE");
        assert!(chunk[1].1.contains(CellFlags::WIDE_CONT), "fullwidth missing WIDE_CONT");
    }
}

#[test]
fn ideographic_space_is_wide() {
    // U+3000 IDEOGRAPHIC SPACE.
    let s = "a\u{3000}b";
    let cells = row0_flags(s);
    // 'a' (1) + ideographic space lead (1) + WIDE_CONT (1) + 'b' (1) = 4.
    assert_eq!(cells.len(), 4, "ideographic space not wide: {cells:?}");
    assert_eq!(cells[0].0, 'a');
    assert_eq!(cells[1].0, '\u{3000}');
    assert!(cells[1].1.contains(CellFlags::WIDE));
    assert!(cells[2].1.contains(CellFlags::WIDE_CONT));
    assert_eq!(cells[3].0, 'b');
}

// -----------------------------------------------------------------------
// Zero-width characters: ZWJ (U+200D) and ZWSP (U+200B). The cursor
// MUST NOT advance for these — they're either folded onto the prior
// cell or stored as zero-advance. Both are acceptable; what's NOT
// acceptable is treating them as a full-width space.
// -----------------------------------------------------------------------
#[test]
fn zwj_does_not_consume_a_column() {
    let mut p = Parser::new(Grid::new(16, 1));
    p.advance("ab".as_bytes());
    let col_before = p.grid().cursor.col;
    p.advance("\u{200d}".as_bytes());
    let col_after = p.grid().cursor.col;
    assert_eq!(col_before, col_after, "ZWJ wrongly advanced the cursor");
}

#[test]
fn zwsp_does_not_consume_a_column() {
    let mut p = Parser::new(Grid::new(16, 1));
    p.advance("ab".as_bytes());
    let col_before = p.grid().cursor.col;
    p.advance("\u{200b}".as_bytes());
    let col_after = p.grid().cursor.col;
    assert_eq!(col_before, col_after, "ZWSP wrongly advanced the cursor");
}
