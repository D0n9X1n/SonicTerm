//! PR-C (#319): after scroll_up ejects a uniform line into scrollback
//! as Cluster form, reads through the canonical scrollback iterator
//! return the same cells as a control grid that never compressed.
//!
//! This is the render-adjacent smoke: PR-B2 (#381) already made
//! `Line::iter` / `Hash` cluster-transparent. PR-C just produces the
//! Cluster lines for the first time, so we re-verify equivalence end-
//! to-end here so any future regression in the render path's read
//! lands a failure in this crate, not at runtime.

use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid};

fn build(cols: u16, rows: u16, n_scrolls: u16) -> Grid {
    let mut g = Grid::new(cols, rows);
    for _ in 0..n_scrolls {
        g.scroll_up(1);
    }
    g
}

#[test]
fn cluster_scrollback_iter_matches_flat_control() {
    let cols: u16 = 80;
    let rows: u16 = 24;
    let n: u16 = 50;
    let g_cluster = build(cols, rows, n);

    // Control: build the same shape but force every line back to Flat
    // by mutating-then-restoring each cell (touches iter_mut which
    // calls degrade_to_flat).
    // Control: a directly-built Flat line of the same uniform content.
    let control_flat = sonicterm_grid::line::Line::flat_filled(cols as usize, Cell::default());
    let expected: Vec<Cell> = control_flat.iter().cloned().collect();

    assert_eq!(g_cluster.scrollback_len(), n as usize);
    for r in 0..g_cluster.scrollback_len() {
        let c_row = g_cluster.scrollback_row(r).expect("cluster row");
        assert_eq!(c_row.len(), expected.len());
        let c: Vec<Cell> = c_row.iter().cloned().collect();
        assert_eq!(c, expected, "row {r} cluster iter != flat control");
    }
}

#[test]
fn cluster_scrollback_hash_matches_flat_equivalent() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let cols: u16 = 40;
    let rows: u16 = 10;
    let mut g = Grid::new(cols, rows);
    g.scroll_up(1);
    let cluster_row = g.scrollback_row(0).expect("ejected uniform line");
    assert!(cluster_row.is_clustered());

    // Build an equivalent Flat line and compare hash.
    let flat = sonicterm_grid::line::Line::flat_filled(cols as usize, Cell::default());

    let mut h1 = DefaultHasher::new();
    cluster_row.hash(&mut h1);
    let mut h2 = DefaultHasher::new();
    flat.hash(&mut h2);
    assert_eq!(h1.finish(), h2.finish(), "Cluster/Flat hashes must match");
    let _ = CellFlags::empty();
    let _ = Color::Default;
}
