use super::*;

#[test]
fn history_prefix_removal_rebases_surviving_prompt_regions() {
    // Explicit limit reduction and ordinary FIFO eviction share prompt and eviction identity bookkeeping.
    for trim_by_limit in [true, false] {
        let mut grid = Grid::new(4, 2);
        grid.set_scrollback_limit(if trim_by_limit { 4 } else { 1 });
        grid.record_prompt_start();
        grid.goto(1, 0);
        grid.record_prompt_end(Some(0));
        grid.scroll_up(1);
        grid.record_prompt_start();
        grid.record_prompt_end(Some(7));
        let before = grid.scrollback_evicted();
        if trim_by_limit {
            grid.set_scrollback_limit(0);
        } else {
            grid.scroll_up(1);
        }
        let prompts: Vec<_> = grid.prompts().cloned().collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].start_row, 1);
        assert_eq!(prompts[0].end_row, Some(1));
        assert_eq!(prompts[0].exit_code, Some(7));
        assert_eq!(grid.scrollback_evicted(), before + 1);
    }
}

#[test]
fn clear_scrollback_preserves_live_stamps_and_rebases_prompt_identity() {
    // History erasure invalidates historical anchors without pretending the live rows changed content.
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(17);
    grid.record_prompt_start();
    grid.scroll_up(2);
    grid.goto(0, 0);
    grid.record_prompt_start();
    grid.goto(1, 0);
    grid.record_prompt_end(Some(3));
    for character in "live".chars() {
        grid.put_char(character, Color::Indexed(2), Color::Indexed(4), CellFlags::BOLD);
    }
    let visible = grid.visible.clone();
    let stamps = grid.row_content_seq.clone();
    let cursor = grid.cursor;
    let sequence = grid.content_seq();
    let revision = grid.revision();
    let evicted = grid.scrollback_evicted();
    let epoch = grid.screen_epoch();
    grid.clear_dirty();

    grid.clear_scrollback();

    assert!(grid.scrollback.is_empty());
    assert_eq!(grid.scrollback.capacity(), 0);
    assert_eq!(grid.visible, visible);
    assert_eq!(grid.row_content_seq, stamps);
    assert_eq!(grid.cursor, cursor);
    assert!(grid.pending_wrap());
    assert_eq!(grid.content_seq(), sequence);
    assert_eq!(grid.screen_epoch(), epoch);
    assert_ne!(grid.revision(), revision);
    assert_eq!(grid.dirty_count(), 2);
    assert_eq!(grid.scrollback_evicted(), evicted + 2);
    assert_eq!(grid.scrollback_requested_limit, 17);
    assert_eq!(grid.scrollback_limit, 17);
    assert_eq!(grid.rows_since_budget_check, 0);
    assert_eq!(
        grid.prompts().copied().collect::<Vec<_>>(),
        vec![PromptRegion { start_row: 0, end_row: Some(1), exit_code: Some(3) }]
    );
    grid.clear_dirty();
    let revision = grid.revision();
    grid.clear_scrollback();
    assert_eq!(grid.revision(), revision);
    assert_eq!(grid.dirty_count(), 0);
    assert_eq!(grid.scrollback_evicted(), evicted + 2);
    for _ in 0..18 {
        grid.scroll_up(1);
    }
    assert_eq!(grid.scrollback_len(), 17);
    assert_eq!(grid.scrollback_evicted(), evicted + 3);
}

#[test]
fn fifo_evictions_preserve_amortized_budget_check_cadence() {
    // Recycling an oldest row cannot reset the counter and indefinitely postpone retained-byte enforcement.
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(1);
    for _ in 0..ROWS_BETWEEN_BUDGET_CHECKS - 1 {
        grid.scroll_up(1);
    }
    assert_eq!(grid.rows_since_budget_check, ROWS_BETWEEN_BUDGET_CHECKS - 1);
    grid.scroll_up(1);
    assert_eq!(grid.rows_since_budget_check, 0);
    assert_eq!(grid.scrollback_evicted(), (ROWS_BETWEEN_BUDGET_CHECKS - 1) as u64);
}

#[test]
fn saved_primary_prompt_regions_follow_history_prefix_removal() {
    // Prompt coordinates follow their owning rows while alternate-screen history pressure trims the primary.
    let mut grid = Grid::new(4, 2);
    grid.record_prompt_start();
    grid.scroll_up(1);
    grid.goto(1, 0);
    grid.record_prompt_start();
    grid.record_prompt_end(Some(7));
    let evicted = grid.scrollback_evicted();
    grid.enter_alt_screen();
    assert_eq!(grid.prompts_len(), 0);
    grid.record_prompt_start();
    grid.set_scrollback_limit(0);
    assert_eq!(grid.scrollback_evicted(), evicted + 1);
    assert_eq!(grid.prompts().next().unwrap().start_row, 0);
    grid.leave_alt_screen();
    assert_eq!(grid.scrollback_evicted(), evicted + 1);
    assert_eq!(
        grid.prompts().copied().collect::<Vec<_>>(),
        vec![PromptRegion { start_row: 1, end_row: Some(1), exit_code: Some(7) }]
    );
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
    for character in "abcdefgh".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
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

/// Only a byte-budget trim advances `budget_trimmed_rows`: at-capacity scrolling and a clear
/// evict rows without counting, and the trim counts exactly the rows it dropped.
#[test]
fn budget_trimmed_rows_counts_only_byte_budget_trims() {
    let mut grid = Grid::new(10, 3);
    grid.set_scrollback_limit(100);
    for _ in 0..200 {
        grid.linefeed();
    }
    assert!(grid.scrollback_evicted() > 0, "at-capacity scrolling evicted rows");
    assert_eq!(grid.budget_trimmed_rows(), 0);
    grid.clear_scrollback();
    assert_eq!(grid.budget_trimmed_rows(), 0, "a clear is not a budget trim");
    for _ in 0..100 {
        grid.linefeed();
    }
    let before = grid.scrollback_len();
    let evicted = grid.scrollback_evicted();
    grid.trim_scrollback_to_reported_budget(0);
    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(grid.budget_trimmed_rows(), before as u64);
    assert_eq!(grid.scrollback_evicted() - evicted, before as u64);
    assert_eq!(grid.budget_trimmed_at_clear(), 0);
    grid.clear_scrollback();
    assert_eq!(
        grid.budget_trimmed_at_clear(),
        before as u64,
        "a clear with no history left still marks"
    );
}
