
use super::*;

fn state_with_matches() -> SearchState {
    SearchState {
        matches: vec![
            MatchRange { row: 10, col_start: 2, col_end: 5 },
            MatchRange { row: 20, col_start: 8, col_end: 9 },
            MatchRange { row: 30, col_start: 1, col_end: 3 },
        ],
        ..SearchState::new()
    }
}

#[test]
fn first_enter_selects_nearest_match_to_cursor() {
    let mut s = state_with_matches();
    s.select_nearest(19, 0);
    assert_eq!(s.current, Some(1));
    assert_eq!(s.requested_scroll_row, Some(20));
}

#[test]
fn arrow_direction_selects_relative_to_cursor_when_unselected() {
    let mut down = state_with_matches();
    down.next_from(20, 8);
    assert_eq!(down.current, Some(2));

    let mut up = state_with_matches();
    up.prev_from(20, 8);
    assert_eq!(up.current, Some(0));
}

#[test]
fn search_ignores_newline_input() {
    let grid = Grid::new(10, 2);
    let mut s = SearchState::new();
    s.input_char('a', &grid);
    s.input_char('\n', &grid);
    s.input_char('\r', &grid);
    s.input_char('b', &grid);
    assert_eq!(s.query, "ab");
}

#[test]
fn search_accepts_ime_commit_text_as_single_line() {
    let grid = Grid::new(10, 2);
    let mut s = SearchState::new();
    s.input_str("你\r\n好\n世界", &grid);
    assert_eq!(s.query, "你好世界");
}

#[test]
fn visible_match_range_bounds_to_viewport() {
    // Rows 10, 20, 30 (matches the shared fixture's ordering).
    let s = state_with_matches();
    // Viewport [15, 25) -> only the row-20 match (index 1).
    assert_eq!(s.visible_match_range(15, 10), (1, 2));
    // Viewport [0, 10) -> nothing (row 10 is excluded by the half-open top).
    assert_eq!(s.visible_match_range(0, 10), (0, 0));
    // Viewport covering everything.
    assert_eq!(s.visible_match_range(0, 100), (0, 3));
    // Viewport above all matches.
    assert_eq!(s.visible_match_range(40, 10), (3, 3));
}

#[test]
fn visible_match_range_includes_all_matches_on_a_boundary_row() {
    // Multiple matches on the same row must all fall inside the window —
    // equal-row runs are contiguous because matches are row-sorted.
    let s = SearchState {
        matches: vec![
            MatchRange { row: 5, col_start: 0, col_end: 1 },
            MatchRange { row: 5, col_start: 4, col_end: 6 },
            MatchRange { row: 5, col_start: 9, col_end: 11 },
            MatchRange { row: 99, col_start: 0, col_end: 2 },
        ],
        ..SearchState::new()
    };
    // Viewport [5, 6) captures all three row-5 matches, not the row-99 one.
    assert_eq!(s.visible_match_range(5, 1), (0, 3));
}
