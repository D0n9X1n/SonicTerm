use sonicterm_grid::grid::Grid;

#[test]
fn exports_grid_constructor() {
    let grid = Grid::new(4, 2);
    assert_eq!(grid.cols, 4);
    assert_eq!(grid.rows, 2);
    assert!(grid.is_row_dirty(0));
}
