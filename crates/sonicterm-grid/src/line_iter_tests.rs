use crate::line::line_test_support::*;
use crate::line::*;

#[test]
fn iteration_and_random_access_agree_across_forms() {
    let (flat, clustered) = flat_and_cluster(&[
        (blank(), 10),
        (plain_cell('x'), 1),
        (blank(), 5),
        (plain_cell('x'), 2),
        (blank(), 50),
    ]);
    assert!(!flat.is_clustered());
    assert!(clustered.is_clustered());
    assert_eq!(flat.len(), clustered.len());

    let flat_iter: Vec<_> = flat.iter().cloned().collect();
    let clust_iter: Vec<_> = clustered.iter_storage().cloned().collect();
    assert_eq!(flat_iter, clust_iter);

    for column in 0..flat.len() {
        assert_eq!(flat.get(column), clustered.get(column), "mismatch at {column}");
    }
}

#[test]
fn get_range_matches_across_forms_and_clamps() {
    let (flat, clustered) =
        flat_and_cluster(&[(plain_cell('a'), 3), (plain_cell('b'), 4), (plain_cell('c'), 3)]);

    // Interior window straddling cluster boundaries.
    let flat_cells: Vec<_> = flat.get_range(2, 8).cloned().collect();
    let cluster_cells: Vec<_> = clustered.get_range(2, 8).cloned().collect();
    assert_eq!(flat_cells, cluster_cells);
    assert_eq!(
        flat_cells,
        vec![
            plain_cell('a'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('c')
        ]
    );

    // end clamps to len(); start==end and reversed ranges are empty.
    assert_eq!(clustered.get_range(8, 999).count(), 2);
    assert_eq!(clustered.get_range(4, 4).count(), 0);
    assert_eq!(clustered.get_range(6, 2).count(), 0);
    assert_eq!(flat.get_range(6, 2).count(), 0);
}

#[test]
fn get_range_window_inside_single_cluster() {
    let clustered = Line::from_clusters(vec![Cluster { cell: plain_cell('z'), count: 20 }]);
    let got: Vec<_> = clustered.get_range(5, 9).cloned().collect();
    assert_eq!(got, vec![plain_cell('z'); 4]);
}

#[test]
fn reverse_iteration_agrees_across_forms() {
    let (flat, clustered) =
        flat_and_cluster(&[(plain_cell('a'), 3), (plain_cell('b'), 1), (plain_cell('c'), 4)]);
    let fwd: Vec<_> = flat.iter().cloned().collect();

    let flat_rev: Vec<_> = flat.iter().rev().cloned().collect();
    let clust_rev: Vec<_> = clustered.iter().rev().cloned().collect();
    assert_eq!(flat_rev, clust_rev);

    let mut expect_rev = fwd.clone();
    expect_rev.reverse();
    assert_eq!(clust_rev, expect_rev);
}

#[test]
fn double_ended_meet_in_middle_on_cluster() {
    // Alternate popping from front and back; the two ends must meet without
    // yielding a cell twice or skipping one.
    let clustered = Line::from_clusters(vec![
        Cluster { cell: plain_cell('a'), count: 2 },
        Cluster { cell: plain_cell('b'), count: 3 },
        Cluster { cell: plain_cell('c'), count: 2 },
    ]);
    let mut it = clustered.iter();
    let mut front = Vec::new();
    let mut back = Vec::new();
    while let Some(front_cell) = it.next() {
        front.push(front_cell.clone());
        if let Some(back_cell) = it.next_back() {
            back.push(back_cell.clone());
        }
    }
    back.reverse();
    front.extend(back);
    assert_eq!(front, clustered.to_vec());
    assert_eq!(front.len(), 7);
}

#[test]
fn exact_size_hint_tracks_remaining() {
    let clustered = Line::from_clusters(vec![
        Cluster { cell: plain_cell('a'), count: 4 },
        Cluster { cell: plain_cell('b'), count: 2 },
    ]);
    let mut it = clustered.iter();
    assert_eq!(it.len(), 6);
    assert_eq!(it.size_hint(), (6, Some(6)));
    it.next();
    it.next_back();
    assert_eq!(it.len(), 4);
    assert_eq!(it.size_hint(), (4, Some(4)));
}
