//! Integration tests for the sonicterm-core grid re-exports.

use sonicterm_grid::grid::*;

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
    assert!(c.hyperlink().is_none());
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

// ============================================================================
// Epic B2: dirty-row tracking tests
//
// Every grid mutator must mark the rows it touches so the renderer can
// skip re-shaping rows whose cells haven't changed. Tests below pin
// down the exact set of rows each mutator marks.
// ============================================================================

#[test]
fn dirty_fresh_grid_is_fully_dirty() {
    // A fresh grid has never been rendered, so every row counts as
    // dirty until the renderer walks it and calls clear_dirty().
    let g = Grid::new(8, 4);
    assert_eq!(g.dirty_count(), 4);
    for r in 0..4 {
        assert!(g.is_row_dirty(r), "row {r} should be dirty on fresh grid");
    }
}

#[test]
fn dirty_clear_empties_and_does_not_bump_revision() {
    let mut g = Grid::new(8, 4);
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    let rev_before = g.revision();
    g.clear_dirty();
    assert_eq!(g.dirty_count(), 0);
    for r in 0..4 {
        assert!(!g.is_row_dirty(r));
    }
    // clear_dirty is NOT a mutator — it must not bump revision.
    assert_eq!(g.revision(), rev_before);
}

#[test]
fn dirty_put_char_marks_cursor_row_only() {
    let mut g = Grid::new(8, 4);
    g.goto(2, 0);
    g.clear_dirty();
    g.put_char('X', Color::Default, Color::Default, CellFlags::empty());
    assert!(g.is_row_dirty(2));
    assert!(!g.is_row_dirty(0));
    assert!(!g.is_row_dirty(1));
    assert!(!g.is_row_dirty(3));
    assert_eq!(g.dirty_count(), 1);
}

#[test]
fn dirty_carriage_return_marks_cursor_row() {
    let mut g = Grid::new(8, 4);
    g.goto(1, 5);
    g.clear_dirty();
    g.carriage_return();
    assert!(g.is_row_dirty(1));
    assert_eq!(g.dirty_count(), 1);
}

#[test]
fn dirty_linefeed_marks_old_and_new_rows() {
    let mut g = Grid::new(8, 4);
    g.goto(1, 0);
    g.clear_dirty();
    g.linefeed();
    // moved from row 1 -> row 2: both should be dirty.
    assert!(g.is_row_dirty(1));
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 2);
}

#[test]
fn dirty_linefeed_at_bottom_scrolls_marks_all() {
    let mut g = Grid::new(8, 4);
    g.goto(3, 0); // bottom row
    g.clear_dirty();
    g.linefeed(); // forces scroll_up(1)
                  // scroll_up marks every row dirty.
    assert_eq!(g.dirty_count(), 4);
}

#[test]
fn dirty_backspace_and_tab_mark_cursor_row() {
    let mut g = Grid::new(40, 4);
    g.goto(2, 10);
    g.clear_dirty();
    g.backspace();
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 1);

    g.clear_dirty();
    g.tab();
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 1);
}

#[test]
fn dirty_goto_marks_both_old_and_new_rows() {
    let mut g = Grid::new(8, 5);
    g.goto(1, 0);
    g.clear_dirty();
    g.goto(3, 4);
    assert!(g.is_row_dirty(1), "old cursor row must be dirty");
    assert!(g.is_row_dirty(3), "new cursor row must be dirty");
    assert!(!g.is_row_dirty(0));
    assert!(!g.is_row_dirty(2));
    assert!(!g.is_row_dirty(4));
    assert_eq!(g.dirty_count(), 2);
}

#[test]
fn dirty_scroll_up_marks_all_rows() {
    let mut g = Grid::new(8, 4);
    g.clear_dirty();
    g.scroll_up(1);
    assert_eq!(g.dirty_count(), 4);
}

#[test]
fn dirty_erase_line_variants_mark_cursor_row_only() {
    let mut g = Grid::new(8, 4);
    g.goto(2, 4);
    g.clear_dirty();
    g.erase_line_to_end();
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 1);

    g.clear_dirty();
    g.erase_line_to_start();
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 1);

    g.clear_dirty();
    g.erase_line();
    assert!(g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 1);
}

#[test]
fn dirty_erase_below_marks_cursor_to_end() {
    let mut g = Grid::new(8, 5);
    g.goto(2, 0);
    g.clear_dirty();
    g.erase_below();
    assert!(!g.is_row_dirty(0));
    assert!(!g.is_row_dirty(1));
    assert!(g.is_row_dirty(2));
    assert!(g.is_row_dirty(3));
    assert!(g.is_row_dirty(4));
    assert_eq!(g.dirty_count(), 3);
}

