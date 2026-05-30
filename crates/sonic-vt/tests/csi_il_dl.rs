//! Regression tests for CSI L (IL — Insert Line) and CSI M (DL — Delete Line).
//!
//! Pre-fix, sonic-vt `csi_dispatch` had no arms for these sequences, dropping
//! them into the wildcard `_ => {}` and silently discarding them. nvim under
//! key-repeat batches multi-row body shifts as CUP + CSI nM + CUP + CSI nL +
//! print; with IL/DL no-op'd, no grid mutation happened, no rows were marked
//! dirty, and the line/glyph caches replayed the previous frame above the
//! freshly-printed bottom line — leaving "stale rows" smear in nvim during
//! held j. This file pins down ECMA-48 region semantics for both.

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
fn csi_dl_deletes_line_at_cursor_within_decstbm_region() {
    let mut p = parser(10, 5);
    p.advance(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC\r\nDDDDDDDDDD\r\nEEEEEEEEEE");
    p.advance(b"\x1b[2;4r"); // DECSTBM rows 1..3 (0-based, inclusive)
    p.advance(b"\x1b[2;1H"); // CUP row 2 col 1 -> 0-based (1,0)
    p.advance(b"\x1b[1M"); // DL 1

    assert_eq!(row_text(&p, 0), "AAAAAAAAAA");
    assert_eq!(row_text(&p, 1), "CCCCCCCCCC");
    assert_eq!(row_text(&p, 2), "DDDDDDDDDD");
    assert_eq!(row_text(&p, 3), "          ");
    assert_eq!(row_text(&p, 4), "EEEEEEEEEE");
    let g = p.grid();
    assert!(g.is_row_dirty(1) && g.is_row_dirty(2) && g.is_row_dirty(3));
}

#[test]
fn csi_il_inserts_blank_line_at_cursor_within_decstbm_region() {
    let mut p = parser(10, 5);
    p.advance(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC\r\nDDDDDDDDDD\r\nEEEEEEEEEE");
    p.advance(b"\x1b[2;4r"); // DECSTBM rows 1..3
    p.advance(b"\x1b[2;1H"); // CUP row 2 col 1
    p.advance(b"\x1b[1L"); // IL 1

    assert_eq!(row_text(&p, 0), "AAAAAAAAAA");
    assert_eq!(row_text(&p, 1), "          ");
    assert_eq!(row_text(&p, 2), "BBBBBBBBBB");
    assert_eq!(row_text(&p, 3), "CCCCCCCCCC");
    assert_eq!(row_text(&p, 4), "EEEEEEEEEE");
    let g = p.grid();
    assert!(g.is_row_dirty(1) && g.is_row_dirty(2) && g.is_row_dirty(3));
}

#[test]
fn nvim_hold_j_batched_scroll_renders_no_stale_rows() {
    // Synthetic replay of the nvim hold-j byte pattern: CUP top of body,
    // CSI 1 M, CUP bot of body, CSI 1 L, print "NEW". This is exactly the
    // sequence that nvim emits in batched form when key-repeat fires faster
    // than its redraw tick. Pre-fix, both M and L were no-ops, so "NEW"
    // ended up wherever the cursor happened to be and the body rows never
    // shifted. Post-fix, the body shifts up one and the new line is filled.
    let mut p = parser(10, 5);
    p.advance(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC\r\nDDDDDDDDDD\r\nEEEEEEEEEE");
    p.advance(b"\x1b[2;4r"); // DECSTBM body rows 1..3
    p.advance(b"\x1b[2;1H"); // CUP top of body
    p.advance(b"\x1b[1M"); // DL 1 -> body rows shift up
    p.advance(b"\x1b[4;1H"); // CUP bottom of body (inside DECSTBM)
    p.advance(b"\x1b[1L"); // IL 1 -> blank line inserted at body bottom
    p.advance(b"NEW");

    assert_eq!(row_text(&p, 0), "AAAAAAAAAA");
    assert_eq!(row_text(&p, 1), "CCCCCCCCCC");
    assert_eq!(row_text(&p, 2), "DDDDDDDDDD");
    // IL creates the blank body-bottom line that the new nvim row then fills.
    assert_eq!(row_text(&p, 3), "NEW       ");
    // Row 4 is outside the scroll region and must remain untouched.
    assert_eq!(row_text(&p, 4), "EEEEEEEEEE");
}

#[test]
fn csi_dl_outside_scroll_region_is_noop() {
    let mut p = parser(10, 5);
    p.advance(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC\r\nDDDDDDDDDD\r\nEEEEEEEEEE");
    p.advance(b"\x1b[2;4r"); // DECSTBM rows 1..3
    p.advance(b"\x1b[5;1H"); // CUP outside region (row 4)
    p.advance(b"\x1b[1M"); // DL — must be a no-op per ECMA-48

    assert_eq!(row_text(&p, 4), "EEEEEEEEEE");
}

#[test]
fn csi_2026_sync_output_is_accepted_silently() {
    // The same-PR bonus: ?2026 h/l (synchronized output / BSU/ESU) must not
    // crash, warn, or do anything visible — just consume the sequence so the
    // bytes around it land cleanly instead of being treated as printable text.
    let mut p = parser(10, 2);
    p.advance(b"\x1b[?2026hHELLO\x1b[?2026l");
    assert_eq!(&row_text(&p, 0)[..5], "HELLO");
}
