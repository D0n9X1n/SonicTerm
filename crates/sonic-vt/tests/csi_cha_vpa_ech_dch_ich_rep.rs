//! Regression tests for the six CSI arms that were silently dropped from
//! `csi_dispatch` until #359: ECH (X), CHA (G), REP (b), VPA (d), DCH (P),
//! ICH (@). Their absence manifested in nvim+neo-tree as stale-suffix
//! smear on held-j after #352 took nvim off the full-repaint fallback.

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
fn csi_ech_erases_n_cells_at_cursor() {
    let mut p = parser(10, 3);
    p.advance(b"ABCDEFGHIJ"); // row 0 full; cursor wraps to (1,0)
    p.advance(b"\x1b[1;3H"); // CUP row 1 col 3 (1-based) -> cursor (0,2) on 'C'
    p.advance(b"\x1b[4X"); // ECH 4 — clears C,D,E,F
    assert_eq!(row_text(&p, 0), "AB    GHIJ");
    assert_eq!(p.grid().cursor.row, 0);
    assert_eq!(p.grid().cursor.col, 2);
}

#[test]
fn csi_cha_moves_cursor_to_column() {
    let mut p = parser(10, 3);
    p.advance(b"\x1b[1;5H"); // CUP row 1 col 5 -> (0,4)
    p.advance(b"\x1b[8G"); // CHA col 8 -> (0,7)
    assert_eq!(p.grid().cursor.row, 0);
    assert_eq!(p.grid().cursor.col, 7);
}

#[test]
fn csi_vpa_moves_cursor_to_row() {
    let mut p = parser(10, 5);
    p.advance(b"\x1b[1;4H"); // (0,3)
    p.advance(b"\x1b[3d"); // VPA row 3 -> (2,3)
    assert_eq!(p.grid().cursor.row, 2);
    assert_eq!(p.grid().cursor.col, 3);
}

#[test]
fn csi_dch_deletes_and_shifts_left() {
    let mut p = parser(10, 3);
    p.advance(b"ABCDEF");
    p.advance(b"\x1b[1;2H"); // CUP -> (0,1) on 'B'
    p.advance(b"\x1b[2P"); // DCH 2: remove BC
                           // Remaining row: A, D, E, F, then 6 blanks (cols 4..=9).
    assert_eq!(row_text(&p, 0), "ADEF      ");
}

#[test]
fn csi_ich_inserts_blanks_shift_right() {
    let mut p = parser(10, 3);
    p.advance(b"ABCDEF");
    p.advance(b"\x1b[1;2H"); // (0,1) on 'B'
    p.advance(b"\x1b[2@"); // ICH 2: insert 2 blanks at col 1
                           // Expected: A, blank, blank, B, C, D, E, F, then trailing blanks.
    assert_eq!(row_text(&p, 0), "A  BCDEF  ");
}

#[test]
fn csi_rep_repeats_last_printed_char() {
    let mut p = parser(10, 3);
    p.advance(b"X");
    p.advance(b"\x1b[4b"); // REP 4 -> four more X
    assert_eq!(row_text(&p, 0), "XXXXX     ");
    assert_eq!(p.grid().cursor.col, 5);
}

#[test]
fn csi_rep_after_cuf_is_noop() {
    let mut p = parser(10, 3);
    p.advance(b"X");
    p.advance(b"\x1b[1C"); // CUF resets last_printed_char per ECMA-48 REP semantics.
    p.advance(b"\x1b[4b");

    assert_eq!(row_text(&p, 0), "X         ");
    assert_eq!(p.grid().cursor.col, 2);
}

#[test]
fn neotree_per_row_ech_tail_clear_no_smear() {
    // Synthetic neo-tree replay: write a long stale row, then jump to
    // mid-row, write a short new label, and rely on ECH to clear the
    // tail. Pre-fix the ECH was a no-op and "OLDOLD" lingered after "NEW".
    let mut p = parser(15, 3);
    p.advance(b"OLDOLDOLDOLDOLD"); // row 0 fully covered
    p.advance(b"\x1b[1;4H"); // CUP row 1 col 4 -> (0,3)
    p.advance(b"NEW"); // overwrite 3 chars
    p.advance(b"\x1b[12X"); // ECH 12 — clear remainder of the row
    assert_eq!(row_text(&p, 0), "OLDNEW         ");
}
