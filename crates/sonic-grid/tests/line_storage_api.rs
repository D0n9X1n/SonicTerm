//! PR-A tests for `LineStorage` API completeness (#319).
//!
//! Covers every additive method on the primitive: Flat/Cluster preservation,
//! auto-degradation on mutation, random-access iteration parity, and edges
//! (empty, single, large).

use sonic_grid::line::{Cluster, LineStorage};
use sonic_types::cell::{Cell, CellFlags, Color};

fn cell_with_ch(ch: char) -> Cell {
    let mut c = Cell::default();
    c.ch = ch;
    c
}

fn flat_of(s: &str) -> LineStorage {
    LineStorage::Flat(s.chars().map(cell_with_ch).collect())
}

fn cluster_uniform(ch: char, n: usize) -> LineStorage {
    LineStorage::Cluster(vec![Cluster { cell: cell_with_ch(ch), count: n }])
}

#[test]
fn len_is_empty_flat_and_cluster() {
    assert_eq!(flat_of("").len(), 0);
    assert!(flat_of("").is_empty());
    assert_eq!(flat_of("abc").len(), 3);
    assert!(!flat_of("abc").is_empty());

    assert_eq!(cluster_uniform(' ', 80).len(), 80);
    assert!(!cluster_uniform(' ', 80).is_empty());
    assert!(LineStorage::Cluster(vec![]).is_empty());
}

#[test]
fn is_cluster_is_flat() {
    assert!(flat_of("xy").is_flat());
    assert!(!flat_of("xy").is_cluster());
    assert!(cluster_uniform('a', 5).is_cluster());
    assert!(!cluster_uniform('a', 5).is_flat());
}

#[test]
fn get_flat_in_and_out_of_range() {
    let s = flat_of("abc");
    assert_eq!(s.get(0).unwrap().ch, 'a');
    assert_eq!(s.get(2).unwrap().ch, 'c');
    assert!(s.get(3).is_none());
}

#[test]
fn get_cluster_crosses_runs() {
    let s = LineStorage::Cluster(vec![
        Cluster { cell: cell_with_ch('a'), count: 3 },
        Cluster { cell: cell_with_ch('b'), count: 2 },
    ]);
    assert_eq!(s.get(0).unwrap().ch, 'a');
    assert_eq!(s.get(2).unwrap().ch, 'a');
    assert_eq!(s.get(3).unwrap().ch, 'b');
    assert_eq!(s.get(4).unwrap().ch, 'b');
    assert!(s.get(5).is_none());
}

#[test]
fn get_range_basic_flat() {
    let s = LineStorage::Flat((0..10).map(|idx| cell_with_ch(char::from(b'0' + idx))).collect());
    let chars: Vec<char> = s.get_range(2, 5).map(|cell| cell.ch).collect();
    assert_eq!(chars, vec!['2', '3', '4']);
}

#[test]
fn get_range_basic_cluster() {
    let cell = Cell::plain('A', Color::Rgb(1, 2, 3), Color::Indexed(4), CellFlags::BOLD);
    let s = LineStorage::Cluster(vec![Cluster { cell: cell.clone(), count: 10 }]);
    let cells: Vec<Cell> = s.get_range(3, 7).collect();

    assert_eq!(cells.len(), 4);
    assert!(cells.iter().all(|actual| actual == &cell));
}

#[test]
fn get_range_edge_cases() {
    let s = flat_of("abcd");
    assert!(s.get_range(3, 2).collect::<Vec<_>>().is_empty());
    assert!(s.get_range(5, 6).collect::<Vec<_>>().is_empty());
    assert!(s.get_range(2, 2).collect::<Vec<_>>().is_empty());
    assert_eq!(s.get_range(2, 99).map(|cell| cell.ch).collect::<Vec<_>>(), vec!['c', 'd']);

    let c = cluster_uniform('x', 4);
    assert!(c.get_range(1, 1).collect::<Vec<_>>().is_empty());
    assert!(c.get_range(9, 10).collect::<Vec<_>>().is_empty());
}

#[test]
fn set_on_flat_preserves_flat() {
    let mut s = flat_of("aaa");
    assert!(s.set(1, cell_with_ch('Z')));
    assert!(s.is_flat());
    assert_eq!(s.get(1).unwrap().ch, 'Z');
    assert!(!s.set(99, cell_with_ch('X')));
}

