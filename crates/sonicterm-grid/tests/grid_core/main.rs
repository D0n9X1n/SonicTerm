use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid};

fn text(grid: &Grid, row: u16) -> String {
    grid.row(row).iter().map(|cell| cell.ch).collect()
}

fn assert_row_has_no_orphan_wide_cells(grid: &Grid) {
    let row = grid.row(0);
    for c in 0..grid.cols as usize {
        let flags = row[c].flags;
        if flags.contains(CellFlags::WIDE) {
            assert!(c + 1 < grid.cols as usize);
            assert!(row[c + 1].flags.contains(CellFlags::WIDE_CONT));
        }
        if flags.contains(CellFlags::WIDE_CONT) {
            assert!(c > 0);
            assert!(row[c - 1].flags.contains(CellFlags::WIDE));
        }
    }
}

#[test]
fn scrollback_limit_zero_recycles_rows_without_history() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(0);
    for ch in "abcd".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.linefeed();
    for ch in "efgh".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.linefeed();

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(text(&grid, 0), "efgh");
    assert_eq!(text(&grid, 1), "    ");
}

#[test]
fn insert_delete_and_erase_cells_preserve_row_width() {
    let mut grid = Grid::new(6, 1);
    for ch in "abcdef".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }

    grid.insert_cells(0, 2, 2);
    assert_eq!(grid.row(0).len(), 6);
    grid.delete_cells(0, 1, 3);
    assert_eq!(grid.row(0).len(), 6);
    grid.erase_cells(0, 1, 2);
    assert_eq!(grid.row(0).len(), 6);
    assert_row_has_no_orphan_wide_cells(&grid);
}

#[test]
fn region_scroll_does_not_touch_scrollback_for_partial_region() {
    let mut grid = Grid::new(4, 4);
    for (row, label) in ["1111", "2222", "3333", "4444"].into_iter().enumerate() {
        grid.goto(row as u16, 0);
        for ch in label.chars() {
            grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }

    grid.scroll_region_up(1, 2, 1);

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(text(&grid, 0), "1111");
    assert_eq!(text(&grid, 1), "3333");
    assert_eq!(text(&grid, 2), "    ");
    assert_eq!(text(&grid, 3), "4444");
}

#[test]
fn full_region_scroll_routes_to_scrollback() {
    let mut grid = Grid::new(4, 2);
    for ch in "abcd".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.goto(1, 0);
    for ch in "efgh".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }

    grid.scroll_region_up(0, 1, 1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(grid.scrollback_row(0).unwrap()[0].ch, 'a');
    assert_eq!(text(&grid, 0), "efgh");
}

#[test]
fn rare_attrs_survive_wide_cell_fill_cleanup() {
    let mut grid = Grid::new(4, 1);
    grid.put_char('中', Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6), CellFlags::UNDERLINE);
    grid.goto(0, 1);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(text(&grid, 0), " x  ");
    assert!(!grid.row(0)[0].flags.contains(CellFlags::WIDE));
    assert!(!grid.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
}

#[test]
fn prompt_markers_track_scrollback_absolute_rows() {
    let mut grid = Grid::new(4, 2);
    grid.record_prompt_start();
    grid.linefeed();
    grid.record_prompt_end(Some(0));
    grid.linefeed();

    let prompts: Vec<_> = grid.prompts().collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].start_row, 0);
    assert_eq!(prompts[0].end_row, Some(1));
    assert_eq!(grid.prompt_visible_row(prompts[0]), None);
}

#[test]
fn autowrap_off_overwrites_right_edge() {
    let mut grid = Grid::new(4, 1);
    grid.set_autowrap(false);
    for ch in "abcdef".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }

    assert_eq!(text(&grid, 0), "abcf");
    assert_eq!(grid.cursor.col, 3);
    assert!(!grid.pending_wrap());
}

#[test]
fn pending_wrap_is_cleared_by_cursor_motion() {
    let mut grid = Grid::new(4, 2);
    for ch in "abcd".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    assert!(grid.pending_wrap());

    grid.goto(0, 1);
    grid.put_char('Z', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(text(&grid, 0), "aZcd");
    assert_eq!(text(&grid, 1), "    ");
}

#[test]
fn custom_fill_is_used_for_scroll_rows() {
    let mut grid = Grid::new(3, 1);
    let fill = Cell::plain('.', Color::Indexed(1), Color::Indexed(2), CellFlags::empty());

    grid.scroll_up_with(1, fill.clone());

    assert_eq!(text(&grid, 0), "...");
    assert_eq!(grid.row(0)[0].fg, fill.fg);
    assert_eq!(grid.row(0)[0].bg, fill.bg);
}
