
use super::*;
use sonicterm_grid::line::{Cluster, Line};

#[test]
fn row_quad_hash_cells_accepts_cluster_storage() {
    let line = Line::from_clusters(vec![
        Cluster {
            cell: Cell::plain('a', Default::default(), Default::default(), Default::default()),
            count: 2,
        },
        Cluster {
            cell: Cell::plain('b', Default::default(), Default::default(), Default::default()),
            count: 1,
        },
    ]);
    let hash = row_quad_hash_cells(0, 0, line.iter(), 1, 10.0, 20.0, 0.0, 0.0, 100.0, 20.0, None);
    assert_ne!(hash, 0);
}
