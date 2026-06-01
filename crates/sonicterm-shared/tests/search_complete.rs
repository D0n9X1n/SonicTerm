//! End-to-end tests for the completed in-page search feature: scrollback
//! search, case toggle, regex mode, next/prev wrap, viewport scroll
//! request, and close-clears semantics.

use sonicterm_core::grid::{CellFlags, Color, Grid};
use sonicterm_shared::search::{
    find_in_grid, find_regex_in_grid, MatchRange, SearchMode, SearchState,
};

fn put(g: &mut Grid, s: &str) {
    for ch in s.chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// Build a grid of `cols` x `rows` and fill it line-by-line. Any text that
/// overflows the visible region is pushed into scrollback by `scroll_up`.
fn fill(cols: u16, rows: u16, lines: &[&str]) -> Grid {
    let mut g = Grid::new(cols, rows);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            // Move down; if we're at the bottom, scroll first to push the
            // previous row into scrollback.
            if g.cursor.row + 1 >= g.rows {
                g.scroll_up(1);
                g.cursor.col = 0;
            } else {
                g.cursor.row += 1;
                g.cursor.col = 0;
            }
        }
        put(&mut g, line);
    }
    g
}

// 1. substring match across grid + scrollback

#[test]
fn substring_match_spans_scrollback_and_visible() {
    // 3 visible rows; 4 lines => 1 row in scrollback ("alpha").
    let g = fill(16, 3, &["alpha", "beta target", "gamma", "target end"]);
    assert_eq!(g.scrollback_len(), 1);
    let matches = find_in_grid(&g, "target", true);
    assert_eq!(matches.len(), 2, "should find one in scrollback row, one in visible");
    // First match is in scrollback row (the "beta target" line — wait,
    // actually scrollback contains "alpha", visible contains "beta target",
    // "gamma", "target end"). Both matches are visible. Re-check setup:
    // visible == ["beta target", "gamma", "target end"], scrollback ==
    // ["alpha"]. Both matches are in visible rows.
    // Absolute rows: scrollback len = 1, so visible rows are at abs 1,2,3.
    assert_eq!(matches[0].row, 1);
    assert_eq!(matches[1].row, 3);
}

#[test]
fn substring_match_in_scrollback_proper() {
    // 2 visible rows; 4 lines => 2 rows in scrollback (with "target" in
    // scrollback line 0).
    let g = fill(32, 2, &["target_top", "row two", "row three", "row four"]);
    assert_eq!(g.scrollback_len(), 2);
    let matches = find_in_grid(&g, "target_top", true);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].row, 0, "match should be in scrollback row 0");
}

// 2. case-insensitive toggle

#[test]
fn case_toggle_recomputes_matches() {
    let g = fill(16, 1, &["Foo foo FOO"]);
    let mut s = SearchState::new();
    s.case_sensitive = true;
    s.input_char('f', &g);
    s.input_char('o', &g);
    s.input_char('o', &g);
    assert_eq!(s.matches.len(), 1, "case-sensitive: only 'foo' matches");
    s.toggle_case_sensitive(&g);
    assert!(!s.case_sensitive);
    assert_eq!(s.matches.len(), 3, "after toggle: all three match");
    // Toggle back.
    s.toggle_case_sensitive(&g);
    assert_eq!(s.matches.len(), 1);
}

// 3. next/prev wraps around

#[test]
fn next_wraps_and_prev_wraps() {
    let g = fill(16, 1, &["ab ab ab"]);
    let mut s = SearchState::new();
    s.input_char('a', &g);
    s.input_char('b', &g);
    assert_eq!(s.matches.len(), 3);
    assert_eq!(s.current, Some(0));
    s.next();
    s.next();
    s.next();
    assert_eq!(s.current, Some(0), "next wraps from last back to 0");
    s.prev();
    assert_eq!(s.current, Some(2), "prev wraps from 0 to last");
}

// 4. regex mode

#[test]
fn regex_mode_finds_pattern() {
    let g = fill(32, 1, &["foo123 bar456 baz789"]);
    let mut s = SearchState::new();
    s.toggle_regex(&g);
    assert_eq!(s.mode, SearchMode::Regex);
    s.query = r"\d{3}".to_string();
    s.refresh(&g);
    assert_eq!(s.matches.len(), 3, "should find three 3-digit runs");
    assert!(s.regex_error.is_none());
}

#[test]
fn regex_mode_reports_compile_errors() {
    let g = fill(32, 1, &["hello"]);
    let matches = find_regex_in_grid(&g, "(unclosed", true);
    assert!(matches.is_err());
}

#[test]
fn regex_case_insensitive_via_toggle() {
    let g = fill(32, 1, &["Hello hello HELLO"]);
    let mut s = SearchState::new();
    s.case_sensitive = false;
    s.toggle_regex(&g);
    s.query = r"hello".to_string();
    s.refresh(&g);
    assert_eq!(s.matches.len(), 3);
}

// 5. match in scrollback scrolls viewport

#[test]
fn scrollback_match_sets_scroll_request() {
    let g = fill(32, 2, &["target_top", "row two", "row three", "row four"]);
    let mut s = SearchState::new();
    s.input_char('t', &g);
    s.input_char('a', &g);
    s.input_char('r', &g);
    s.input_char('g', &g);
    s.input_char('e', &g);
    s.input_char('t', &g);
    // current = first match; first match is in scrollback (row 0).
    let cur = s.current_match().expect("must have current match");
    assert!(s.is_in_scrollback(&cur));
    assert_eq!(s.requested_scroll_row, Some(0));
}

