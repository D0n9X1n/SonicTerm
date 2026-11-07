//! Replays each wide layout's setup on a real pane tree, with the tree's own directional focus, and
//! checks the result: the expected number of panes, all the same size, every role where the plan
//! says, and focus on role 1.

use super::*;
use sonicterm_ui::pane::{PaneTree, Rect};

/// Apply `actions` to a one-pane tree as the app does: a split opens the next role's pane and
/// focuses it, a focus move follows the tree's neighbour. Returns the tree, the pane id of each
/// role in order, and the focused pane.
fn replay(actions: &[SetupAction]) -> (PaneTree, Vec<u64>, u64) {
    let mut tree = PaneTree::leaf(0);
    let mut roles = vec![0];
    let mut focus = 0;
    for action in actions {
        match action {
            SetupAction::SplitRight | SetupAction::SplitDown => {
                let direction = if *action == SetupAction::SplitRight {
                    Direction::Right
                } else {
                    Direction::Down
                };
                let new_pane = roles.len() as u64;
                assert!(tree.split(focus, direction, new_pane), "split {action:?} of {focus}");
                roles.push(new_pane);
                focus = new_pane;
            }
            SetupAction::FocusPane(direction) => {
                focus = tree.focus_neighbor(focus, *direction).expect("a neighbour");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    (tree, roles, focus)
}

/// Every wide layout replays to an equal grid with each role where `grid_setup` placed it and
/// focus on role 1.
#[test]
fn each_wide_layout_builds_an_equal_grid_ending_on_the_typing_pane() {
    for (columns, rows) in [(4, 2), (4, 4), (4, 8)] {
        let (actions, places) = grid_setup(columns, rows);
        let (tree, roles, focus) = replay(&actions);
        assert_eq!(roles.len(), columns * rows, "{columns}x{rows}");
        let outer = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let layout = tree.layout(outer);
        assert_eq!(layout.len(), columns * rows);
        let (width, height) = (outer.w / columns as f32, outer.h / rows as f32);
        for (role, pane) in roles.iter().enumerate() {
            let rect = layout.iter().find(|(id, _)| id == pane).expect("laid out").1;
            assert!((rect.w - width).abs() < 2.0 && (rect.h - height).abs() < 2.0, "{rect:?}");
            let (column, row) = places[role];
            assert!((rect.x - column as f32 * width).abs() < 2.0, "role {role} column: {rect:?}");
            assert!((rect.y - row as f32 * height).abs() < 2.0, "role {role} row: {rect:?}");
        }
        assert_eq!(focus, roles[1], "{columns}x{rows}: setup ends on role 1");
        assert_eq!(places[0], (0, 0), "the flood stays top left");
    }
}
