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

fn content_rows_since(grid: &Grid, seq: u64) -> Vec<usize> {
    grid.visible_rows_changed_since(seq).collect()
}

#[test]
fn content_sequence_excludes_cursor_and_presentation_only_dirtiness() {
    let mut grid = Grid::new(8, 3);
    let before = grid.content_seq();

    grid.goto(1, 2);
    grid.carriage_return();
    grid.tab();
    grid.backspace();
    grid.mark_all_dirty();

    assert_eq!(grid.content_seq(), before);
    assert!(content_rows_since(&grid, before).is_empty());
}

#[test]
fn content_sequence_marks_only_the_row_whose_cells_changed() {
    let mut grid = Grid::new(8, 3);
    grid.goto(1, 0);
    let before = grid.content_seq();

    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());

    assert!(grid.content_seq() > before);
    assert_eq!(content_rows_since(&grid, before), vec![1]);
}

#[test]
fn primary_scroll_keeps_existing_rows_at_their_absolute_content_identity() {
    let mut grid = Grid::new(4, 3);
    grid.goto(0, 0);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(1, 0);
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(2, 0);
    grid.put_char('c', Color::Default, Color::Default, CellFlags::empty());
    let before = grid.content_seq();

    grid.scroll_up(1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(text(&grid, 0).chars().next(), Some('b'));
    assert_eq!(content_rows_since(&grid, before), vec![2]);
}

#[test]
fn alternate_screen_scroll_marks_every_fixed_screen_position_changed() {
    let mut grid = Grid::new(4, 3);
    grid.enter_alt_screen();
    let before = grid.content_seq();

    grid.scroll_up(1);

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(content_rows_since(&grid, before), vec![0, 1, 2]);
}

#[test]
fn region_scroll_marks_only_the_region_content_changed() {
    let mut grid = Grid::new(4, 5);
    let before = grid.content_seq();

    grid.scroll_region_up(1, 3, 1);

    assert_eq!(content_rows_since(&grid, before), vec![1, 2, 3]);
}

#[test]
fn saved_primary_history_evictions_fold_into_the_active_counter() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(40);
    for _ in 0..20 {
        grid.scroll_up(1);
    }
    assert_eq!(grid.scrollback_len(), 20);
    let before = grid.scrollback_evicted();

    grid.enter_alt_screen();
    grid.set_scrollback_limit(4);

    assert_eq!(
        grid.scrollback_evicted() - before,
        16,
        "rows removed from the saved primary are removals from this pane's history"
    );
    grid.leave_alt_screen();
    assert_eq!(
        grid.scrollback_evicted() - before,
        16,
        "leaving the alternate screen must not lose or double-count folded evictions"
    );
}

#[test]
fn row_content_stamp_storage_stays_bounded_by_visible_rows() {
    let mut grid = Grid::new(8, 3);
    grid.set_scrollback_limit(5_000);
    for _ in 0..5_000 {
        grid.scroll_up(1);
    }

    assert_eq!(grid.row_content_seq.len(), grid.visible.len());
    assert!(
        grid.row_content_seq.capacity() <= grid.visible.capacity().max(grid.visible.len() * 2),
        "content identity storage follows the visible grid, not scrollback depth"
    );
}

#[test]
fn region_scroll_down_marks_only_the_region_content_changed() {
    let mut grid = Grid::new(4, 5);
    let before = grid.content_seq();

    grid.scroll_region_down(1, 3, 1);

    assert_eq!(content_rows_since(&grid, before), vec![1, 2, 3]);
}