#[test]
fn set_on_cluster_degrades_to_flat() {
    let mut s = cluster_uniform('a', 4);
    assert!(s.is_cluster());
    s.set(2, cell_with_ch('Z'));
    assert!(s.is_flat());
    let collected: Vec<char> = s.iter().map(|c| c.ch).collect();
    assert_eq!(collected, vec!['a', 'a', 'Z', 'a']);
}

#[test]
fn push_extends_right_and_degrades_cluster() {
    let mut s = flat_of("ab");
    s.push(cell_with_ch('c'));
    assert_eq!(s.len(), 3);
    assert_eq!(s.get(2).unwrap().ch, 'c');

    let mut c = cluster_uniform(' ', 2);
    c.push(cell_with_ch('X'));
    assert!(c.is_flat());
    assert_eq!(c.len(), 3);
    assert_eq!(c.get(2).unwrap().ch, 'X');
}

#[test]
fn truncate_flat_and_cluster_preserve_form() {
    let mut s = flat_of("abcdef");
    s.truncate(3);
    assert!(s.is_flat());
    assert_eq!(s.len(), 3);
    assert_eq!(s.get(2).unwrap().ch, 'c');

    let mut c = LineStorage::Cluster(vec![
        Cluster { cell: cell_with_ch('a'), count: 3 },
        Cluster { cell: cell_with_ch('b'), count: 5 },
    ]);
    c.truncate(5);
    assert!(c.is_cluster());
    assert_eq!(c.len(), 5);
    assert_eq!(c.get(4).unwrap().ch, 'b');

    // No-op when new_len >= current
    let mut s2 = flat_of("xy");
    s2.truncate(10);
    assert_eq!(s2.len(), 2);
}

#[test]
fn resize_grow_and_shrink_keep_cluster_when_matching_fill() {
    // Shrink: delegates to truncate.
    let mut s = flat_of("abcdef");
    s.resize(2, cell_with_ch(' '));
    assert_eq!(s.len(), 2);

    // Grow Flat.
    let mut s2 = flat_of("ab");
    s2.resize(5, cell_with_ch('.'));
    assert_eq!(s2.len(), 5);
    assert_eq!(s2.get(4).unwrap().ch, '.');

    // Grow Cluster: trailing cluster matches fill → merges, stays cluster.
    let mut c = cluster_uniform(' ', 4);
    c.resize(10, cell_with_ch(' '));
    assert!(c.is_cluster());
    assert_eq!(c.len(), 10);

    // Grow Cluster: trailing cluster differs → new cluster appended.
    let mut c2 = cluster_uniform('a', 3);
    c2.resize(6, cell_with_ch('b'));
    assert!(c2.is_cluster());
    assert_eq!(c2.len(), 6);
    assert_eq!(c2.get(3).unwrap().ch, 'b');
    assert_eq!(c2.get(5).unwrap().ch, 'b');
}

#[test]
fn clear_empties() {
    let mut s = flat_of("abc");
    s.clear();
    assert!(s.is_empty());
    assert!(s.is_flat());

    let mut c = cluster_uniform('x', 50);
    c.clear();
    assert!(c.is_empty());
}

#[test]
fn iter_flat_and_cluster_yield_identical_vec() {
    let flat = LineStorage::Flat(vec![
        cell_with_ch('a'),
        cell_with_ch('a'),
        cell_with_ch('a'),
        cell_with_ch('b'),
        cell_with_ch('b'),
    ]);
    let clust = LineStorage::Cluster(vec![
        Cluster { cell: cell_with_ch('a'), count: 3 },
        Cluster { cell: cell_with_ch('b'), count: 2 },
    ]);
    let v1: Vec<char> = flat.iter().map(|c| c.ch).collect();
    let v2: Vec<char> = clust.iter().map(|c| c.ch).collect();
    assert_eq!(v1, v2);
    assert_eq!(v1, vec!['a', 'a', 'a', 'b', 'b']);
}

#[test]
fn iter_mut_forces_flat_and_allows_edits() {
    let mut c = cluster_uniform('a', 3);
    for slot in c.iter_mut() {
        slot.ch = 'Z';
    }
    assert!(c.is_flat());
    let v: Vec<char> = c.iter().map(|x| x.ch).collect();
    assert_eq!(v, vec!['Z', 'Z', 'Z']);
}

