use super::*;

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
    for column in 0..grid.cols as usize {
        let flags = row[column].flags;
        if flags.contains(CellFlags::WIDE) {
            assert!(column + 1 < grid.cols as usize, "wide lead at row end");
            assert!(
                row[column + 1].flags.contains(CellFlags::WIDE_CONT),
                "wide lead without continuation at col {column}"
            );
        }
        if flags.contains(CellFlags::WIDE_CONT) {
            assert!(column > 0, "wide continuation at col 0");
            assert!(
                row[column - 1].flags.contains(CellFlags::WIDE),
                "wide continuation without lead at col {column}"
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
    for character in "0.1".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.insert_cells_with(0, 0, 2, Cell::default());
    let after: String = grid.row(0).iter().map(|cell| cell.ch).collect();
    assert_eq!(after, "  0.1   ");
}

#[test]
fn insert_cells_mid_line_preserves_both_sides() {
    // Insert in the middle: "abcd" + insert 1 at col 2 → "ab cd ".
    let mut grid = Grid::new(6, 1);
    for character in "abcd".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.insert_cells_with(0, 2, 1, Cell::default());
    let after: String = grid.row(0).iter().map(|cell| cell.ch).collect();
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
    let after: String = grid.row(0).iter().map(|cell| cell.ch).collect();
    assert!(after.contains('x'), "narrow cell after the split must survive: {after:?}");
    // No dangling wide half: every WIDE_CONT must have a WIDE lead to its left.
    for column in 0..6 {
        if grid.row(0)[column].flags.contains(CellFlags::WIDE_CONT) {
            assert!(
                column > 0 && grid.row(0)[column - 1].flags.contains(CellFlags::WIDE),
                "orphaned wide continuation at col {column}: {after:?}"
            );
        }
    }
}

#[test]
fn insert_delete_and_erase_cells_preserve_row_width() {
    let mut grid = Grid::new(6, 1);
    for character in "abcdef".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }

    grid.insert_cells(0, 2, 2);
    assert_eq!(grid.row(0).len(), 6);
    grid.delete_cells(0, 1, 3);
    assert_eq!(grid.row(0).len(), 6);
    grid.erase_cells(0, 1, 2);
    assert_eq!(grid.row(0).len(), 6);
    assert_row_has_no_orphan_wide_cells(&grid);
}

/// Erasing a continuation row keeps its incoming wrap; erasing the wrapping row's last column revokes it.
#[test]
fn directional_erases_revoke_only_the_outgoing_wrap() {
    // "/tmp/file" on five columns wraps "file" onto row 1.
    let wrapped = || {
        let mut grid = Grid::new(5, 2);
        for character in "/tmp/file".chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        assert!(grid.row(1).soft_wrapped_from_previous());
        grid
    };

    // zsh repaints a continuation row with CR + EL0; the logical line survives every row erase.
    for operation in 0..3 {
        let mut grid = wrapped();
        grid.goto(1, if operation == 1 { 4 } else { 0 });
        match operation {
            0 => grid.erase_line_to_end(),
            1 => grid.erase_line_to_start(),
            _ => grid.erase_line(),
        }
        assert!(grid.row(1).soft_wrapped_from_previous(), "continuation erase {operation}");
    }

    // EL0, EL1 and EL2 that reach the wrapping row's last column revoke the continuation.
    for operation in 0..3 {
        let mut grid = wrapped();
        grid.goto(0, if operation == 1 { 4 } else { 2 });
        match operation {
            0 => grid.erase_line_to_end(),
            1 => grid.erase_line_to_start(),
            _ => grid.erase_line(),
        }
        assert!(!grid.row(1).soft_wrapped_from_previous(), "wrapping-row erase {operation}");
    }

    // EL1 that stops before the last column leaves the wrapping cell, and so the wrap, intact.
    let mut partial = wrapped();
    partial.goto(0, 2);
    partial.erase_line_to_start();
    assert!(partial.row(1).soft_wrapped_from_previous());
}

/// ED1 revokes the cursor row's outgoing wrap only when its partial erase reaches the last column.
#[test]
fn erase_above_revokes_successor_wrap_only_through_last_column() {
    for (cursor_col, wrap_kept) in [(1, true), (3, false)] {
        let mut grid = Grid::new(4, 4);
        grid.set_soft_wrapped_from_previous(2, true);
        grid.goto(1, cursor_col);

        grid.erase_above();

        assert_eq!(grid.row(2).soft_wrapped_from_previous(), wrap_kept, "col={cursor_col}");
    }
}

/// ICH and DCH rewrite a row's last column and ECH through it erases that column, so each revokes
/// the wrap a non-final row passes to the next row; the row keeps its own incoming wrap.
#[test]
fn cell_shifts_and_erases_on_a_wrapping_row_revoke_its_outgoing_wrap() {
    for operation in 0..4 {
        let mut grid = Grid::new(4, 3);
        grid.set_soft_wrapped_from_previous(1, true);
        grid.set_soft_wrapped_from_previous(2, true);
        match operation {
            0 => grid.insert_cells(1, 1, 1),
            1 => grid.delete_cells(1, 1, 1),
            2 => grid.erase_cells(1, 2, 2),
            _ => grid.erase_cells(1, 0, 1),
        }
        assert!(grid.row(1).soft_wrapped_from_previous(), "operation={operation}");
        // Only the ECH that stops before the last column keeps the outgoing wrap.
        assert_eq!(
            grid.row(2).soft_wrapped_from_previous(),
            operation == 3,
            "operation={operation}"
        );
    }
}

// ---------------------------------------------------------------------------
// Trimmed history rows and the eject path
// ---------------------------------------------------------------------------
