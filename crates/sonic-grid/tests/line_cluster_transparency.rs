//! Regression tests for Haiku review on #381: Line iter/Hash/range
//! access MUST be cluster-transparent (no panic via `as_flat_slice` →
//! `as_vec` when storage is Cluster). Without this, PR-C (which begins
//! actually producing Cluster lines from scrollback eject) would
//! panic every downstream call site that iterates, hashes, or ranges
//! a row.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sonic_grid::line::{Cluster, Line, LineIter};
use sonic_types::cell::{Cell, CellFlags, Color};

fn a() -> Cell {
    Cell::plain('A', Color::Default, Color::Default, CellFlags::empty())
}

fn b() -> Cell {
    Cell::plain('B', Color::Default, Color::Default, CellFlags::empty())
}

fn uniform_cluster_a_x10() -> Line {
    Line::from_clusters(vec![Cluster { cell: a(), count: 10 }])
}

fn flat_equivalent() -> Line {
    Line::from_flat(vec![a(); 10])
}

#[test]
fn iter_on_cluster_does_not_panic_and_yields_correct_count() {
    let line = uniform_cluster_a_x10();
    assert!(line.is_clustered());
    let count = line.iter().count();
    assert_eq!(count, 10);
}

#[test]
fn iter_on_cluster_yields_correct_cells() {
    let line = uniform_cluster_a_x10();
    let collected: Vec<_> = line.iter().cloned().collect();
    assert_eq!(collected, vec![a(); 10]);
}

#[test]
fn iter_double_ended_on_cluster() {
    let line = uniform_cluster_a_x10();
    // rev().next() returns last element — equivalent to get(len-1).
    let last_via_rev = line.iter().next_back().cloned();
    let last_via_get = line.get(9).cloned();
    assert_eq!(last_via_rev, last_via_get);
    assert_eq!(last_via_rev, Some(a()));
}

#[test]
fn iter_exact_size_on_cluster() {
    let line = uniform_cluster_a_x10();
    let it = line.iter();
    assert_eq!(it.len(), 10);
    assert_eq!(it.size_hint(), (10, Some(10)));
}

#[test]
fn iter_double_ended_walks_full_line_from_both_ends() {
    // Mixed cluster line: 3 A, 2 B, 5 A
    let line = Line::from_clusters(vec![
        Cluster { cell: a(), count: 3 },
        Cluster { cell: b(), count: 2 },
        Cluster { cell: a(), count: 5 },
    ]);
    assert_eq!(line.len(), 10);

    let mut it = line.iter();
    // Take from both ends alternately.
    let mut front = Vec::new();
    let mut back = Vec::new();
    #[allow(clippy::while_let_loop)] // intentional bidirectional drain
    loop {
        let Some(c) = it.next() else { break };
        front.push(c.clone());
        let Some(c) = it.next_back() else { break };
        back.push(c.clone());
    }
    // Drain any remaining (odd count).
    for c in it.by_ref() {
        front.push(c.clone());
    }
    while let Some(c) = it.next_back() {
        back.push(c.clone());
    }
    back.reverse();
    let mut combined = front;
    combined.extend(back);

    let expected: Vec<Cell> = std::iter::repeat_n(a(), 3)
        .chain(std::iter::repeat_n(b(), 2))
        .chain(std::iter::repeat_n(a(), 5))
        .collect();
    assert_eq!(combined, expected);
}

#[test]
fn get_range_on_cluster_window() {
    let line = uniform_cluster_a_x10();
    let v: Vec<_> = line.get_range(2, 5).cloned().collect();
    assert_eq!(v.len(), 3);
    assert!(v.iter().all(|c| *c == a()));
}

#[test]
fn get_range_window_crosses_cluster_boundary() {
    // 3 A, 4 B, 3 A — range (2, 8) should yield A B B B B A
    let line = Line::from_clusters(vec![
        Cluster { cell: a(), count: 3 },
        Cluster { cell: b(), count: 4 },
        Cluster { cell: a(), count: 3 },
    ]);
    let v: Vec<_> = line.get_range(2, 8).cloned().collect();
    assert_eq!(v, vec![a(), b(), b(), b(), b(), a()]);
}

#[test]
fn get_range_window_inside_single_cluster() {
    let line = uniform_cluster_a_x10();
    let v: Vec<_> = line.get_range(4, 7).cloned().collect();
    assert_eq!(v, vec![a(), a(), a()]);
}

#[test]
fn get_range_clamps_end_and_handles_empty() {
    let line = uniform_cluster_a_x10();
    assert_eq!(line.get_range(0, 100).count(), 10);
    assert_eq!(line.get_range(5, 3).count(), 0);
    assert_eq!(line.get_range(10, 20).count(), 0);
}

#[test]
fn get_range_on_flat_matches_cluster() {
    let flat = Line::from_flat(
        std::iter::repeat_n(a(), 3)
            .chain(std::iter::repeat_n(b(), 4))
            .chain(std::iter::repeat_n(a(), 3))
            .collect(),
    );
    let cluster = Line::from_clusters(vec![
        Cluster { cell: a(), count: 3 },
        Cluster { cell: b(), count: 4 },
        Cluster { cell: a(), count: 3 },
    ]);
    for (s, e) in [(0, 10), (0, 3), (3, 7), (2, 8), (7, 10), (4, 6)] {
        let f: Vec<_> = flat.get_range(s, e).cloned().collect();
        let c: Vec<_> = cluster.get_range(s, e).cloned().collect();
        assert_eq!(f, c, "range ({s}, {e}) mismatch");
    }
}

fn hash_of(line: &Line) -> u64 {
    let mut h = DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
}

#[test]
fn equal_content_flat_and_cluster_hash_identically() {
    let flat = flat_equivalent();
    let cluster = uniform_cluster_a_x10();
    assert_eq!(flat.len(), cluster.len());
    assert_eq!(hash_of(&flat), hash_of(&cluster));
}

#[test]
fn hash_differs_for_different_content() {
    let line_a = uniform_cluster_a_x10();
    let line_b = Line::from_clusters(vec![Cluster { cell: b(), count: 10 }]);
    assert_ne!(hash_of(&line_a), hash_of(&line_b));
}

#[test]
fn hash_consistent_after_compact() {
    // Build flat, hash it, compact it, hash again — must match.
    let mut line = Line::from_flat(vec![a(); 200]);
    let pre = hash_of(&line);
    let changed = line.compact_if_beneficial();
    assert!(changed, "200 identical cells should compact");
    assert!(line.is_clustered());
    let post = hash_of(&line);
    assert_eq!(pre, post, "compaction must not change the hash");
}

#[test]
fn index_single_cell_on_cluster_does_not_panic() {
    let line = uniform_cluster_a_x10();
    assert_eq!(line[0], a());
    assert_eq!(line[9], a());
}

#[test]
fn into_iter_for_ref_line_works_on_cluster() {
    let line = uniform_cluster_a_x10();
    let mut n = 0;
    for cell in &line {
        assert_eq!(*cell, a());
        n += 1;
    }
    assert_eq!(n, 10);
}

#[test]
fn line_iter_enum_variants_are_constructible() {
    // Smoke test that the LineIter type itself is the right shape.
    let line = flat_equivalent();
    let it = line.iter();
    let _: LineIter<'_> = it;
}
