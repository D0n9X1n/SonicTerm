use sonic_cfg::keymap::Direction;
use sonic_ui::pane::{PaneTree, Rect, SplitAxis};

#[test]
fn two_by_two_grid_emits_only_interior_splitters() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.split(1, Direction::Down, 3));
    assert!(tree.split(2, Direction::Down, 4));

    let splitters = tree.splitter_rects(Rect::new(0.0, 0.0, 200.0, 100.0), 1.0);

    assert_eq!(splitters.len(), 2);
    let vertical =
        splitters.iter().find(|s| s.axis == SplitAxis::Vertical).expect("vertical interior seam");
    let horizontal = splitters
        .iter()
        .find(|s| s.axis == SplitAxis::Horizontal)
        .expect("horizontal interior seam");

    assert_eq!(vertical.rect, Rect::new(99.5, 0.0, 1.0, 100.0));
    assert_eq!(horizontal.rect, Rect::new(0.0, 49.5, 200.0, 1.0));

    assert!(vertical.rect.x > 0.0);
    assert!(vertical.rect.x + vertical.rect.w < 200.0);
    assert_eq!(vertical.rect.y, 0.0);
    assert_eq!(vertical.rect.y + vertical.rect.h, 100.0);

    assert!(horizontal.rect.y > 0.0);
    assert!(horizontal.rect.y + horizontal.rect.h < 100.0);
    assert_eq!(horizontal.rect.x, 0.0);
    assert_eq!(horizontal.rect.x + horizontal.rect.w, 200.0);
}
