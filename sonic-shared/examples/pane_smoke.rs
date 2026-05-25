//! Pane-layout smoke test. Builds a 3-way split, walks neighbours, asserts
//! ordering — does not spawn any PTYs or open a window.
//!
//! Run with: `cargo run --example pane_smoke -p sonic-shared --release`

use sonic_core::keymap::Direction;
use sonic_shared::pane::{PaneTree, Rect};

fn main() {
    // Build:   1 | (2 top / 3 bottom)
    let mut t = PaneTree::leaf(1);
    assert!(t.split(1, Direction::Right, 2));
    assert!(t.split(2, Direction::Down, 3));

    let leaves = t.leaves();
    assert_eq!(leaves, vec![1, 2, 3], "leaf ordering mismatch: {leaves:?}");

    // Layout: pane 1 fills the left half, panes 2/3 share the right half.
    let panes = t.layout(Rect::new(0.0, 0.0, 100.0, 100.0));
    assert_eq!(panes.len(), 3);
    let p1 = panes.iter().find(|(id, _)| *id == 1).unwrap().1;
    let p2 = panes.iter().find(|(id, _)| *id == 2).unwrap().1;
    let p3 = panes.iter().find(|(id, _)| *id == 3).unwrap().1;
    assert!((p1.w - 50.0).abs() < 0.01);
    assert!((p2.w - 50.0).abs() < 0.01);
    assert!((p2.h - 50.0).abs() < 0.01);
    assert!((p3.h - 50.0).abs() < 0.01);
    assert!(p2.y < p3.y, "pane 2 should sit above pane 3");

    // Focus walk
    let right_of_1 = t.focus_neighbor(1, Direction::Right);
    assert!(matches!(right_of_1, Some(2) | Some(3)), "right of 1 = {right_of_1:?}");
    assert_eq!(t.focus_neighbor(3, Direction::Up), Some(2));
    assert_eq!(t.focus_neighbor(2, Direction::Down), Some(3));
    assert_eq!(t.focus_neighbor(2, Direction::Left), Some(1));
    assert_eq!(t.focus_neighbor(1, Direction::Left), None);

    // Close middle pane, ensure tree collapses cleanly.
    assert!(t.close(2));
    assert_eq!(t.leaves(), vec![1, 3]);

    println!("[pane_smoke] OK");
}
