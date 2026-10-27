use super::Grid;

pub(super) fn text(grid: &Grid, row: u16) -> String {
    grid.row(row).iter().map(|cell| cell.ch).collect()
}

pub(super) fn dirty_rows_vec(grid: &Grid) -> Vec<usize> {
    grid.dirty_rows().collect()
}

/// Source pins cover every production split; grid keeps its legacy test marker last for the writer parser.
pub(super) const GRID_SOURCE: &str = concat!(
    include_str!("grid_dirty.rs"),
    "\n",
    include_str!("grid_screens.rs"),
    "\n",
    include_str!("grid_write.rs"),
    "\n",
    include_str!("grid_scroll.rs"),
    "\n",
    include_str!("grid_erase.rs"),
    "\n",
    include_str!("grid_history.rs"),
    "\n",
    include_str!("grid_retention.rs"),
    "\n",
    include_str!("grid.rs"),
);