#[test]
fn dirty_erase_above_marks_0_through_cursor() {
    let mut g = Grid::new(8, 5);
    g.goto(2, 0);
    g.clear_dirty();
    g.erase_above();
    assert!(g.is_row_dirty(0));
    assert!(g.is_row_dirty(1));
    assert!(g.is_row_dirty(2));
    assert!(!g.is_row_dirty(3));
    assert!(!g.is_row_dirty(4));
    assert_eq!(g.dirty_count(), 3);
}

#[test]
fn dirty_erase_screen_marks_all() {
    let mut g = Grid::new(8, 5);
    g.clear_dirty();
    g.erase_screen();
    assert_eq!(g.dirty_count(), 5);
}

#[test]
fn dirty_resize_reallocates_bitset_and_marks_all() {
    let mut g = Grid::new(8, 4);
    g.clear_dirty();
    g.resize(10, 6);
    // bitset must be resized to the new row count
    assert_eq!(g.dirty_count(), 6);
    for r in 0..6 {
        assert!(g.is_row_dirty(r), "row {r} should be dirty after resize");
    }
    // Shrink also re-allocates and marks all.
    g.clear_dirty();
    g.resize(10, 3);
    assert_eq!(g.dirty_count(), 3);
}

#[test]
fn dirty_alt_screen_transitions_mark_all() {
    let mut g = Grid::new(8, 4);
    g.clear_dirty();
    g.enter_alt_screen();
    assert_eq!(g.dirty_count(), 4);
    g.clear_dirty();
    g.leave_alt_screen();
    assert_eq!(g.dirty_count(), 4);
}

#[test]
fn dirty_count_after_specific_sequence() {
    // Write "ABC" across two rows, then erase the second row, then
    // move cursor away — exact dirty set is well-defined.
    let mut g = Grid::new(4, 3);
    g.clear_dirty();
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.put_char('B', Color::Default, Color::Default, CellFlags::empty());
    g.put_char('C', Color::Default, Color::Default, CellFlags::empty());
    g.linefeed();
    g.put_char('D', Color::Default, Color::Default, CellFlags::empty());
    // Touched rows: 0 (the three puts), 1 (linefeed arrival + put).
    assert!(g.is_row_dirty(0));
    assert!(g.is_row_dirty(1));
    assert!(!g.is_row_dirty(2));
    assert_eq!(g.dirty_count(), 2);
}

#[test]
fn prompt_before_and_after_navigate() {
    let mut g = Grid::new(10, 5);
    g.record_prompt_start(); // row 0
    g.goto(2, 0);
    g.record_prompt_start(); // row 2
    g.goto(4, 0);
    g.record_prompt_start(); // row 4
    assert_eq!(g.prompts_len(), 3);

    // From row 3, previous = 2, next = 4.
    assert_eq!(g.prompt_before(3).map(|p| p.start_row), Some(2));
    assert_eq!(g.prompt_after(3).map(|p| p.start_row), Some(4));

    // From row 4, next is None (no prompt strictly greater).
    assert!(g.prompt_after(4).is_none());
    // From row 0, prev is None.
    assert!(g.prompt_before(0).is_none());
}

#[test]
fn prompt_visible_row_tracks_scrollback() {
    let mut g = Grid::new(4, 3);
    // Generate scrollback so prompt_visible_row exercise the offset math.
    g.record_prompt_start(); // absolute row 0
                             // Scroll three lines — that pushes the prompt row off-screen.
    g.scroll_up(3);
    let p = *g.prompts().next().unwrap();
    assert_eq!(p.start_row, 0);
    assert_eq!(g.prompt_visible_row(&p), None);

    // Record a new prompt while the cursor is at visible row 1, with
    // 3 lines of scrollback → absolute row 4.
    g.goto(1, 0);
    g.record_prompt_start();
    let p2 = *g.prompts().last().unwrap();
    assert_eq!(p2.start_row, 4);
    assert_eq!(g.prompt_visible_row(&p2), Some(1));
}

#[test]
fn prompt_region_limit_evicts_oldest() {
    let mut g = Grid::new(10, 2);
    for _ in 0..(PROMPT_REGION_LIMIT + 5) {
        // Force unique absolute rows by scrolling between markers so the
        // coalescing guard doesn't drop them.
        g.record_prompt_start();
        g.scroll_up(1);
    }
    assert_eq!(g.prompts_len(), PROMPT_REGION_LIMIT);
}

