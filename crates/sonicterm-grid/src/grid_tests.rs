use super::*;

fn text(grid: &Grid, row: u16) -> String {
    grid.row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn overwriting_wide_lead_clears_continuation() {
    let mut grid = Grid::new(10, 1);
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());

    grid.goto(0, 0);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(grid.row(0)[0].ch, 'a');
    assert!(!grid.row(0)[0].flags.contains(CellFlags::WIDE));
    assert_eq!(grid.row(0)[1].ch, ' ');
    assert!(!grid.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
}

#[test]
fn overwriting_wide_continuation_clears_lead() {
    let mut grid = Grid::new(10, 1);
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());

    grid.backspace();
    grid.put_char(' ', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(grid.row(0)[0].ch, ' ');
    assert!(!grid.row(0)[0].flags.contains(CellFlags::WIDE));
    assert_eq!(grid.row(0)[1].ch, ' ');
    assert!(!grid.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
}

#[test]
fn erase_cells_splits_wide_char_cleanly() {
    let mut grid = Grid::new(10, 1);
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('文', Color::Default, Color::Default, CellFlags::empty());

    grid.erase_cells_with(0, 1, 1, Cell::default());

    assert_row_has_no_orphan_wide_cells(&grid);
    assert_eq!(grid.row(0)[0].ch, ' ');
    assert_eq!(grid.row(0)[1].ch, ' ');
}

#[test]
fn delete_cells_expands_single_cell_delete_to_full_wide_char() {
    let mut grid = Grid::new(12, 1);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('文', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());

    grid.delete_cells_with(0, 1, 1, Cell::default());

    assert_row_has_no_orphan_wide_cells(&grid);
    assert_eq!(grid.row(0)[0].ch, 'a');
    assert_eq!(grid.row(0)[1].ch, '文');
    assert!(grid.row(0)[1].flags.contains(CellFlags::WIDE));
    assert!(grid.row(0)[2].flags.contains(CellFlags::WIDE_CONT));
    assert_eq!(grid.row(0)[3].ch, 'b');
}

#[test]
fn delete_cells_from_wide_continuation_deletes_full_wide_char() {
    let mut grid = Grid::new(12, 1);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('文', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());

    grid.delete_cells_with(0, 2, 1, Cell::default());

    assert_row_has_no_orphan_wide_cells(&grid);
    assert_eq!(grid.row(0)[0].ch, 'a');
    assert_eq!(grid.row(0)[1].ch, '文');
    assert!(grid.row(0)[1].flags.contains(CellFlags::WIDE));
    assert!(grid.row(0)[2].flags.contains(CellFlags::WIDE_CONT));
    assert_eq!(grid.row(0)[3].ch, 'b');
}

#[test]
fn insert_cells_inside_wide_char_repairs_row() {
    let mut grid = Grid::new(12, 1);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('文', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());

    grid.insert_cells_with(0, 2, 1, Cell::default());

    assert_row_has_no_orphan_wide_cells(&grid);
}

fn assert_row_has_no_orphan_wide_cells(grid: &Grid) {
    let row = grid.row(0);
    for c in 0..grid.cols as usize {
        let flags = row[c].flags;
        if flags.contains(CellFlags::WIDE) {
            assert!(c + 1 < grid.cols as usize, "wide lead at row end");
            assert!(
                row[c + 1].flags.contains(CellFlags::WIDE_CONT),
                "wide lead without continuation at col {c}"
            );
        }
        if flags.contains(CellFlags::WIDE_CONT) {
            assert!(c > 0, "wide continuation at col 0");
            assert!(
                row[c - 1].flags.contains(CellFlags::WIDE),
                "wide continuation without lead at col {c}"
            );
        }
    }
}

#[test]
fn insert_cells_before_preserves_shifted_text() {
    // inserting blank cells must shift the EXISTING text right intact,
    // not blank the source cells before the shift. Pre-fix this dropped the
    // inserted-before text and kept only the originally-rightmost cell
    // ("0.1" + insert 2 at col 0 → "    1   " instead of "  0.1   ").
    let mut grid = Grid::new(8, 1);
    for ch in "0.1".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.insert_cells_with(0, 0, 2, Cell::default());
    let after: String = grid.row(0).iter().map(|c| c.ch).collect();
    assert_eq!(after, "  0.1   ");
}

#[test]
fn insert_cells_mid_line_preserves_both_sides() {
    // Insert in the middle: "abcd" + insert 1 at col 2 → "ab cd ".
    let mut grid = Grid::new(6, 1);
    for ch in "abcd".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.insert_cells_with(0, 2, 1, Cell::default());
    let after: String = grid.row(0).iter().map(|c| c.ch).collect();
    assert_eq!(after, "ab cd ");
}

#[test]
fn insert_cells_splitting_wide_pair_repairs_orphans() {
    // A wide glyph straddling the insertion point must not leave a dangling
    // lead/continuation. "中x" (中 occupies cols 0-1) + insert 1 at col 1
    // splits the pair; repair_wide_row blanks the orphaned lead and the
    // narrow 'x' (originally col 2) survives the shift.
    let mut grid = Grid::new(6, 1);
    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    grid.insert_cells_with(0, 1, 1, Cell::default());
    let after: String = grid.row(0).iter().map(|c| c.ch).collect();
    assert!(after.contains('x'), "narrow cell after the split must survive: {after:?}");
    // No dangling wide half: every WIDE_CONT must have a WIDE lead to its left.
    for c in 0..6 {
        if grid.row(0)[c].flags.contains(CellFlags::WIDE_CONT) {
            assert!(
                c > 0 && grid.row(0)[c - 1].flags.contains(CellFlags::WIDE),
                "orphaned wide continuation at col {c}: {after:?}"
            );
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

fn dirty_rows_vec(grid: &Grid) -> Vec<usize> {
    grid.dirty_rows().collect()
}

#[test]
fn resize_grow_dirties_every_row_including_new_ones() {
    let mut grid = Grid::new(8, 3);
    grid.clear_dirty();

    grid.resize(10, 5);

    assert_eq!(dirty_rows_vec(&grid), vec![0, 1, 2, 3, 4]);
}

#[test]
fn resize_shrink_dirties_every_remaining_row() {
    let mut grid = Grid::new(8, 5);
    grid.clear_dirty();

    grid.resize(4, 2);

    assert_eq!(dirty_rows_vec(&grid), vec![0, 1]);
}

#[test]
fn bounded_grid_size_caps_axes_and_total_cells() {
    let (cols, rows) = bounded_grid_size(u64::MAX, u64::MAX);

    assert!(cols <= MAX_GRID_AXIS);
    assert!(rows <= MAX_GRID_AXIS);
    assert!(u32::from(cols) * u32::from(rows) <= MAX_GRID_CELLS);
}

#[test]
fn grid_new_and_resize_apply_memory_bounds() {
    let mut grid = Grid::new(u16::MAX, u16::MAX);
    assert!(u32::from(grid.cols) * u32::from(grid.rows) <= MAX_GRID_CELLS);

    grid.resize(u16::MAX, u16::MAX);
    assert!(u32::from(grid.cols) * u32::from(grid.rows) <= MAX_GRID_CELLS);
}

#[test]
fn reshape_releases_row_capacity_above_visible_cell_budget() {
    let mut grid = Grid::new(4096, 128);

    grid.resize(128, 4096);

    let retained_bytes = grid.rows_iter().map(|row| row.approx_capacity_byte_size()).sum::<usize>();
    let visible_budget_bytes = MAX_VISIBLE_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        retained_bytes <= visible_budget_bytes,
        "visible rows retain {retained_bytes} bytes above the {visible_budget_bytes}-byte budget"
    );
}

#[test]
fn exact_half_reshape_stays_within_visible_capacity_budget() {
    let mut grid = Grid::new(4096, 128);

    grid.resize(2048, 256);

    let retained_bytes = grid.rows_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    let visible_budget_bytes = MAX_VISIBLE_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        retained_bytes <= visible_budget_bytes,
        "exact-half reshape retains {retained_bytes} bytes above {visible_budget_bytes}"
    );
}

#[test]
fn narrow_exact_half_reshape_stays_within_visible_capacity_budget() {
    let mut grid = Grid::new(1024, 512);

    grid.resize(512, 1024);

    let retained_bytes = grid.rows_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    let visible_budget_bytes = MAX_VISIBLE_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        retained_bytes <= visible_budget_bytes,
        "narrow exact-half reshape retains {retained_bytes} bytes above {visible_budget_bytes}"
    );
}

#[test]
fn adjacent_column_resize_preserves_populated_history_capacity() {
    let mut grid = Grid::new(100, 24);
    grid.scrollback_requested_limit = 256;
    grid.scrollback_limit = 256;
    for index in 0..256 {
        let mut cells = vec![Cell::default(); 100];
        cells[0].ch = char::from(b'a' + (index % 26) as u8);
        grid.scrollback.push_back(Line::from_flat(cells));
    }
    let retained_before =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();

    grid.resize(99, 24);
    let retained_after_shrink =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    grid.resize(100, 24);
    let retained_after_restore =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();

    assert_eq!(retained_after_shrink, retained_before);
    assert_eq!(retained_after_restore, retained_before);
}

#[test]
fn grow_first_adjacent_resize_reuses_populated_history_capacity() {
    let mut grid = Grid::new(100, 24);
    grid.scrollback_requested_limit = 256;
    grid.scrollback_limit = 256;
    for index in 0..256 {
        let mut cells = vec![Cell::default(); 100];
        cells[0].ch = char::from(b'a' + (index % 26) as u8);
        grid.scrollback.push_back(Line::from_flat(cells));
    }

    grid.resize(101, 24);
    let retained_after_grow =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    grid.resize(100, 24);
    let retained_after_restore =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    grid.resize(101, 24);
    let retained_after_second_grow =
        grid.scrollback_iter().map(Line::approx_capacity_byte_size).sum::<usize>();

    assert_eq!(retained_after_restore, retained_after_grow);
    assert_eq!(retained_after_second_grow, retained_after_grow);
}

#[test]
fn scrollback_limit_releases_outer_high_water_capacity() {
    let mut grid = Grid::new(1, 1);
    grid.scrollback_requested_limit = 4096;
    grid.scrollback_limit = 4096;
    grid.scrollback = VecDeque::with_capacity(4096);
    for _ in 0..4096 {
        grid.scrollback.push_back(Line::flat_filled(1, Cell::default()));
    }

    grid.set_scrollback_limit(8);

    assert_eq!(grid.scrollback_len(), 8);
    assert!(
        grid.scrollback_capacity() <= grid.scrollback_len().saturating_mul(2),
        "scrollback len {} retained outer capacity {}",
        grid.scrollback_len(),
        grid.scrollback_capacity()
    );
}

#[test]
fn scrollback_limit_shares_the_total_grid_cell_budget() {
    assert_eq!(
        bounded_scrollback_rows(
            MAX_GRID_AXIS,
            (MAX_GRID_CELLS / u32::from(MAX_GRID_AXIS)) as u16,
            usize::MAX,
        ),
        0
    );

    let mut grid = Grid::new(100, 24);
    grid.set_scrollback_limit(usize::MAX);
    assert!(
        u64::from(grid.cols)
            * (u64::from(grid.rows) + grid.scrollback_limit as u64)
            <= u64::from(MAX_GRID_CELLS)
    );
}

#[test]
fn scrollback_limit_update_trims_saved_primary_while_alt_is_active() {
    let mut grid = Grid::new(4, 2);
    for ch in "abcdefgh".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.goto(1, 0);
    grid.linefeed();
    assert_eq!(grid.scrollback_len(), 1);

    grid.enter_alt_screen();
    grid.set_scrollback_limit(0);
    grid.leave_alt_screen();

    assert_eq!(grid.scrollback_len(), 0);
}

#[test]
fn alternate_screen_scrolling_does_not_retain_second_scrollback_budget() {
    let mut grid = Grid::new(4, 2);
    grid.enter_alt_screen();
    for _ in 0..100 {
        grid.scroll_up(1);
    }

    assert_eq!(grid.scrollback_len(), 0);
}

#[test]
fn primary_and_alternate_storage_share_one_cell_budget() {
    let mut grid = Grid::new(u16::MAX, u16::MAX);
    grid.set_scrollback_limit(usize::MAX);
    grid.enter_alt_screen();
    let primary = grid.alt_screen.as_ref().expect("saved primary");
    let retained_cells = u64::from(grid.cols) * u64::from(grid.rows)
        + u64::from(primary.cols) * u64::from(primary.rows)
        + u64::from(primary.cols) * primary.scrollback_limit as u64;

    assert!(retained_cells <= u64::from(MAX_GRID_CELLS));
}

#[test]
fn wide_char_put_dirties_only_its_row() {
    let mut grid = Grid::new(8, 3);
    grid.goto(1, 0);
    grid.clear_dirty();

    grid.put_char('中', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(dirty_rows_vec(&grid), vec![1]);
    assert!(grid.row(1)[0].flags.contains(CellFlags::WIDE));
    assert!(grid.row(1)[1].flags.contains(CellFlags::WIDE_CONT));
}

#[test]
fn zero_width_cluster_bytes_are_bounded_per_cell() {
    let mut grid = Grid::new(8, 1);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    for _ in 0..1000 {
        grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
    }

    assert!(grid.row(0)[0].extras().expect("combining marks retained").len()
        <= MAX_CELL_EXTRAS_BYTES);
}