#[test]
fn full_region_primary_scroll_preserves_survivor_identity() {
    let mut grid = Grid::new(4, 3);
    for row in 0..3 {
        grid.goto(row, 0);
        grid.put_char(
            char::from(b'a' + row as u8),
            Color::Default,
            Color::Default,
            CellFlags::empty(),
        );
    }
    let before = grid.content_seq();

    grid.scroll_region_up(0, 2, 1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(content_rows_since(&grid, before), vec![2]);
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
fn history_reshape_stays_within_total_cell_capacity_budget() {
    let mut grid = Grid::new(1024, 1);
    grid.scrollback_requested_limit = 1023;
    grid.scrollback_limit = 1023;
    for index in 0..1023 {
        let mut cells = vec![Cell::default(); 1024];
        cells[0].ch = char::from(b'a' + (index % 26) as u8);
        grid.scrollback.push_back(Line::from_flat(cells));
    }

    grid.resize(512, 1024);

    let retained_bytes = grid
        .rows_iter()
        .chain(grid.scrollback_iter())
        .map(Line::approx_capacity_byte_size)
        .sum::<usize>();
    let total_budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        retained_bytes <= total_budget_bytes,
        "visible + history retain {retained_bytes} bytes above {total_budget_bytes}"
    );
}

#[test]
fn entering_alt_screen_compacts_saved_primary_capacity() {
    let mut grid = Grid::new(1024, 512);
    grid.scrollback_requested_limit = 511;
    grid.scrollback_limit = 511;
    for index in 0..511 {
        let mut cells = vec![Cell::default(); 1024];
        cells[0].ch = char::from(b'a' + (index % 26) as u8);
        grid.scrollback.push_back(Line::from_flat(cells));
    }
    grid.resize(512, 512);

    grid.enter_alt_screen();

    let active_bytes = grid.rows_iter().map(Line::approx_capacity_byte_size).sum::<usize>();
    let saved = grid.alt_screen.as_ref().expect("saved primary");
    let saved_bytes = saved
        .rows_iter()
        .chain(saved.scrollback_iter())
        .map(Line::approx_capacity_byte_size)
        .sum::<usize>();
    let total_budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(active_bytes + saved_bytes <= total_budget_bytes);
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
        u64::from(grid.cols) * (u64::from(grid.rows) + grid.scrollback_limit as u64)
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

    assert!(
        grid.row(0)[0].extras().expect("combining marks retained").len() <= MAX_CELL_EXTRAS_BYTES
    );
}

#[test]
fn v120_grid_aggregate_retention_has_one_governor() {
    // The baseline invariant this sentinel was placed for: one figure covers
    // everything a grid retains, and it is the same figure the grid enforces
    // its own budget against. Two accounting paths that can disagree is the
    // failure this exists to prevent.
    let mut grid = Grid::new(80, 24);
    let empty = grid.retained_amount();
    assert_eq!(empty.items, 24, "a fresh grid retains its visible rows");

    grid.set_scrollback_limit(500);
    for row in 0..600 {
        for column in 0..40 {
            grid.put_char(
                char::from(b'a' + (column % 26) as u8),
                Color::Default,
                Color::Default,
                CellFlags::empty(),
            );
            let _ = row;
        }
        grid.linefeed();
    }

    let filled = grid.retained_amount();
    assert!(filled.bytes > empty.bytes, "scrollback growth is reflected in retained bytes");
    assert!(filled.items > empty.items, "scrollback growth is reflected in retained rows");

    // The reported figure is the enforced figure: it never exceeds the budget
    // the grid clamps itself to.
    let budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        filled.bytes <= budget_bytes,
        "retained {} bytes exceeds the {} byte budget the grid enforces",
        filled.bytes,
        budget_bytes
    );

    // Alternate-screen storage is counted in the same aggregate rather than
    // escaping it.
    grid.enter_alt_screen();
    for _ in 0..24 {
        for _ in 0..40 {
            grid.put_char('z', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.linefeed();
    }
    let with_alt = grid.retained_amount();
    assert!(
        with_alt.items > filled.items,
        "alternate-screen rows join the aggregate rather than escaping it"
    );
    assert!(
        with_alt.bytes <= budget_bytes,
        "the aggregate still respects the enforced budget with an alternate screen"
    );

    // Leaving the alternate screen returns to the primary aggregate.
    grid.leave_alt_screen();
    let restored = grid.retained_amount();
    assert!(restored.items <= with_alt.items, "leaving the alternate screen releases its rows");
}

#[test]
fn retained_amount_tracks_trimming_back_down() {
    // Retention has to fall when history is trimmed, or a governor would hold
    // a charge for storage that was already given back.
    let mut grid = Grid::new(40, 10);
    grid.set_scrollback_limit(200);
    for _ in 0..250 {
        for _ in 0..20 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.linefeed();
    }
    let grown = grid.retained_amount();

    grid.set_scrollback_limit(10);
    let trimmed = grid.retained_amount();

    assert!(
        trimmed.items < grown.items,
        "trimming history must reduce retained rows: {} -> {}",
        grown.items,
        trimmed.items
    );
}

// ---------------------------------------------------------------------------
// Rare-attribute box accounting
//
// `Cell` keeps hyperlink ids, grapheme extras, and non-default underline
// metadata behind `Option<Box<FatAttributes>>`. Row capacity accounting
// multiplies by `size_of::<Cell>()`, which counts the pointer slot and not the
// allocation behind it.
// ---------------------------------------------------------------------------

/// Linked cells must move the reported figure.
///
/// Before this was counted, filling a screen with OSC 8 links moved
/// `retained_amount().bytes` by **exactly zero** while allocating 76,800 bytes.
#[test]
fn linking_cells_moves_the_retained_figure() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    let mut linked = 0usize;
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char_linked(
                'x',
                Color::Default,
                Color::Default,
                CellFlags::empty(),
                Some(HyperlinkId(1)),
            );
            linked += 1;
        }
    }

    let after = grid.retained_amount().bytes;
    let delta = after.saturating_sub(plain);
    let fat = std::mem::size_of::<sonicterm_types::FatAttributes>();

    assert!(
        delta > 0,
        "linking {linked} cells allocated {} bytes and moved the reported figure by 0",
        linked * fat
    );
    // Cluster storage may collapse identical runs, so the figure is bounded
    // above by one box per cell rather than equal to it. Assert the shape,
    // not an exact count that storage form would make brittle.
    assert!(
        delta <= linked * fat,
        "reported {delta} bytes for at most {} of boxes — over-counting suggests \
         logical columns are being counted instead of stored cells",
        linked * fat
    );
}

