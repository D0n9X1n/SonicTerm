use super::*;

#[test]
fn bounded_grid_size_caps_axes_and_total_cells() {
    let (cols, rows) = bounded_grid_size(u64::MAX, u64::MAX);

    assert!(cols <= MAX_GRID_AXIS);
    assert!(rows <= MAX_GRID_AXIS);
    assert!(u32::from(cols) * u32::from(rows) <= MAX_GRID_CELLS);
}
