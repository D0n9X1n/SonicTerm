use super::*;

#[test]
fn shared_neighbor_layout_matches_individual_direction_queries() {
    // Reusing one layout must preserve all directional ranking, absent-focus, and zoom behavior.
    let mut tree = nested_tree();
    for zoom in [false, true] {
        if zoom {
            tree.toggle_zoom(1);
        }
        for focus in [1, 2, 3, 99] {
            let expected = [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
                .map(|direction| tree.focus_neighbor(focus, direction));
            assert_eq!(tree.focus_neighbors(focus), expected);
        }
    }
}

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

#[test]
fn successful_split_exits_zoom_for_every_direction_and_tree_depth() {
    // A newly focused leaf must be visible immediately, including when its parent is nested.
    let outer = Rect::new(10.0, 20.0, 800.0, 240.0);
    for nested in [false, true] {
        for direction in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            let mut tree = if nested { nested_tree() } else { PaneTree::leaf(1) };
            let focus = if nested { 3 } else { 1 };
            let mut expected = tree.clone();
            assert!(expected.split(focus, direction, 4));
            assert!(tree.toggle_zoom(focus));
            assert_eq!(tree.layout(outer), [(focus, outer)]);

            assert!(tree.split(focus, direction, 4));

            assert_eq!(tree.zoomed_pane_id(), None, "nested={nested}, {direction:?}");
            assert_eq!(tree.leaves(), expected.leaves());
            assert_eq!(tree.layout(outer), expected.layout(outer));
            assert_eq!(tree.layout(outer).iter().filter(|(id, _)| *id == 4).count(), 1);
            assert!(!tree.splitter_rects(outer, 1.0).is_empty());
        }
    }
}

#[test]
fn refused_split_preserves_zoom_and_geometry() {
    // An unknown focus must not unzoom or resize the existing tree when insertion is refused.
    let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
    for nested in [false, true] {
        for direction in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            let mut tree = if nested { nested_tree() } else { PaneTree::leaf(1) };
            let focus = if nested { 3 } else { 1 };
            let leaves = tree.leaves();
            let unzoomed_layout = tree.layout(outer);
            assert!(tree.toggle_zoom(focus));

            assert!(!tree.split(99, direction, 4));

            assert_eq!(tree.zoomed_pane_id(), Some(focus));
            assert_eq!(tree.leaves(), leaves);
            assert_eq!(tree.layout(outer), [(focus, outer)]);
            assert!(tree.toggle_zoom(focus));
            assert_eq!(tree.layout(outer), unzoomed_layout);
        }
    }
}

#[test]
fn split_after_zoom_preserves_subsequent_close_and_resize_layout() {
    // Collapsing the new split must not restore the zoom that was cleared by successful insertion.
    let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
    let mut tree = nested_tree();
    let mut expected = tree.clone();
    assert!(tree.toggle_zoom(3));
    assert!(tree.split(3, Direction::Right, 4));
    assert!(expected.split(3, Direction::Right, 4));
    assert!(tree.resize_split(4, Direction::Right, 0.1));
    assert!(expected.resize_split(4, Direction::Right, 0.1));
    assert_eq!(tree.layout(outer), expected.layout(outer));

    assert!(tree.close(3));
    assert!(expected.close(3));
    assert_eq!(tree.zoomed_pane_id(), None);
    assert_eq!(tree.leaves(), expected.leaves());
    assert_eq!(tree.layout(outer), expected.layout(outer));
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

#[test]
fn leaf_queries_match_the_allocating_leaf_list() {
    // `contains_pane`, `leaf_count` and `for_each_leaf` replace `leaves()` where only membership,
    // a count or a walk is needed; on seeded random split trees they agree with `leaves()` exactly,
    // order included, for present and absent ids.
    let mut seed: u64 = 0x5eed_0e00;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let directions = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
    for _ in 0..200 {
        let mut tree = PaneTree::leaf(1);
        for new_id in 2..2 + next(12) {
            let leaves = tree.leaves();
            let focus = leaves[next(leaves.len() as u64) as usize];
            tree.split(focus, directions[next(4) as usize], new_id);
        }
        let leaves = tree.leaves();
        let mut walked = Vec::new();
        tree.for_each_leaf(&mut |id| walked.push(id));
        assert_eq!(walked, leaves, "the walk keeps leaves() order");
        assert_eq!(tree.leaf_count(), leaves.len());
        for id in 0..20 {
            assert_eq!(tree.contains_pane(id), leaves.contains(&id), "id {id} in {leaves:?}");
        }
    }
}

#[test]
fn pane_rect_matches_the_full_layout() {
    // `pane_rect` lays out only the splits that contain one pane; on seeded random trees, with and
    // without a zoomed pane, it returns exactly the rect `layout` gives that pane, and `None` for
    // a pane `layout` does not show.
    let mut seed: u64 = 0x5eed_0e01;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let directions = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
    let outer = Rect::new(3.0, 17.0, 1_280.0, 720.0);
    for _ in 0..200 {
        let mut tree = PaneTree::leaf(1);
        for new_id in 2..2 + next(12) {
            let leaves = tree.leaves();
            let focus = leaves[next(leaves.len() as u64) as usize];
            tree.split(focus, directions[next(4) as usize], new_id);
        }
        if next(3) == 0 {
            // When: one in three trees zooms a pane, which hides every sibling from the layout.
            let leaves = tree.leaves();
            tree.toggle_zoom(leaves[next(leaves.len() as u64) as usize]);
        }
        let layout = tree.layout(outer);
        for id in 0..20 {
            let expected = layout.iter().find_map(|(pane, rect)| (*pane == id).then_some(*rect));
            assert_eq!(tree.pane_rect(outer, id), expected, "pane {id} in {layout:?}");
        }
    }
}
