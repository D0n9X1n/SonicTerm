//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::grid::Grid;

#[test]
fn exports_grid_constructor() {
    let grid = Grid::new(4, 2);
    assert_eq!(grid.cols, 4);
    assert_eq!(grid.rows, 2);
    assert!(grid.is_row_dirty(0));
}
