use sonic_grid::grid::Grid;
use sonic_ui::copy_mode::{CopyMode, CopyModeState};

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
