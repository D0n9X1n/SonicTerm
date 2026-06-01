//! PR-D (#319) tests: smart Cluster → Flat degrade on edits.
//!
//! The contract under test: writes that match the existing single-Cluster
//! line's representative cell stay Cluster (no-op); writes that differ in
//! character OR attributes degrade to Flat. Bulk fills follow the same
//! rule. After degrade, content correctness is preserved.

use sonicterm_grid::line::Line;
use sonicterm_types::cell::{Cell, CellFlags, Color};

fn cell_with(ch: char) -> Cell {
    Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())
}

fn cell_bold(ch: char) -> Cell {
    Cell::plain(ch, Color::Default, Color::Default, CellFlags::BOLD)
}

fn clustered_line(ch: char, len: usize) -> Line {
    let mut line = Line::from_flat(vec![cell_with(ch); len]);
    assert!(line.try_compress(), "test setup expects compression to succeed");
    assert!(line.is_clustered());
    line
}

#[test]
fn set_same_char_same_attrs_stays_cluster() {
    let mut line = clustered_line(' ', 80);
    let ok = line.set(10, cell_with(' '));
    assert!(ok);
    assert!(line.is_clustered(), "matching write must NOT degrade");
}

#[test]
fn set_different_attrs_degrades_to_flat() {
    let mut line = clustered_line(' ', 80);
    let ok = line.set(10, cell_bold(' '));
    assert!(ok);
    assert!(!line.is_clustered(), "attr mismatch must degrade");
    assert_eq!(line.get(10).map(|c| c.flags), Some(CellFlags::BOLD));
    assert_eq!(line.get(9).map(|c| c.ch), Some(' '));
    assert_eq!(line.get(11).map(|c| c.ch), Some(' '));
    assert_eq!(line.len(), 80);
}

#[test]
fn set_different_char_degrades_to_flat() {
    let mut line = clustered_line(' ', 80);
    let ok = line.set(5, cell_with('X'));
    assert!(ok);
    assert!(!line.is_clustered(), "char mismatch must degrade");
    assert_eq!(line.get(5).map(|c| c.ch), Some('X'));
    assert_eq!(line.get(4).map(|c| c.ch), Some(' '));
    assert_eq!(line.get(6).map(|c| c.ch), Some(' '));
    assert_eq!(line.len(), 80);
}

#[test]
fn set_out_of_range_returns_false_and_preserves_storage() {
    let mut line = clustered_line(' ', 8);
    assert!(!line.set(100, cell_with('Z')));
    assert!(line.is_clustered());
}

#[test]
fn fill_range_matching_stays_cluster() {
    let mut line = clustered_line(' ', 80);
    line.fill_range(10, 50, cell_with(' '));
    assert!(line.is_clustered(), "matching fill must NOT degrade");
    assert_eq!(line.len(), 80);
}

#[test]
fn fill_range_mismatching_attrs_degrades_and_writes() {
    let mut line = clustered_line(' ', 80);
    line.fill_range(10, 50, cell_bold(' '));
    assert!(!line.is_clustered());
    for i in 0..10 {
        assert_eq!(line.get(i).map(|c| c.flags), Some(CellFlags::empty()), "prefix at {i}");
    }
    for i in 10..50 {
        assert_eq!(line.get(i).map(|c| c.flags), Some(CellFlags::BOLD), "filled at {i}");
    }
    for i in 50..80 {
        assert_eq!(line.get(i).map(|c| c.flags), Some(CellFlags::empty()), "suffix at {i}");
    }
}

#[test]
fn fill_range_mismatching_char_degrades_and_writes() {
    let mut line = clustered_line(' ', 40);
    line.fill_range(5, 15, cell_with('-'));
    assert!(!line.is_clustered());
    for i in 0..5 {
        assert_eq!(line.get(i).map(|c| c.ch), Some(' '));
    }
    for i in 5..15 {
        assert_eq!(line.get(i).map(|c| c.ch), Some('-'));
    }
    for i in 15..40 {
        assert_eq!(line.get(i).map(|c| c.ch), Some(' '));
    }
}

#[test]
fn fill_range_empty_is_noop_on_cluster() {
    let mut line = clustered_line(' ', 40);
    line.fill_range(5, 5, cell_with('X'));
    line.fill_range(10, 3, cell_with('X'));
    assert!(line.is_clustered());
}

#[test]
fn set_on_flat_line_works_unchanged() {
    let mut line = Line::from_flat(vec![cell_with(' '); 10]);
    assert!(!line.is_clustered());
    assert!(line.set(3, cell_with('A')));
    assert_eq!(line.get(3).map(|c| c.ch), Some('A'));
    assert!(!line.is_clustered());
}

#[test]
fn fill_range_on_flat_line_works_unchanged() {
    let mut line = Line::from_flat(vec![cell_with(' '); 10]);
    line.fill_range(2, 6, cell_with('='));
    for i in 2..6 {
        assert_eq!(line.get(i).map(|c| c.ch), Some('='));
    }
    assert!(!line.is_clustered());
}
