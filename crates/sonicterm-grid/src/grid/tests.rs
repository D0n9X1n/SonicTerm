use super::*;

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
    // #762: inserting blank cells must shift the EXISTING text right intact,
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
