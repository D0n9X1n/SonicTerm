
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
