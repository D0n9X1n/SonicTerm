use sonic_cfg::keymap::Direction;
use sonic_ui::pane::{PaneTree, Rect};

#[test]
fn zoom_toggle_limits_layout_to_active_pane_then_restores_tree() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    assert_eq!(tree.leaves(), vec![1, 2]);

    assert!(tree.toggle_zoom(2));
    assert_eq!(tree.zoomed_pane_id(), Some(2));

    let panes = tree.layout(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!(panes, vec![(2, Rect::new(0.0, 0.0, 100.0, 50.0))]);
    assert_eq!(tree.leaves(), vec![1, 2]);

    assert!(tree.toggle_zoom(2));
    assert_eq!(tree.zoomed_pane_id(), None);
    let restored = tree.layout(Rect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!(restored.len(), 2);
    assert_eq!(tree.leaves(), vec![1, 2]);
}
