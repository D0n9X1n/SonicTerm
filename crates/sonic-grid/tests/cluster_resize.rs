//! PR-E (#319): Grid::resize and Line::resize preserve Cluster
//! compression on the scrollback. Visible rows are always Flat (per
//! PR-C invariant); only scrollback exercises Cluster paths here.

use sonic_grid::grid::{Cell, Grid};
use sonic_grid::line::{Cluster, Line, LineStorage};

fn ensure_uniform_blank_scrollback(g: &mut Grid, n: usize) {
    for _ in 0..n {
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), n);
    for r in 0..n {
        assert!(
            g.scrollback_row(r).expect("in range").is_clustered(),
            "scrollback row {r} should start Clustered"
        );
    }
}

#[test]
fn cols_increase_keeps_scrollback_cluster() {
    let mut g = Grid::new(80, 24);
    ensure_uniform_blank_scrollback(&mut g, 50);

    g.resize(100, 24);
    assert_eq!(g.scrollback_len(), 50);
    for r in 0..50 {
        let row = g.scrollback_row(r).expect("row in range");
        assert!(row.is_clustered(), "row {r} degraded after grow");
        assert_eq!(row.len(), 100, "row {r} not padded to new cols");
        // Trailing padding cell equals default blank — same as head.
        assert_eq!(row.get(0).cloned(), Some(Cell::default()));
        assert_eq!(row.get(99).cloned(), Some(Cell::default()));
    }
}

#[test]
fn cols_decrease_keeps_scrollback_cluster() {
    let mut g = Grid::new(80, 24);
    ensure_uniform_blank_scrollback(&mut g, 50);

    g.resize(60, 24);
    assert_eq!(g.scrollback_len(), 50);
    for r in 0..50 {
        let row = g.scrollback_row(r).expect("row in range");
        assert!(row.is_clustered(), "row {r} degraded after shrink");
        assert_eq!(row.len(), 60);
    }
}

#[test]
fn shrink_then_grow_back_stays_cluster() {
    let mut g = Grid::new(80, 24);
    ensure_uniform_blank_scrollback(&mut g, 30);

    g.resize(60, 24);
    g.resize(80, 24);

    assert_eq!(g.scrollback_len(), 30);
    for r in 0..30 {
        let row = g.scrollback_row(r).expect("row in range");
        assert!(row.is_clustered(), "row {r} degraded across shrink+grow");
        assert_eq!(row.len(), 80);
    }
}

#[test]
fn line_resize_grow_matching_fill_stays_single_cluster() {
    let blank = Cell::default();
    let mut l = Line::from_clusters(vec![Cluster { cell: blank.clone(), count: 80 }]);
    l.resize(100, blank.clone());
    assert!(l.is_clustered());
    assert_eq!(l.len(), 100);
    if let LineStorage::Cluster(cs) = l.storage() {
        assert_eq!(cs.len(), 1, "matching fill should merge into one cluster");
        assert_eq!(cs[0].count, 100);
    } else {
        panic!("expected cluster");
    }
}

#[test]
fn line_resize_grow_mismatched_fill_appends_second_cluster() {
    use sonic_types::cell::Color;
    let mut red = Cell::default();
    red.bg = Color::Indexed(1);
    let blank = Cell::default();
    let mut l = Line::from_clusters(vec![Cluster { cell: red.clone(), count: 80 }]);
    l.resize(100, blank.clone());
    assert!(l.is_clustered(), "should stay clustered (multi-cluster)");
    assert_eq!(l.len(), 100);
    if let LineStorage::Cluster(cs) = l.storage() {
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].cell, red);
        assert_eq!(cs[0].count, 80);
        assert_eq!(cs[1].cell, blank);
        assert_eq!(cs[1].count, 20);
    } else {
        panic!("expected cluster");
    }
}

#[test]
fn line_truncate_keeps_cluster() {
    let blank = Cell::default();
    let mut l = Line::from_clusters(vec![Cluster { cell: blank.clone(), count: 80 }]);
    l.truncate(50);
    assert!(l.is_clustered());
    assert_eq!(l.len(), 50);
    if let LineStorage::Cluster(cs) = l.storage() {
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].count, 50);
    } else {
        panic!("expected cluster");
    }
}

#[test]
fn line_truncate_across_multiple_clusters() {
    use sonic_types::cell::Color;
    let mut red = Cell::default();
    red.bg = Color::Indexed(1);
    let blank = Cell::default();
    let mut l = Line::from_clusters(vec![
        Cluster { cell: red.clone(), count: 30 },
        Cluster { cell: blank.clone(), count: 50 },
    ]);
    // Truncate inside the second cluster.
    l.truncate(40);
    assert!(l.is_clustered());
    assert_eq!(l.len(), 40);
    if let LineStorage::Cluster(cs) = l.storage() {
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].count, 30);
        assert_eq!(cs[1].count, 10);
    } else {
        panic!("expected cluster");
    }
    // Truncate to drop the second cluster entirely.
    l.truncate(20);
    if let LineStorage::Cluster(cs) = l.storage() {
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].cell, red);
        assert_eq!(cs[0].count, 20);
    } else {
        panic!("expected cluster");
    }
}

#[test]
fn resize_with_mixed_visible_and_cluster_scrollback() {
    let mut g = Grid::new(80, 24);
    // Put something non-uniform in visible row 0.
    g.row_mut(0).set(0, {
        let mut c = Cell::default();
        c.ch = 'x';
        c
    });
    // Then push 24 lines into scrollback (first ejects the 'x' row,
    // remaining 23 are uniform blanks → Cluster).
    for _ in 0..24 {
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), 24);

    g.resize(90, 24);
    assert_eq!(g.cols, 90);
    // Row 0 (the 'x' row) was Flat; row 1+ were Cluster.
    assert_eq!(g.scrollback_row(0).unwrap().len(), 90);
    for r in 1..24 {
        let row = g.scrollback_row(r).expect("row in range");
        assert!(row.is_clustered(), "scrollback row {r} should stay clustered");
        assert_eq!(row.len(), 90);
    }
    // Visible row 0 has the non-uniform 'x' — it's Flat already; just
    // confirm width.
    assert_eq!(g.row(0).len(), 90);
}