/// Grapheme extras allocate the same box with no hyperlink involved.
///
/// Worth pinning separately: the excuse for excluding this figure was that the
/// hyperlink registry meters it. The registry meters URI *strings*. A cell
/// carrying only combining marks has no registry entry at all, so nothing else
/// in the process accounts for its box.
#[test]
fn grapheme_extras_are_counted_though_no_hyperlink_exists() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    // A base character followed by combining marks lands in `extras`.
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char('e', Color::Default, Color::Default, CellFlags::empty());
            grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
        }
    }

    let after = grid.retained_amount().bytes;
    assert!(
        after > plain,
        "combining marks allocate a rare-attribute box with no hyperlink and no \
         registry entry; nothing else in the process meters it"
    );
}

/// Clearing a linked cell must return the bytes.
///
/// A figure that only rises is a figure that will eventually read as a leak
/// whether or not one exists.
#[test]
fn clearing_linked_cells_returns_their_bytes() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char_linked(
                'x',
                Color::Default,
                Color::Default,
                CellFlags::empty(),
                Some(HyperlinkId(1)),
            );
        }
    }
    let linked = grid.retained_amount().bytes;
    assert!(linked > plain, "precondition: linking raised the figure");

    // Overwrite every cell with plain content.
    grid.cursor = Pos { row: 0, col: 0 };
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
    }

    let cleared = grid.retained_amount().bytes;
    assert!(
        cleared < linked,
        "overwriting linked cells with plain ones must return their boxes: \
         {cleared} vs {linked}"
    );
}

