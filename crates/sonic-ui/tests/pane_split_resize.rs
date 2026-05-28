use sonic_cfg::keymap::Direction;
use sonic_ui::pane::{PaneTree, Rect};

fn width_for(tree: &PaneTree, pane_id: u64) -> f32 {
    tree.layout(Rect::new(0.0, 0.0, 100.0, 50.0))
        .into_iter()
        .find(|(id, _)| *id == pane_id)
        .expect("pane present")
        .1
        .w
}

#[test]
fn resize_left_nudges_vertical_split_by_five_percent_and_clamps() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));

    for _ in 0..3 {
        assert!(tree.resize_split(1, Direction::Left, 0.05));
    }
    assert!((width_for(&tree, 1) - 35.0).abs() < 0.01);

    for _ in 0..20 {
        assert!(tree.resize_split(1, Direction::Left, 0.05));
    }
    assert!((width_for(&tree, 1) - 10.0).abs() < 0.01);
}
