use sonicterm_ui::overlays::NotificationBubbleLayout;

#[test]
fn notification_bubble_reserves_right_side_close_hit_area() {
    let layout = NotificationBubbleLayout::compute(1000.0, 600.0, 180.0, 0, 2.0);

    assert!(layout.close.w > 0.0);
    assert_eq!(layout.close.y, layout.border.y);
    assert_eq!(layout.close.h, layout.border.h);
    assert!(layout.close.x >= layout.border.x);
    assert!(layout.close.x + layout.close.w <= layout.border.x + layout.border.w);
    assert!(layout.bg.w <= layout.border.w);
}

#[test]
fn notification_bubble_rows_stack_without_overlap() {
    let first = NotificationBubbleLayout::compute(1000.0, 600.0, 160.0, 0, 1.0);
    let second = NotificationBubbleLayout::compute(1000.0, 600.0, 160.0, 1, 1.0);
    let third = NotificationBubbleLayout::compute(1000.0, 600.0, 160.0, 2, 1.0);

    assert!(second.border.y >= first.border.y + first.border.h);
    assert!(third.border.y >= second.border.y + second.border.h);
}
