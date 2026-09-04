use super::*;

fn nested_tree() -> PaneTree {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.split(1, Direction::Down, 3));
    tree
}

/// Split direction determines leaf order, and an unknown focus cannot mutate the tree.
#[test]
fn split_preserves_directional_order_and_rejects_missing_focus() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.split(1, Direction::Left, 3));
    assert_eq!(tree.leaves(), [3, 1, 2]);

    assert!(!tree.split(99, Direction::Down, 4));
    assert_eq!(tree.leaves(), [3, 1, 2]);
}

/// Closing nested leaves collapses their parent and clears zoom when the zoomed pane closes.
#[test]
fn close_collapses_nested_splits_and_preserves_live_identity() {
    let mut tree = nested_tree();
    assert!(tree.toggle_zoom(3));
    assert!(tree.close(3));
    assert_eq!(tree.leaves(), [1, 2]);
    assert_eq!(tree.zoomed_pane_id(), None);

    assert!(!tree.close(99));
    assert_eq!(tree.leaves(), [1, 2]);
}

/// Layout tiles the outer rectangle and zoom gives one live leaf the whole area.
#[test]
fn layout_and_zoom_preserve_leaf_geometry() {
    let mut tree = nested_tree();
    let outer = Rect::new(10.0, 20.0, 100.0, 80.0);
    assert_eq!(
        tree.layout(outer),
        [
            (1, Rect::new(10.0, 20.0, 50.0, 40.0)),
            (3, Rect::new(10.0, 60.0, 50.0, 40.0)),
            (2, Rect::new(60.0, 20.0, 50.0, 80.0)),
        ]
    );

    assert!(tree.toggle_zoom(3));
    assert_eq!(tree.layout(outer), [(3, outer)]);
    assert!(tree.toggle_zoom(3));
    assert_eq!(tree.leaves(), [1, 3, 2]);
}

/// Direct resize honors the split axis and clamps both children to a visible share.
#[test]
fn resize_split_checks_axis_and_clamps_ratio() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(!tree.resize_split(1, Direction::Up, 0.2));
    assert!(tree.resize_split(1, Direction::Right, 2.0));

    let layout = tree.layout(Rect::new(0.0, 0.0, 100.0, 40.0));
    assert_eq!(layout[0].1.w, 90.0);
    assert_eq!(layout[1].1.w, 10.0);
}

/// Splitter hit identity addresses the same divider for a later drag.
#[test]
fn splitter_hit_round_trips_to_resize() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    let outer = Rect::new(0.0, 0.0, 100.0, 40.0);
    let hit = tree.hit_splitter(outer, 2.0, 50.0, 20.0).expect("root divider");

    assert_eq!(hit.axis, SplitAxis::Vertical);
    assert!(tree.resize_splitter_by_delta(&hit.id, outer, 10.0, 0.0));
    assert!((tree.layout(outer)[0].1.w - 60.0).abs() < 0.000_01);
    assert!(!tree.resize_splitter_by_delta(&SplitterId(vec![false, false]), outer, 1.0, 0.0));
}

/// Focus navigation selects the closest pane in-band and stops at outer edges.
#[test]
fn focus_neighbor_follows_spatial_adjacency() {
    let tree = nested_tree();
    assert_eq!(tree.focus_neighbor(1, Direction::Down), Some(3));
    assert_eq!(tree.focus_neighbor(1, Direction::Right), Some(2));
    assert_eq!(tree.focus_neighbor(2, Direction::Left), Some(1));
    assert_eq!(tree.focus_neighbor(2, Direction::Right), None);
    assert_eq!(tree.focus_neighbor(99, Direction::Left), None);
}

/// Rectangle membership is half-open so adjacent panes never both own one edge.
#[test]
fn rectangle_contains_uses_half_open_edges() {
    let rect = Rect::new(10.0, 20.0, 30.0, 40.0);
    assert!(rect.contains(10.0, 20.0));
    assert!(rect.contains(39.999, 59.999));
    assert!(!rect.contains(40.0, 30.0));
    assert!(!rect.contains(20.0, 60.0));
}