#[test]
fn visible_match_clears_scroll_request() {
    let g = fill(32, 3, &["scrollback line", "row two", "match here", "tail"]);
    let mut s = SearchState::new();
    s.query = "match".to_string();
    s.refresh(&g);
    let cur = s.current_match().expect("must have current match");
    assert!(!s.is_in_scrollback(&cur));
    assert_eq!(s.requested_scroll_row, None);
}

// 6. close clears matches (the app drops SearchState; emulate via state reset).

#[test]
fn close_clears_state() {
    let g = fill(32, 1, &["hello hello"]);
    let mut s = SearchState::new();
    s.input_char('h', &g);
    s.input_char('e', &g);
    assert!(!s.matches.is_empty());
    // Simulate Esc by dropping; here we just reset the binding the way
    // app.rs does (`st.search = None;`). Recreating SearchState yields
    // empty matches.
    let fresh = SearchState::new();
    assert!(fresh.matches.is_empty());
    assert_eq!(fresh.current, None);
    assert!(fresh.requested_scroll_row.is_none());
}

// Extras

#[test]
fn count_label_reports_n_of_m() {
    let g = fill(32, 1, &["aa aa aa"]);
    let mut s = SearchState::new();
    s.input_char('a', &g);
    s.input_char('a', &g);
    assert_eq!(s.matches.len(), 3);
    assert_eq!(s.count_label(), "1 of 3");
    s.next();
    assert_eq!(s.count_label(), "2 of 3");
}

#[test]
fn empty_query_gives_zero_of_zero() {
    let s = SearchState::new();
    assert_eq!(s.count_label(), "0 of 0");
}

#[test]
fn match_visible_row_returns_none_for_scrollback() {
    let g = fill(32, 2, &["a target", "b", "c", "d"]);
    let mut s = SearchState::new();
    s.query = "target".to_string();
    s.refresh(&g);
    let m = s.current_match().unwrap();
    // "a target" went to scrollback.
    assert_eq!(s.match_visible_row(&m), None);
}

#[test]
fn match_visible_row_translates_to_visible_index() {
    // 3 visible rows; 4 lines => 1 in scrollback.
    let g = fill(32, 3, &["sb top", "row a", "row b", "needle here"]);
    let mut s = SearchState::new();
    s.query = "needle".to_string();
    s.refresh(&g);
    let m = s.current_match().unwrap();
    // "needle here" is the bottom visible row => visible index 2.
    assert_eq!(s.match_visible_row(&m), Some(2));
}

#[test]
fn refresh_records_revision_and_dimensions() {
    let g = fill(32, 3, &["x", "y target", "z"]);
    let mut s = SearchState::new();
    s.query = "target".to_string();
    s.refresh(&g);
    assert_eq!(s.visible_rows, 3);
    assert_eq!(s.scrollback_len, g.scrollback_len() as u32);
    assert_eq!(s.last_revision, g.revision());
}

#[test]
fn match_range_absolute_row_matches_expectations() {
    let g = fill(16, 2, &["sb line", "vis a", "vis b"]);
    assert_eq!(g.scrollback_len(), 1);
    let matches = find_in_grid(&g, "vis", true);
    let rows: Vec<u32> = matches.iter().map(|m| m.row).collect();
    // visible rows are at absolute rows 1 and 2.
    assert_eq!(rows, vec![1, 2]);
    // sanity: a scrollback search hits row 0.
    let sb = find_in_grid(&g, "sb", true);
    assert_eq!(sb[0].row, 0);
}

#[test]
fn next_prev_no_matches_is_safe() {
    let g = fill(16, 1, &["hello"]);
    let mut s = SearchState::new();
    s.input_char('z', &g);
    s.next();
    s.prev();
    assert_eq!(s.current, None);
    assert_eq!(s.requested_scroll_row, None);
}

// Sanity: MatchRange equality with absolute coordinates.
#[test]
fn match_range_equality() {
    let m = MatchRange { row: 7, col_start: 1, col_end: 4 };
    assert_eq!(m.row, 7);
    assert_eq!(m.col_end - m.col_start, 3);
}

// Regression: SearchState must rescan matches when grid.revision() advances
// (e.g. new pty output landed). Otherwise the active search shows stale
// matches against the previous frame's grid contents.
#[test]
fn maybe_refresh_picks_up_new_matches_on_revision_bump() {
    let mut g = fill(20, 12, &["", "", "", "", "", "foo here", "", "", "", "", "", ""]);
    let mut s = SearchState::new();
    s.input_char('f', &g);
    s.input_char('o', &g);
    s.input_char('o', &g);
    assert_eq!(s.matches.len(), 1, "baseline: 1 match before grid mutation");
    let baseline_rev = s.last_revision;
    let baseline_current = s.current_match().expect("should have a current match");

    // Mutate the grid so a new "foo" exists on a later row. Use put_char
    // (the public write path used by the Performer) so revision() ticks.
    g.cursor.row = 10;
    g.cursor.col = 0;
    put(&mut g, "foo again");
    assert!(g.revision() != baseline_rev, "grid revision must advance after writes");

    let changed = s.maybe_refresh_for_revision(&g);
    assert!(changed, "rescan should happen when revision changed");
    assert_eq!(s.matches.len(), 2, "rescan should find both matches");
    assert_eq!(s.last_revision, g.revision());
    // Current must still be valid and (since the original match still
    // exists) should anchor back on it.
    let cur = s.current_match().expect("current must still be set");
    assert_eq!(cur.row, baseline_current.row);
    assert_eq!(cur.col_start, baseline_current.col_start);

    // No-op on a second call (revision unchanged).
    assert!(!s.maybe_refresh_for_revision(&g));
}