#[test]
fn zwj_codepoints_are_retained_as_cell_extras() {
    // 👨‍👩‍👧 = MAN (U+1F468, width 2) + ZWJ (U+200D, width 0) +
    // WOMAN (U+1F469, width 2) + ZWJ (U+200D, width 0) + GIRL
    // (U+1F467, width 2). Without ZWJ retention the shaper sees three
    // independent base emoji and can never compose the family glyph.
    // After retention: the three base emoji each occupy a wide cell,
    // and the ZWJs are attached as `extras` to the preceding lead
    // cell so the shaper sees the full cluster.
    let mut g = Grid::new(20, 1);
    for ch in "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }

    // Reconstruct the byte stream the shaper would see by walking
    // non-WIDE_CONT cells in row 0 and inlining their extras.
    let mut shaped_text = String::new();
    for cell in g.row(0) {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            continue;
        }
        if cell.ch == ' ' && cell.extras().is_none() {
            continue;
        }
        shaped_text.push(cell.ch);
        if let Some(extras) = cell.extras() {
            for ch in extras.chars() {
                shaped_text.push(ch);
            }
        }
    }
    assert_eq!(
        shaped_text, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "ZWJ codepoints (U+200D) must be retained in cell extras so \
         the shaper sees the full ZWJ family cluster"
    );

    // Confirm the U+200D codepoint is literally present somewhere in
    // a cell's `extras` (not just in shaped_text by accident).
    let zwjs: usize = g
        .row(0)
        .iter()
        .filter_map(|c| c.extras())
        .map(|ex| ex.chars().filter(|c| *c == '\u{200D}').count())
        .sum();
    assert_eq!(zwjs, 2, "expected two ZWJ joiners attached as cell extras");
}

#[test]
fn scroll_up_preserves_row_content_after_rotation() {
    // VecDeque ring-rotation should be byte-identical to the old
    // memmove implementation: row k after `scroll_up(n)` must equal
    // row k+n before, for k+n < rows. The freshly-introduced bottom
    // row(s) must be blank.
    use sonicterm_grid::grid::{CellFlags, Color, Grid};
    let mut g = Grid::new(8, 5);
    // Fill each row with a distinct letter so we can tell them apart.
    for r in 0..5u16 {
        g.goto(r, 0);
        let ch = (b'A' + r as u8) as char;
        for _ in 0..3 {
            g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    g.scroll_up(2);
    // Row 0 was 'A', row 1 'B' → after scroll_up(2) row 0 should be 'C'.
    assert_eq!(g.row(0)[0].ch, 'C');
    assert_eq!(g.row(1)[0].ch, 'D');
    assert_eq!(g.row(2)[0].ch, 'E');
    // Bottom two rows are freshly blank.
    assert_eq!(g.row(3)[0].ch, ' ');
    assert_eq!(g.row(4)[0].ch, ' ');
    // Popped rows landed in scrollback in order.
    assert_eq!(g.scrollback_len(), 2);
    assert_eq!(g.scrollback_row(0).unwrap()[0].ch, 'A');
    assert_eq!(g.scrollback_row(1).unwrap()[0].ch, 'B');
}

#[test]
fn scroll_up_recycles_oldest_scrollback_row_at_limit() {
    // Once scrollback is at the limit, a further scroll should evict
    // the oldest row AND reuse its buffer (no allocation, no row leak).
    use sonicterm_grid::grid::{CellFlags, Color, Grid};
    let mut g = Grid::new(4, 3);
    g.set_scrollback_limit(2);
    for tag in [b'A', b'B', b'C', b'D', b'E'] {
        g.goto(0, 0);
        for _ in 0..2 {
            g.put_char(tag as char, Color::Default, Color::Default, CellFlags::empty());
        }
        g.scroll_up(1);
    }
    // Limit is 2 — only the two newest scrollback entries survive.
    assert_eq!(g.scrollback_len(), 2);
    assert_eq!(g.scrollback_row(0).unwrap()[0].ch, 'D');
    assert_eq!(g.scrollback_row(1).unwrap()[0].ch, 'E');
    // The new blank row at the bottom is fully cleared (recycled
    // buffer must not leak old 'A' cells).
    let bottom = g.row(g.rows.saturating_sub(1));
    for c in bottom {
        assert_eq!(c.ch, ' ');
    }
}
