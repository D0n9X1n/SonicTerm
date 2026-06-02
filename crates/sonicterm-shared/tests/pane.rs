//! Integration tests for pane.

use sonicterm_cfg::keymap::Direction;
use sonicterm_ui::pane::*;

#[test]
fn split_right_then_down() {
    let mut t = PaneTree::leaf(1);
    assert!(t.split(1, Direction::Right, 2));
    assert!(t.split(2, Direction::Down, 3));
    assert_eq!(t.leaves(), vec![1, 2, 3]);
}

#[test]
fn close_collapses_split() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    t.close(2);
    assert_eq!(t.leaves(), vec![1]);
    assert!(matches!(t, PaneTree::Leaf { id: 1, .. }));
}

#[test]
fn split_left_inserts_new_pane_first() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Left, 2);
    assert_eq!(t.leaves(), vec![2, 1]);
}

#[test]
fn split_up_uses_horizontal_axis() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Up, 2);
    if let PaneTree::Split { axis, .. } = &t {
        assert_eq!(*axis, SplitAxis::Horizontal);
    } else {
        panic!("expected split");
    }
}

#[test]
fn split_nonexistent_focus_is_noop() {
    let mut t = PaneTree::leaf(1);
    assert!(!t.split(999, Direction::Right, 2));
    assert_eq!(t.leaves(), vec![1]);
}

#[test]
fn close_nested_pane_preserves_siblings() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    t.split(2, Direction::Down, 3);
    t.close(3);
    assert_eq!(t.leaves(), vec![1, 2]);
}

// ---------- layout ----------

#[test]
fn layout_single_leaf_fills_outer() {
    let t = PaneTree::leaf(7);
    let panes = t.layout(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].0, 7);
    assert_eq!(panes[0].1.w, 100.0);
    assert_eq!(panes[0].1.h, 50.0);
}

#[test]
fn layout_vertical_split_divides_width() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    let panes = t.layout(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!(panes.len(), 2);
    let p1 = panes.iter().find(|(id, _)| *id == 1).unwrap().1;
    let p2 = panes.iter().find(|(id, _)| *id == 2).unwrap().1;
    assert!((p1.w - 50.0).abs() < 0.01);
    assert!((p2.w - 50.0).abs() < 0.01);
    assert!((p1.h - 50.0).abs() < 0.01);
    assert!((p1.x - 0.0).abs() < 0.01);
    assert!((p2.x - 50.0).abs() < 0.01);
}

// ---------- focus_walk_* ----------

#[test]
fn focus_walk_right_finds_sibling() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    assert_eq!(t.focus_neighbor(1, Direction::Right), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Left), Some(1));
}

#[test]
fn focus_walk_down_finds_sibling() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Down, 2);
    assert_eq!(t.focus_neighbor(1, Direction::Down), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Up), Some(1));
}

#[test]
fn focus_walk_off_edge_returns_none() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    assert_eq!(t.focus_neighbor(1, Direction::Left), None);
    assert_eq!(t.focus_neighbor(2, Direction::Right), None);
    assert_eq!(t.focus_neighbor(1, Direction::Up), None);
}

#[test]
fn focus_walk_nested_picks_nearest() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    t.split(2, Direction::Down, 3);
    let neighbour = t.focus_neighbor(1, Direction::Right).expect("neighbour");
    assert!(neighbour == 2 || neighbour == 3);
    assert_eq!(t.focus_neighbor(3, Direction::Up), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Left), Some(1));
    assert_eq!(t.focus_neighbor(3, Direction::Left), Some(1));
}

#[test]
fn focus_walk_three_column_layout() {
    let mut t = PaneTree::leaf(1);
    t.split(1, Direction::Right, 2);
    t.split(2, Direction::Right, 3);
    assert_eq!(t.focus_neighbor(1, Direction::Right), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Right), Some(3));
    assert_eq!(t.focus_neighbor(3, Direction::Left), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Left), Some(1));
}
