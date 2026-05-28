use sonic_cfg::keymap::Direction;
use sonic_ui::pane::{PaneTree, Rect};

#[test]
fn resize_lone_pane_is_noop() {
    let mut tree = PaneTree::leaf(1);

    assert!(!tree.resize_split(1, Direction::Left, 0.05));
    assert_eq!(tree.leaves(), vec![1]);
    assert_eq!(tree.layout(Rect::new(0.0, 0.0, 100.0, 50.0)).len(), 1);
}
