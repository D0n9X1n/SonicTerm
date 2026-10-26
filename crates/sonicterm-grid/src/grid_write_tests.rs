use super::*;
use crate::grid::grid_test_support::*;

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
fn autowrap_off_overwrites_right_edge() {
    let mut grid = Grid::new(4, 1);
    grid.set_autowrap(false);
    for character in "abcdef".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }

    assert_eq!(text(&grid, 0), "abcf");
    assert_eq!(grid.cursor.col, 3);
    assert!(!grid.pending_wrap());
}

#[test]
fn pending_wrap_is_cleared_by_cursor_motion() {
    let mut grid = Grid::new(4, 2);
    for character in "abcd".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert!(grid.pending_wrap());

    grid.goto(0, 1);
    grid.put_char('Z', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(text(&grid, 0), "aZcd");
    assert_eq!(text(&grid, 1), "    ");
}

/// A predecessor edit revokes the successor's wrap only when it changes the predecessor's last column.
#[test]
fn predecessor_last_column_edit_revokes_successor_wrap_provenance() {
    let mut grid = Grid::new(4, 3);
    grid.set_soft_wrapped_from_previous(1, true);

    // An interior edit, such as a shell recoloring its command word, keeps the continuation.
    grid.goto(0, 0);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    assert!(grid.row(1).soft_wrapped_from_previous());

    // Only the last column carries the wrap, as in xterm and WezTerm.
    grid.goto(0, 3);
    grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
    assert!(!grid.row(1).soft_wrapped_from_previous());
}

/// Start-of-row edits keep the row's incoming wrap; only an arbitrary `row_mut` borrow revokes it.
#[test]
fn start_of_row_mutations_keep_incoming_wrap_provenance() {
    let mut wide = Grid::new(4, 2);
    wide.goto(1, 0);
    wide.put_char('界', Color::Default, Color::Default, CellFlags::empty());
    wide.set_soft_wrapped_from_previous(1, true);
    wide.goto(1, 1);
    wide.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    assert!(wide.row(1).soft_wrapped_from_previous());

    let mut insertion = Grid::new(4, 2);
    insertion.goto(1, 0);
    insertion.put_char('界', Color::Default, Color::Default, CellFlags::empty());
    insertion.set_soft_wrapped_from_previous(1, true);
    insertion.insert_cells(1, 1, 1);
    assert!(insertion.row(1).soft_wrapped_from_previous());

    let mut combining = Grid::new(4, 2);
    combining.goto(1, 0);
    combining.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    combining.set_soft_wrapped_from_previous(1, true);
    combining.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
    assert!(combining.row(1).soft_wrapped_from_previous());

    for operation in [0, 1, 2, 3, 4] {
        let mut grid = Grid::new(4, 2);
        grid.set_soft_wrapped_from_previous(1, true);
        match operation {
            0 => {
                grid.goto(1, 0);
                grid.erase_line_to_start();
            }
            1 => grid.erase_cells(1, 0, 1),
            2 => grid.insert_cells(1, 0, 1),
            3 => grid.delete_cells(1, 0, 1),
            _ => {
                grid.row_mut(1)[0] = Cell::default();
            }
        }
        // `row_mut` hands out arbitrary access, so it alone conservatively revokes the boundary.
        assert_eq!(
            grid.row(1).soft_wrapped_from_previous(),
            operation != 4,
            "operation={operation}"
        );
    }
}

/// Interior edits keep both wrap boundaries; a last-column edit revokes only the outgoing one.
#[test]
fn interior_edit_keeps_wraps_and_last_column_edit_revokes_outgoing() {
    let mut grid = Grid::new(4, 3);
    grid.set_soft_wrapped_from_previous(1, true);
    grid.set_soft_wrapped_from_previous(2, true);

    grid.goto(1, 2);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    assert!(grid.row(1).soft_wrapped_from_previous());
    assert!(grid.row(2).soft_wrapped_from_previous());

    grid.goto(1, 3);
    grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
    assert!(grid.row(1).soft_wrapped_from_previous());
    assert!(!grid.row(2).soft_wrapped_from_previous());
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
