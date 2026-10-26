use super::*;
use crate::grid::grid_test_support::*;

#[test]
fn column_shrink_repairs_wide_tails_in_visible_history_and_saved_primary() {
    // All grid-owned row buffers share Line's clipped-pair rule and cannot resurrect clipped text.
    let mut grid = Grid::new(4, 2);
    for row in 0..2 {
        grid.goto(row, 0);
        for character in ['a', 'b', '中'] {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    grid.scroll_up(1);
    grid.enter_alt_screen();
    for character in ['a', 'b', '中'] {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.resize(3, 2);
    assert_eq!(grid.row(0)[2], Cell::default());
    let primary = grid.alt_screen.as_ref().unwrap();
    assert_eq!(primary.row(0)[2], Cell::default());
    assert_eq!(primary.scrollback_row(0).unwrap()[2], Cell::default());
    grid.resize(4, 2);
    grid.leave_alt_screen();
    for line in grid.rows_iter().chain(grid.scrollback_iter()) {
        assert_eq!(line[2], Cell::default());
        assert_eq!(line[3], Cell::default());
    }
}

/// Exact alternate-screen save and restore preserves proven rows but changes buffer identity.
#[test]
fn alternate_screen_round_trip_preserves_provenance_with_new_epochs() {
    let mut grid = Grid::new(4, 3);
    for character in "abcde".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    let primary_epoch = grid.screen_epoch();
    assert!(grid.row(1).soft_wrapped_from_previous());

    grid.enter_alt_screen();
    let alternate_epoch = grid.screen_epoch();
    assert_ne!(alternate_epoch, primary_epoch);

    grid.leave_alt_screen();
    assert!(grid.row(1).soft_wrapped_from_previous());
    assert_ne!(grid.screen_epoch(), alternate_epoch);
}

/// Geometry changes invalidate provenance in visible rows and retained history.
#[test]
fn resize_clears_all_wrap_provenance_without_growing_line_headers() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(8);
    grid.set_soft_wrapped_from_previous(1, true);
    grid.scroll_up(1);
    grid.set_soft_wrapped_from_previous(1, true);

    grid.resize(5, 2);

    assert!(grid.rows_iter().all(|row| !row.soft_wrapped_from_previous()));
    assert!(grid.scrollback_iter().all(|row| !row.soft_wrapped_from_previous()));
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

/// Lines of 70 characters ended by CR LF, so each history row is 70 characters and 10 blanks. A
/// 10-column tail saves under 256 bytes, so every row stays flat and these tests measure `Vec`
/// capacity, not the trimmed form.
fn grid_with_scrollback(rows: usize) -> Grid {
    let mut grid = Grid::new(80, 24);
    for _ in 0..rows {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.carriage_return();
        grid.linefeed();
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

#[test]
fn size_generation_counts_only_real_dimension_changes() {
    // Cell addresses stay comparable across a same-size resize and screen switches; a changed
    // row or column count, including one on the alternate screen, advances the generation once.
    let mut grid = Grid::new(20, 5);
    let start = grid.size_generation();
    grid.resize(20, 5);
    assert_eq!(grid.size_generation(), start, "same size");
    grid.resize(20, 3);
    assert_eq!(grid.size_generation(), start + 1, "rows changed");
    grid.resize(21, 3);
    assert_eq!(grid.size_generation(), start + 2, "cols changed");
    grid.enter_alt_screen();
    grid.resize(22, 3);
    grid.leave_alt_screen();
    assert_eq!(grid.size_generation(), start + 3, "the alternate-screen resize is kept");
}
