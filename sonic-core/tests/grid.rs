use sonic_core::grid::*;

#[test]
fn put_char_advances_cursor() {
    let mut g = Grid::new(10, 3);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.put_char('B', Color::Default, Color::Default, CellFlags::empty());
    assert_eq!(g.cursor, Pos { row: 0, col: 2 });
    assert_eq!(g.row(0)[0].ch, 'A');
    assert_eq!(g.row(0)[1].ch, 'B');
}

#[test]
fn linefeed_scrolls_when_at_bottom() {
    let mut g = Grid::new(4, 2);
    g.cursor = Pos { row: 1, col: 0 };
    g.put_char('X', Color::Default, Color::Default, CellFlags::empty());
    g.linefeed();
    assert_eq!(g.cursor.row, 1);
    assert_eq!(g.scrollback_len(), 1);
}

#[test]
fn wide_char_occupies_two_cells() {
    let mut g = Grid::new(4, 1);
    g.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    assert!(g.row(0)[0].flags.contains(CellFlags::WIDE));
    assert!(g.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
    assert_eq!(g.cursor.col, 2);
}

#[test]
fn erase_screen_clears_all_cells() {
    let mut g = Grid::new(2, 2);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.erase_screen();
    assert_eq!(g.row(0)[0].ch, ' ');
}

#[test]
fn resize_grows_and_shrinks() {
    let mut g = Grid::new(5, 3);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.resize(8, 5);
    assert_eq!(g.cols, 8);
    assert_eq!(g.rows, 5);
    assert_eq!(g.row(0)[0].ch, 'A');
    g.resize(3, 2);
    assert_eq!(g.cols, 3);
    assert_eq!(g.rows, 2);
}

#[test]
fn tab_aligns_to_eight() {
    let mut g = Grid::new(40, 1);
    g.cursor.col = 3;
    g.tab();
    assert_eq!(g.cursor.col, 8);
    g.tab();
    assert_eq!(g.cursor.col, 16);
}

#[test]
fn erase_line_to_end_only_clears_from_cursor() {
    let mut g = Grid::new(5, 1);
    for ch in "abcde".chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    g.cursor.col = 2;
    g.erase_line_to_end();
    assert_eq!(g.row(0)[0].ch, 'a');
    assert_eq!(g.row(0)[1].ch, 'b');
    assert_eq!(g.row(0)[2].ch, ' ');
    assert_eq!(g.row(0)[4].ch, ' ');
}

#[test]
fn cr_does_not_change_row() {
    let mut g = Grid::new(5, 2);
    g.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    g.put_char('b', Color::Default, Color::Default, CellFlags::empty());
    g.carriage_return();
    assert_eq!(g.cursor, Pos { row: 0, col: 0 });
}

#[test]
fn backspace_clamps_to_zero() {
    let mut g = Grid::new(3, 1);
    g.backspace();
    assert_eq!(g.cursor.col, 0);
}

#[test]
fn auto_wrap_at_end_of_row() {
    let mut g = Grid::new(3, 2);
    for ch in "abcd".chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    // 'a','b','c' on row 0; 'd' should wrap to row 1
    assert_eq!(g.row(0)[2].ch, 'c');
    assert_eq!(g.row(1)[0].ch, 'd');
}

#[test]
fn scrollback_caps_at_limit() {
    let mut g = Grid::new(2, 1);
    g.set_scrollback_limit(3);
    for _ in 0..10 {
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), 3);
}

#[test]
fn goto_clamps_out_of_bounds() {
    let mut g = Grid::new(5, 3);
    g.goto(100, 100);
    assert_eq!(g.cursor, Pos { row: 2, col: 4 });
}

#[test]
fn cell_default_hyperlink_is_none() {
    let c = Cell::default();
    assert!(c.hyperlink.is_none());
}

#[test]
fn enter_alt_screen_blanks_visible_and_saves_primary() {
    let mut g = Grid::new(4, 2);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    assert!(!g.is_alt());
    g.enter_alt_screen();
    assert!(g.is_alt());
    assert_eq!(g.row(0)[0].ch, ' ');
    assert_eq!(g.cursor, Pos::default());
}

#[test]
fn leave_alt_screen_restores_primary() {
    let mut g = Grid::new(4, 2);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    let saved_cursor = g.cursor;
    g.enter_alt_screen();
    g.put_char('Z', Color::Default, Color::Default, CellFlags::empty());
    g.leave_alt_screen();
    assert!(!g.is_alt());
    assert_eq!(g.row(0)[0].ch, 'A');
    assert_eq!(g.cursor, saved_cursor);
}

#[test]
fn enter_alt_twice_is_noop() {
    let mut g = Grid::new(3, 2);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.enter_alt_screen();
    g.put_char('B', Color::Default, Color::Default, CellFlags::empty());
    g.enter_alt_screen();
    assert_eq!(g.row(0)[0].ch, 'B');
    g.leave_alt_screen();
    assert_eq!(g.row(0)[0].ch, 'A');
}

#[test]
fn alt_screen_survives_resize() {
    let mut g = Grid::new(4, 2);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.enter_alt_screen();
    g.resize(6, 3);
    g.leave_alt_screen();
    assert_eq!(g.cols, 6);
    assert_eq!(g.rows, 3);
    assert_eq!(g.row(0)[0].ch, 'A');
}

// -- Revision counter (Epic B1) -----------------------------------------

#[test]
fn revision_fresh_grid_is_zero() {
    let g = Grid::new(8, 4);
    assert_eq!(g.revision(), 0);
}

#[test]
fn revision_increments_after_put_char() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.put_char('X', Color::Default, Color::Default, CellFlags::empty());
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_linefeed() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.linefeed();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_carriage_return() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.carriage_return();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_backspace() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.backspace();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_tab() {
    let mut g = Grid::new(16, 4);
    let before = g.revision();
    g.tab();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_goto() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.goto(1, 1);
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_scroll_up() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.scroll_up(1);
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_screen() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.erase_screen();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_line() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.erase_line();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_line_to_end() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.erase_line_to_end();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_line_to_start() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.erase_line_to_start();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_below() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.erase_below();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_erase_above() {
    let mut g = Grid::new(8, 4);
    g.goto(2, 0);
    let before = g.revision();
    g.erase_above();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_resize() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.resize(10, 5);
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_enter_alt_screen() {
    let mut g = Grid::new(8, 4);
    let before = g.revision();
    g.enter_alt_screen();
    assert!(g.revision() > before);
}

#[test]
fn revision_increments_after_leave_alt_screen() {
    let mut g = Grid::new(8, 4);
    g.enter_alt_screen();
    let before = g.revision();
    g.leave_alt_screen();
    assert!(g.revision() > before);
}

#[test]
fn revision_not_changed_by_read_only_ops() {
    let mut g = Grid::new(8, 4);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    let before = g.revision();
    let _ = g.row(0);
    let _ = g.rows_iter().count();
    let _ = g.scrollback_len();
    let _ = g.revision();
    assert_eq!(g.revision(), before);
}

#[test]
fn revision_survives_resize() {
    let mut g = Grid::new(8, 4);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    let mid = g.revision();
    assert!(mid > 0);
    g.resize(10, 5);
    // resize bumps (doesn't reset)
    assert!(g.revision() > mid);
    assert_ne!(g.revision(), 0);
}