// ---------------------------------------------------------------------------
// Adjacent-resize capacity reclamation
//
// Growing a row by one column doubles its `Vec`; shrinking back truncates the
// length and keeps the capacity. Per row that excess is trivial. In aggregate
// over a populated scrollback it is not.
// ---------------------------------------------------------------------------

fn grid_with_scrollback(rows: usize) -> Grid {
    let mut grid = Grid::new(80, 24);
    for _ in 0..rows {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid
}

/// A window drag that returns to its starting width must return its memory.
///
/// Measured before this was handled: one ±1 column drag on a populated
/// scrollback permanently retained **1.875 MiB** — about 8% of a pane's entire
/// grid ceiling, from a gesture the user would not describe as doing anything.
#[test]
fn an_adjacent_resize_round_trip_returns_its_capacity() {
    let mut grid = grid_with_scrollback(12_000);
    let before = grid.retained_amount().bytes;

    grid.resize(81, 24);
    grid.resize(80, 24);

    let after = grid.retained_amount().bytes;
    let leaked = after.saturating_sub(before);

    // A small residual is acceptable — the threshold is hysteretic by design.
    // What is not acceptable is the doubling.
    assert!(
        leaked < before / 8,
        "a ±1 column round trip retained {:.3} MiB against a starting {:.3} MiB",
        leaked as f64 / 1048576.0,
        before as f64 / 1048576.0
    );
}

/// Dragging must not reallocate every row on every frame.
///
/// This is the other half of the criterion, and it pulls against the test
/// above: a threshold tight enough to reclaim on every step would compact
/// thousands of rows per frame of a drag. The band exists to make a drag
/// settle rather than thrash.
#[test]
fn dragging_a_window_edge_does_not_thrash_allocations() {
    let mut grid = grid_with_scrollback(12_000);
    grid.resize(81, 24);
    grid.resize(80, 24);

    let mut changes = 0usize;
    let mut previous = grid.retained_amount().bytes;
    // The shape of a drag: adjacent widths, back and forth, ending where it
    // started.
    for width in [81u16, 82, 81, 80, 79, 80, 81, 80] {
        grid.resize(width, 24);
        let now = grid.retained_amount().bytes;
        if now != previous {
            changes += 1;
        }
        previous = now;
    }

    assert!(
        changes <= 3,
        "the retained figure moved on {changes} of 8 adjacent resize steps; a drag \
         must settle into steady state rather than compacting every frame"
    );
}

/// A resize that genuinely shrinks the grid must still release, immediately.
///
/// The hysteresis must not become an excuse to keep memory after the user has
/// made the window materially smaller — that is the case where they can see
/// the space they gave back.
#[test]
fn a_large_shrink_releases_without_waiting_for_a_threshold() {
    let mut grid = grid_with_scrollback(12_000);
    grid.resize(200, 24);
    let wide = grid.retained_amount().bytes;

    grid.resize(40, 24);
    let narrow = grid.retained_amount().bytes;

    assert!(
        narrow < wide / 2,
        "shrinking 200 → 40 columns must release: {} MiB → {} MiB",
        wide / 1048576,
        narrow / 1048576
    );
}

/// Content must survive the compaction.
///
/// Reclaiming capacity is only correct if it reclaims *capacity*. A pass that
/// dropped cells would be trading the user's scrollback for memory.
#[test]
fn compaction_preserves_the_content_it_compacts() {
    let mut grid = Grid::new(80, 24);
    for row in 0..40 {
        for _ in 0..70 {
            grid.put_char(
                char::from(b'a' + (row % 26) as u8),
                Color::Default,
                Color::Default,
                CellFlags::empty(),
            );
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    let rows_before = grid.retained_amount().items;
    let sample: String = grid.row(0).iter().map(|cell| cell.ch).collect();

    grid.resize(81, 24);
    grid.resize(80, 24);

    assert_eq!(grid.retained_amount().items, rows_before, "no row may be dropped");
    assert_eq!(
        grid.row(0).iter().map(|cell| cell.ch).collect::<String>(),
        sample,
        "cell content must survive a capacity compaction"
    );
}

/// The regions must sum to the total, in every state the grid can be in.
///
/// This is what makes splitting the charge safe: if the parts did not sum to
/// the whole, attributing them to separate classes would silently change how
/// much is charged, not just where. A governor would then be reading a
/// different number than the one every existing test pins.
#[test]
fn region_amounts_sum_to_the_retained_total() {
    let mut grid = Grid::new(80, 24);

    let states: [(&str, fn(&mut Grid)); 5] = [
        ("empty", |_| {}),
        ("visible content", |g| {
            for _ in 0..20 {
                for _ in 0..70 {
                    g.put_char('x', Color::Default, Color::Default, CellFlags::empty());
                }
                g.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
            }
        }),
        ("populated scrollback", |g| {
            for _ in 0..500 {
                for _ in 0..70 {
                    g.put_char('y', Color::Default, Color::Default, CellFlags::empty());
                }
                g.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
            }
        }),
        ("alternate screen active", |g| g.enter_alt_screen()),
        ("back to primary", |g| g.leave_alt_screen()),
    ];

    for (name, step) in states {
        step(&mut grid);
        let regions = grid.retained_amount_by_region();
        let total = grid.retained_amount();
        assert_eq!(
            regions.total(),
            total,
            "regions must sum to the total in state '{name}': \
             visible={:?} history={:?} alternate={:?}",
            regions.visible,
            regions.history,
            regions.alternate
        );
    }
}

/// Entering an alternate screen must move bytes into the alternate region,
/// not merely leave them in history.
///
/// This is the attribution the split exists for: the saved primary is what an
/// operator would want to see separated, because it is memory held for a
/// screen the user is not currently looking at.
#[test]
fn entering_an_alternate_screen_attributes_the_saved_primary_separately() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..500 {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }

    let before = grid.retained_amount_by_region();
    assert_eq!(before.alternate, ResourceAmount::default(), "no alternate screen yet");
    assert!(before.history.bytes > 0, "precondition: history holds the scrollback");

    grid.enter_alt_screen();
    let during = grid.retained_amount_by_region();

    assert!(
        during.alternate.bytes > 0,
        "the saved primary must be attributed to the alternate region, not left in history"
    );
    assert!(
        during.history.bytes < before.history.bytes,
        "and history must no longer be carrying it: {} vs {}",
        during.history.bytes,
        before.history.bytes
    );

    grid.leave_alt_screen();
    let after = grid.retained_amount_by_region();
    assert_eq!(after.alternate, ResourceAmount::default(), "leaving must clear the region");
}

/// Row containers are counted, not only the cells they point at.
///
/// A `VecDeque<Line>` reserves `Line` headers independently of the cell
/// storage each points at, and nothing else in the process counts them. The
/// figure is small — 0.68% on a full 200×50 grid — and is counted for the same
/// reason the rare-attribute boxes are: "small" was the answer there too,
/// before measurement made it 1.67× the reported total.
#[test]
fn row_containers_are_counted_in_the_retained_figure() {
    let mut grid = Grid::new(80, 24);
    let empty = grid.retained_amount().bytes;

    // Grow the scrollback so the deque reserves materially more slots.
    for _ in 0..2_000 {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    let full = grid.retained_amount().bytes;

    let line = std::mem::size_of::<Line>();
    let container = (grid.visible.capacity() + grid.scrollback.capacity()) * line
        + grid.dirty_rows.capacity() * std::mem::size_of::<bool>()
        + grid.row_content_seq.capacity() * std::mem::size_of::<u64>();
    assert!(container > 0, "precondition: the deques reserved slots");

    let cells: usize =
        grid.rows_iter().chain(grid.scrollback_iter()).map(Line::approx_capacity_byte_size).sum();

    assert!(
        full > cells,
        "the reported figure must exceed cell storage alone; containers and boxes are \
         grid-owned and counted by nothing else"
    );
    assert!(full > empty, "filling the grid must move the figure");
}

/// Adding the container term must not break the region split.
///
/// The three regions sum to the total, and that property is what lets them be
/// charged to separate classes. A term added to the total and not to a region
/// would silently change how much is charged.
#[test]
fn container_bytes_keep_the_region_split_exact() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..300 {
        for _ in 0..70 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid.enter_alt_screen();

    assert_eq!(
        grid.retained_amount_by_region().total(),
        grid.retained_amount(),
        "regions must still sum to the total with containers counted"
    );
}

/// The saved primary's own containers are counted, not just its rows.
///
/// Entering an alternate screen boxes the whole primary `Grid`: its two deque
/// spines, dirty bitset, row-content stamps, prompt ring, and the struct itself.
/// Counting only the rows leaves the rest held but unreported.
///
/// Pinned here rather than in the counting-allocator suite because the term is
/// fixed-size — measured at 232 bytes — and the allocations the test harness
/// makes inside a measurement window are larger than that. At this level no
/// allocator is involved and the figure is exact.
#[test]
fn entering_an_alternate_screen_counts_the_saved_primarys_containers() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..300 {
        for _ in 0..70 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid.enter_alt_screen();

    let saved = grid.alt_screen.as_ref().expect("the alternate screen holds the saved primary");
    let saved_rows = saved.visible.capacity().saturating_add(saved.scrollback.capacity())
        * std::mem::size_of::<Line>();
    let saved_dirty = saved.dirty_rows.capacity() * std::mem::size_of::<bool>();
    let saved_content_stamps = saved.row_content_seq.capacity() * std::mem::size_of::<u64>();
    let saved_prompts = saved.prompts.capacity() * std::mem::size_of::<PromptRegion>();
    let expected_saved = saved_rows
        + saved_dirty
        + saved_content_stamps
        + saved_prompts
        + std::mem::size_of::<Grid>();

    let live_rows = grid.visible.capacity().saturating_add(grid.scrollback.capacity())
        * std::mem::size_of::<Line>();
    let live_dirty = grid.dirty_rows.capacity() * std::mem::size_of::<bool>();
    let live_content_stamps = grid.row_content_seq.capacity() * std::mem::size_of::<u64>();

    assert!(
        expected_saved > saved_rows,
        "precondition: the non-row terms must be non-zero or this asserts nothing"
    );
    assert_eq!(
        grid.container_bytes(),
        live_rows + live_dirty + live_content_stamps + expected_saved,
        "the saved primary's spines, dirty bitset, content stamps, prompt ring and struct are \
         all memory held while the alternate screen shows, and all must be counted"
    );
}

/// Grapheme extras move the reported figure.
///
/// A cell's trailing zero-width codepoints live in a `Box<str>` behind the
/// rare-attribute box. A figure built from `size_of::<FatAttributes>()` alone
/// reports a screen of accented text identically to a screen of plain text,
/// while the accented one holds a separate allocation per cell.
#[test]
fn grapheme_extras_move_the_reported_figure() {
    fn screen_with_marks(marks: usize) -> usize {
        let mut grid = Grid::new(80, 24);
        for _ in 0..24 {
            for _ in 0..80 {
                grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
                for _ in 0..marks {
                    // U+0301 COMBINING ACUTE ACCENT: zero width, 2 bytes UTF-8.
                    grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
                }
            }
        }
        grid.retained_amount().bytes
    }

    let plain = screen_with_marks(0);
    let light = screen_with_marks(8);
    let heavy = screen_with_marks(24);

    assert!(
        light > plain,
        "a screen of accented text holds an allocation per cell that plain text does not, \
         so it must report above it (plain {plain}, accented {light})"
    );
    assert!(
        heavy > light,
        "the figure must scale with the payload rather than being a per-cell constant \
         ({light} at 8 marks, {heavy} at 24)"
    );
}