#[test]
fn fill_range_degrades_and_clamps_end() {
    let mut s = cluster_uniform(' ', 10);
    s.fill_range(2, 5, cell_with_ch('X'));
    assert!(s.is_flat());
    let v: Vec<char> = s.iter().map(|c| c.ch).collect();
    assert_eq!(v, vec![' ', ' ', 'X', 'X', 'X', ' ', ' ', ' ', ' ', ' ']);

    // Clamp at end
    let mut s2 = flat_of("abcd");
    s2.fill_range(2, 100, cell_with_ch('!'));
    let v2: Vec<char> = s2.iter().map(|c| c.ch).collect();
    assert_eq!(v2, vec!['a', 'b', '!', '!']);

    // Empty range no-op (stays in original form since we early-return).
    let mut s3 = cluster_uniform('a', 4);
    s3.fill_range(2, 2, cell_with_ch('X'));
    let v3: Vec<char> = s3.iter().map(|c| c.ch).collect();
    assert_eq!(v3, vec!['a', 'a', 'a', 'a']);
}

#[test]
fn copy_within_handles_overlap() {
    let mut s = flat_of("abcdef");
    s.copy_within(0..3, 3);
    let v: Vec<char> = s.iter().map(|c| c.ch).collect();
    assert_eq!(v, vec!['a', 'b', 'c', 'a', 'b', 'c']);

    // Overlap left-shift
    let mut s2 = flat_of("abcdef");
    s2.copy_within(2..5, 0);
    let v2: Vec<char> = s2.iter().map(|c| c.ch).collect();
    assert_eq!(v2, vec!['c', 'd', 'e', 'd', 'e', 'f']);

    // From Cluster: forces Flat.
    let mut c = cluster_uniform('a', 6);
    c.copy_within(0..3, 3);
    assert!(c.is_flat());
    assert_eq!(c.len(), 6);
}

#[test]
fn to_flat_is_idempotent() {
    let mut f = flat_of("xy");
    f.to_flat();
    assert!(f.is_flat());

    let mut c = cluster_uniform('a', 3);
    c.to_flat();
    assert!(c.is_flat());
    let v: Vec<char> = c.iter().map(|x| x.ch).collect();
    assert_eq!(v, vec!['a', 'a', 'a']);
}

#[test]
fn try_compress_uniform_flat_to_cluster() {
    let mut s = LineStorage::Flat(vec![cell_with_ch(' '); 200]);
    assert!(s.try_compress());
    assert!(s.is_cluster());
    assert_eq!(s.len(), 200);

    // Already cluster: no-op.
    assert!(!s.try_compress());

    // Empty: no-op.
    let mut e = LineStorage::Flat(Vec::new());
    assert!(!e.try_compress());
}

#[test]
fn large_line_random_access_parity() {
    // 1000-cell line: alternating runs of 10 'a' and 5 'b'.
    let mut flat_cells = Vec::with_capacity(1000);
    let mut clusters = Vec::new();
    let mut i = 0;
    while i < 1000 {
        let (ch, run) = if (i / 10) % 2 == 0 { ('a', 10) } else { ('b', 5) };
        let take = run.min(1000 - i);
        for _ in 0..take {
            flat_cells.push(cell_with_ch(ch));
        }
        clusters.push(Cluster { cell: cell_with_ch(ch), count: take });
        i += take;
    }
    let flat = LineStorage::Flat(flat_cells);
    let clust = LineStorage::Cluster(clusters);
    assert_eq!(flat.len(), clust.len());
    for idx in [0usize, 1, 9, 10, 14, 15, 500, 999] {
        assert_eq!(flat.get(idx).unwrap().ch, clust.get(idx).unwrap().ch, "idx {idx}");
    }
    let v1: Vec<char> = flat.iter().map(|c| c.ch).collect();
    let v2: Vec<char> = clust.iter().map(|c| c.ch).collect();
    assert_eq!(v1, v2);
}

#[test]
fn single_cell_edge() {
    let mut s = flat_of("x");
    assert_eq!(s.len(), 1);
    s.truncate(0);
    assert!(s.is_empty());

    let mut c = cluster_uniform('y', 1);
    assert_eq!(c.get(0).unwrap().ch, 'y');
    c.set(0, cell_with_ch('Z'));
    assert_eq!(c.get(0).unwrap().ch, 'Z');
}
