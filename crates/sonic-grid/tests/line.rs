//! Tests for `sonic_grid::line::Line` cluster compression (Epic #300, P5).

use sonic_grid::line::{Cluster, Line, LineStorage};
use sonic_types::cell::{Cell, CellFlags, Color};

fn blank() -> Cell {
    Cell::plain(' ', Color::Default, Color::Default, CellFlags::empty())
}

fn x() -> Cell {
    Cell::plain('x', Color::Default, Color::Default, CellFlags::empty())
}

#[test]
fn cluster_uses_less_memory_than_flat_for_uniform_line() {
    let cells = vec![blank(); 200];
    let flat = Line::from_flat(cells.clone());
    let mut clustered = Line::from_flat(cells);
    let changed = clustered.compact_if_beneficial();

    assert!(changed, "200 identical cells should compact");
    assert!(clustered.is_clustered());
    assert!(
        clustered.approx_byte_size() * 4 <= flat.approx_byte_size(),
        "cluster ({}) must be at least 4× smaller than flat ({})",
        clustered.approx_byte_size(),
        flat.approx_byte_size()
    );
    assert_eq!(clustered.len(), 200);
}

#[test]
fn iteration_results_identical_across_storage_forms() {
    // Mixed pattern: 10 blanks, 1 x, 5 blanks, 2 x, 50 blanks
    let mut cells = Vec::new();
    cells.extend(std::iter::repeat_n(blank(), 10));
    cells.push(x());
    cells.extend(std::iter::repeat_n(blank(), 5));
    cells.extend(std::iter::repeat_n(x(), 2));
    cells.extend(std::iter::repeat_n(blank(), 50));

    let flat = Line::from_flat(cells.clone());
    let clustered = Line::from_clusters(vec![
        Cluster { cell: blank(), count: 10 },
        Cluster { cell: x(), count: 1 },
        Cluster { cell: blank(), count: 5 },
        Cluster { cell: x(), count: 2 },
        Cluster { cell: blank(), count: 50 },
    ]);

    assert_eq!(flat.len(), clustered.len());
    let flat_iter: Vec<_> = flat.iter().cloned().collect();
    let clust_iter: Vec<_> = clustered.iter_storage().cloned().collect();
    assert_eq!(flat_iter, clust_iter);
    assert_eq!(flat_iter, cells);

    // Random access also agrees
    for i in 0..flat.len() {
        assert_eq!(flat.get(i), clustered.get(i), "mismatch at {i}");
    }

    // cluster_from_flat must round-trip
    let rebuilt = LineStorage::cluster_from_flat(&cells);
    let rebuilt_line = Line::from_clusters(match rebuilt {
        LineStorage::Cluster(c) => c,
        _ => unreachable!(),
    });
    let rebuilt_iter: Vec<_> = rebuilt_line.iter_storage().cloned().collect();
    assert_eq!(rebuilt_iter, cells);
}

#[test]
fn edit_degrades_cluster_to_flat() {
    let mut line = Line::from_clusters(vec![Cluster { cell: blank(), count: 100 }]);
    assert!(line.is_clustered());

    let ok = line.set(42, x());
    assert!(ok);
    assert!(!line.is_clustered(), "set() must degrade to Flat");
    assert_eq!(line.len(), 100);
    assert_eq!(line.get(41), Some(&blank()));
    assert_eq!(line.get(42), Some(&x()));
    assert_eq!(line.get(43), Some(&blank()));

    // Further iteration still correct
    let collected: Vec<_> = line.iter().cloned().collect();
    assert_eq!(collected.len(), 100);
    assert_eq!(collected[42], x());
    assert_eq!(collected.iter().filter(|c| **c == x()).count(), 1);

    // Out-of-range set returns false, no panic
    assert!(!line.set(1000, blank()));
}
