//! Integration tests for sonicterm-shared search.

use sonicterm_grid::grid::{CellFlags, Color, Grid};
use sonicterm_ui::search::*;

fn put(g: &mut Grid, s: &str) {
    for ch in s.chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
}

fn grid_with(text: &str, cols: u16) -> Grid {
    let rows = (text.lines().count() as u16).max(1);
    let mut g = Grid::new(cols, rows);
    let mut first = true;
    for line in text.lines() {
        if !first {
            let cur_row = g.cursor.row + 1;
            g.cursor.row = cur_row;
            g.cursor.col = 0;
        }
        first = false;
        put(&mut g, line);
    }
    g
}

#[test]
fn find_empty_query_returns_empty() {
    let g = grid_with("hello world", 32);
    assert!(find_in_grid(&g, "", false).is_empty());
    assert!(find_in_grid(&g, "", true).is_empty());
}

#[test]
fn find_single_line_one_match() {
    let g = grid_with("the quick brown fox", 32);
    let m = find_in_grid(&g, "quick", false);
    assert_eq!(m, vec![MatchRange { row: 0, col_start: 4, col_end: 9 }]);
}

#[test]
fn find_multiple_matches_per_row() {
    let g = grid_with("ababab", 8);
    let m = find_in_grid(&g, "ab", true);
    assert_eq!(
        m,
        vec![
            MatchRange { row: 0, col_start: 0, col_end: 2 },
            MatchRange { row: 0, col_start: 2, col_end: 4 },
            MatchRange { row: 0, col_start: 4, col_end: 6 },
        ]
    );
}

#[test]
fn find_case_sensitive_toggle() {
    let g = grid_with("Foo foo FOO", 16);
    assert_eq!(find_in_grid(&g, "foo", true).len(), 1);
    assert_eq!(find_in_grid(&g, "foo", false).len(), 3);
}

#[test]
fn find_matches_do_not_overlap() {
    let g = grid_with("aaaa", 8);
    let m = find_in_grid(&g, "aa", true);
    assert_eq!(
        m,
        vec![
            MatchRange { row: 0, col_start: 0, col_end: 2 },
            MatchRange { row: 0, col_start: 2, col_end: 4 },
        ]
    );
}

#[test]
fn find_skips_wide_cont_cells() {
    let mut g = Grid::new(8, 1);
    g.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    assert!(g.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
    let m = find_in_grid(&g, "中A", true);
    assert_eq!(m, vec![MatchRange { row: 0, col_start: 0, col_end: 3 }]);
}

#[test]
fn search_state_next_wraps() {
    let g = grid_with("ab ab ab", 16);
    let mut s = SearchState::new();
    s.input_char('a', &g);
    s.input_char('b', &g);
    assert_eq!(s.matches.len(), 3);
    assert_eq!(s.current, Some(0));
    s.next();
    assert_eq!(s.current, Some(1));
    s.next();
    assert_eq!(s.current, Some(2));
    s.next();
    assert_eq!(s.current, Some(0), "next should wrap from last to first");
}

#[test]
fn search_state_prev_wraps() {
    let g = grid_with("ab ab ab", 16);
    let mut s = SearchState::new();
    s.input_char('a', &g);
    s.input_char('b', &g);
    assert_eq!(s.current, Some(0));
    s.prev();
    assert_eq!(s.current, Some(2), "prev should wrap from first to last");
    s.prev();
    assert_eq!(s.current, Some(1));
}

#[test]
fn search_state_empty_matches_clears_current() {
    let g = grid_with("hello", 16);
    let mut s = SearchState::new();
    s.input_char('z', &g);
    assert!(s.matches.is_empty());
    assert_eq!(s.current, None);
    s.next();
    s.prev();
    assert_eq!(s.current, None);
}

#[test]
fn backspace_recomputes() {
    let g = grid_with("hello", 16);
    let mut s = SearchState::new();
    s.input_char('h', &g);
    s.input_char('z', &g);
    assert!(s.matches.is_empty());
    s.backspace(&g);
    assert_eq!(s.matches.len(), 1);
}

// --- Haiku review fixes ---

#[test]
fn case_insensitive_handles_multichar_fold() {
    // Turkish dotted-İ lowercases to "i\u{0307}" (two chars). A
    // case-insensitive search for "i\u{0307}" should match a cell
    // containing İ.
    let g = grid_with("aİb", 16);
    let m = find_in_grid(&g, "i\u{0307}", false);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].col_start, 1);
    assert_eq!(m[0].col_end, 2);
}

#[test]
fn wide_cell_match_extends_col_end_over_continuation() {
    // A wide CJK char occupies two cells (WIDE + WIDE_CONT). A match
    // ending on it must report col_end past the continuation cell so
    // the highlight covers the full glyph.
    let g = grid_with("中文", 16);
    let m = find_in_grid(&g, "中", true);
    assert_eq!(m.len(), 1);
    // wide '中' is at col 0; WIDE_CONT at col 1; col_end should be 2.
    assert_eq!(m[0].col_start, 0);
    assert_eq!(m[0].col_end, 2);
}
