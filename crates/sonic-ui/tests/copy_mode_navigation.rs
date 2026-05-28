use sonic_grid::grid::Grid;
use sonic_ui::copy_mode::{CopyMode, CopyModeState, QuickSelectState};

fn grid_with_text(lines: &[&str]) -> Grid {
    let cols = lines.iter().map(|line| line.chars().count()).max().unwrap_or(1).max(1) as u16;
    let rows = lines.len().max(1) as u16;
    let mut grid = Grid::new(cols, rows);
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            grid.row_mut(row as u16)[col].ch = ch;
        }
    }
    grid
}

#[test]
fn new_at_sets_cursor_mode_and_empty_anchor() {
    let state = CopyModeState::new_at((3, 2));
    assert_eq!(state.cursor, (3, 2));
    assert_eq!(state.mode, CopyMode::Cursor);
    assert_eq!(state.anchor, None);
}

#[test]
fn move_lr_ud_stays_within_bounds() {
    let grid = grid_with_text(&["abc", "def"]);
    let mut state = CopyModeState::new_at((1, 1));
    state.move_left(&grid);
    assert_eq!(state.cursor, (0, 1));
    state.move_left(&grid);
    assert_eq!(state.cursor, (0, 1));
    state.move_right(&grid);
    state.move_right(&grid);
    state.move_right(&grid);
    assert_eq!(state.cursor, (2, 1));
    state.move_up(&grid);
    assert_eq!(state.cursor, (2, 0));
    state.move_up(&grid);
    assert_eq!(state.cursor, (2, 0));
    state.move_down(&grid);
    state.move_down(&grid);
    assert_eq!(state.cursor, (2, 1));
}

#[test]
fn word_forward_and_back_skip_whitespace() {
    let grid = grid_with_text(&["foo  bar baz"]);
    let mut state = CopyModeState::new_at((0, 0));
    state.move_word_fwd(&grid);
    assert_eq!(state.cursor, (5, 0));
    state.move_word_fwd(&grid);
    assert_eq!(state.cursor, (9, 0));
    state.move_word_back(&grid);
    assert_eq!(state.cursor, (5, 0));
}

#[test]
fn line_start_and_end() {
    let grid = grid_with_text(&["abc   "]);
    let mut state = CopyModeState::new_at((1, 0));
    state.move_line_end(&grid);
    assert_eq!(state.cursor, (2, 0));
    state.move_line_start(&grid);
    assert_eq!(state.cursor, (0, 0));
}

#[test]
fn top_and_bottom_move_rows() {
    let grid = grid_with_text(&["aaa", "bbb", "ccc"]);
    let mut state = CopyModeState::new_at((1, 1));
    state.move_top(&grid);
    assert_eq!(state.cursor, (1, 0));
    state.move_bottom(&grid);
    assert_eq!(state.cursor, (1, 2));
}

#[test]
fn start_select_sets_anchor_and_selected_range_is_sorted() {
    let mut state = CopyModeState::new_at((4, 2));
    state.start_select();
    assert_eq!(state.mode, CopyMode::Select);
    assert_eq!(state.anchor, Some((4, 2)));
    state.cursor = (1, 0);
    assert_eq!(state.selected_range(), Some(((1, 0), (4, 2))));
}

fn grid_with_scrollback(history: &[&str], live: &[&str]) -> Grid {
    let cols = history
        .iter()
        .chain(live.iter())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as u16;
    let mut grid = grid_with_text(live);
    grid.resize(cols, live.len().max(1) as u16);
    for line in history {
        for (col, ch) in line.chars().enumerate() {
            grid.row_mut(0)[col].ch = ch;
        }
        grid.scroll_up(1);
    }
    for (row, line) in live.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            grid.row_mut(row as u16)[col].ch = ch;
        }
    }
    grid
}

#[test]
fn copy_mode_scrolls_into_history() {
    let grid = grid_with_scrollback(&["old-one", "old-two"], &["live-one", "live-two"]);
    assert_eq!(grid.scrollback_len(), 2);
    let mut state = CopyModeState::new_at((3, 3));
    state.move_up(&grid);
    assert_eq!(state.cursor, (3, 2));
    state.move_top(&grid);
    assert_eq!(state.cursor, (3, 0));
}

#[test]
fn quick_select_assigns_hints_to_urls() {
    let grid = grid_with_text(&[
        "see https://one.test",
        "open https://two.test",
        "read https://three.test",
    ]);
    let quick = QuickSelectState::from_grid(&grid);
    let hints: Vec<char> = quick.hints.iter().map(|h| h.hint).collect();
    assert_eq!(hints, vec!['a', 'b', 'c']);
}

#[test]
fn quick_select_press_letter_copies() {
    let grid = grid_with_text(&["see https://one.test and https://two.test"]);
    let quick = QuickSelectState::from_grid(&grid);
    assert_eq!(quick.text_for_hint('a'), Some("https://one.test"));
    assert_eq!(quick.text_for_hint('A'), Some("https://one.test"));
    assert_eq!(quick.text_for_hint('z'), None);
}
