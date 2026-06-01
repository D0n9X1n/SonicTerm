//! Integration tests for sonicterm-shared selection.

use sonicterm_core::grid::{CellFlags, Color, Grid};

use sonicterm_shared::selection::*;

fn grid_with(text: &[&str]) -> Grid {
    let cols = text.iter().map(|s| s.chars().count()).max().unwrap_or(1) as u16;
    let rows = text.len() as u16;
    let mut g = Grid::new(cols, rows);
    for (r, line) in text.iter().enumerate() {
        g.cursor.row = r as u16;
        g.cursor.col = 0;
        for ch in line.chars() {
            g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    g
}

#[test]
fn single_cell_selection_contains_only_itself() {
    let s = Selection::new(2, 5);
    assert!(s.contains(2, 5));
    assert!(!s.contains(2, 6));
    assert!(s.is_empty());
}

#[test]
fn extend_grows_selection() {
    let mut s = Selection::new(1, 1);
    s.extend(3, 7);
    assert!(s.contains(2, 4));
    assert!(s.contains(3, 7));
    assert!(!s.contains(4, 0));
}

#[test]
fn normalized_reorders_reverse_selection() {
    let mut s = Selection::new(3, 7);
    s.extend(1, 2);
    let (a, b) = s.normalized();
    assert_eq!(a, (1, 2));
    assert_eq!(b, (3, 7));
}

#[test]
fn to_string_single_row() {
    let g = grid_with(&["hello world", "second line"]);
    let mut s = Selection::new(0, 0);
    s.extend(0, 4);
    assert_eq!(s.as_text(&g), "hello");
}

#[test]
fn to_string_multi_row() {
    let g = grid_with(&["abc", "def", "ghi"]);
    let mut s = Selection::new(0, 1);
    s.extend(2, 1);
    assert_eq!(s.as_text(&g), "bc\ndef\ngh");
}

#[test]
fn to_string_trims_trailing_spaces() {
    let mut g = Grid::new(10, 1);
    for ch in "hi".chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    let mut s = Selection::new(0, 0);
    s.extend(0, 9);
    assert_eq!(s.as_text(&g), "hi");
}
