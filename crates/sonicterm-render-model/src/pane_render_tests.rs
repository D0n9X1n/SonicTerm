//! Pins the frame-source and acknowledgement-receipt contract: a borrowed source lends its panes
//! once, and a receipt clears only the rows it names, and only from the grid it was taken from.

use sonicterm_grid::grid::{Grid, RowSet};

use super::*;

/// The grid's dirty rows, in order.
fn dirty(grid: &Grid) -> Vec<usize> {
    grid.dirty_rows().collect()
}

#[test]
fn a_borrowed_source_lends_its_panes_once_and_returns_the_closures_result() {
    // Lending hands the caller's slice to the closure, whose result comes back unchanged.
    let mut panes: [PaneRender<'_>; 0] = [];
    let lent = BorrowedSource(&mut panes).lend(|lent| lent.len() + 7);
    assert_eq!(lent, 7);
}

#[test]
fn a_receipt_records_the_grids_identities_when_it_is_taken() {
    // Every identity is read from the grid as it is at assembly, with the pane's place and id.
    let grid = Grid::new(4, 3);
    let receipt = AckReceipt::of(2, 9, &grid, AckRows::All);
    assert_eq!((receipt.index, receipt.pane_id), (2, 9));
    assert_eq!(receipt.revision, grid.revision());
    assert_eq!(receipt.dirty_generation, grid.dirty_generation());
    assert_eq!(receipt.size_generation, grid.size_generation());
    assert_eq!(receipt.screen_epoch, grid.screen_epoch());
    assert!(receipt.matches(&grid));
}

#[test]
fn a_matching_receipt_clears_exactly_its_rows_or_every_row() {
    // A row receipt clears the rows it names and keeps the others; an all-rows receipt clears
    // every row. A fresh grid starts fully dirty.
    let mut grid = Grid::new(4, 3);
    assert_eq!(dirty(&grid), vec![0, 1, 2]);
    let mut rows = RowSet::new();
    rows.insert(1);
    assert!(AckReceipt::of(0, 1, &grid, AckRows::Rows(rows)).try_apply(&mut grid));
    assert_eq!(dirty(&grid), vec![0, 2]);
    assert!(AckReceipt::of(0, 1, &grid, AckRows::All).try_apply(&mut grid));
    assert!(dirty(&grid).is_empty());
}

#[test]
fn a_receipt_whose_grid_moved_since_assembly_clears_nothing() {
    // Dirt written after assembly advances the dirty generation, so the receipt no longer matches
    // and that dirt stays for the next frame.
    let mut grid = Grid::new(4, 3);
    let receipt = AckReceipt::of(0, 1, &grid, AckRows::All);
    grid.mark_all_dirty();
    assert!(!receipt.matches(&grid));
    assert!(!receipt.try_apply(&mut grid));
    assert_eq!(dirty(&grid), vec![0, 1, 2]);
}
