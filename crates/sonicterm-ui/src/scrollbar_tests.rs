use super::*;

// 100x100 pane whose right edge is at x=100, top at y=0.
fn pane() -> Rect {
    Rect::new(0.0, 0.0, 100.0, 100.0)
}

#[test]
fn never_mode_and_no_scrollback_suppress_the_bar() {
    assert!(compute(10, 20, 0, pane(), ScrollbarMode::Never, 8.0).is_none());
    // total <= viewport => nothing to scroll.
    assert!(compute(20, 20, 0, pane(), ScrollbarMode::Always, 8.0).is_none());
    assert!(compute(20, 10, 0, pane(), ScrollbarMode::Always, 8.0).is_none());
}

#[test]
fn degenerate_inputs_suppress_the_bar() {
    assert!(compute(0, 20, 0, pane(), ScrollbarMode::Always, 8.0).is_none());
    assert!(compute(10, 20, 0, pane(), ScrollbarMode::Always, 0.0).is_none());
    assert!(compute(10, 20, 0, Rect::new(0.0, 0.0, 0.0, 100.0), ScrollbarMode::Always, 8.0)
        .is_none());
}

#[test]
fn track_is_right_aligned_with_the_given_width() {
    let g = compute(10, 20, 0, pane(), ScrollbarMode::Always, 8.0).unwrap();
    assert_eq!(g.track_rect.x, 92.0);
    assert_eq!(g.track_rect.w, 8.0);
    assert_eq!(g.track_rect.h, 100.0);
}

#[test]
fn thumb_height_tracks_viewport_ratio_with_a_min() {
    // 10/20 visible => half the track height.
    let g = compute(10, 20, 0, pane(), ScrollbarMode::Always, 8.0).unwrap();
    assert!((g.thumb_rect.h - 50.0).abs() < 0.001);
    // Huge scrollback clamps to the 12px minimum so the handle stays
    // grabbable.
    let g2 = compute(10, 100_000, 0, pane(), ScrollbarMode::Always, 8.0).unwrap();
    assert!((g2.thumb_rect.h - 12.0).abs() < 0.001);
}

#[test]
fn thumb_sits_at_top_when_following_oldest_and_bottom_at_live_edge() {
    let top = compute(10, 20, 0, pane(), ScrollbarMode::Always, 8.0).unwrap();
    assert_eq!(top.thumb_rect.y, 0.0);
    // max_view_top = total - vp = 10; at the live edge the thumb bottom
    // touches the track bottom.
    let bottom = compute(10, 20, 10, pane(), ScrollbarMode::Always, 8.0).unwrap();
    assert!((bottom.thumb_rect.y + bottom.thumb_rect.h - 100.0).abs() < 0.001);
}

#[test]
fn hit_test_classifies_thumb_track_and_miss() {
    let g = compute(10, 20, 5, pane(), ScrollbarMode::Always, 8.0).unwrap();
    // Off the track entirely (left of x=92).
    assert_eq!(hit_test(&g, Point::new(50.0, 50.0)), HitTarget::None);
    // On the thumb.
    let mid_thumb = g.thumb_rect.y + g.thumb_rect.h / 2.0;
    assert_eq!(hit_test(&g, Point::new(95.0, mid_thumb)), HitTarget::Thumb);
    // Above / below the thumb but still on the track.
    assert_eq!(hit_test(&g, Point::new(95.0, g.thumb_rect.y - 1.0)), HitTarget::TrackAbove);
    assert_eq!(
        hit_test(&g, Point::new(95.0, g.thumb_rect.y + g.thumb_rect.h + 1.0)),
        HitTarget::TrackBelow
    );
}

#[test]
fn thumb_to_view_top_inverts_compute() {
    // For every reachable view_top, compute the thumb_y then map it back.
    for view_top in 0..=10u64 {
        let g = compute(10, 20, view_top, pane(), ScrollbarMode::Always, 8.0).unwrap();
        let back = thumb_to_view_top(&g, g.thumb_rect.y, 10, 20);
        assert_eq!(back, view_top, "round-trip failed at view_top={view_top}");
    }
}

// Regression: the hit-test width must match the *rendered*
// (DPI-scaled) width, or the left of the thumb is dead on fractional DPI.
#[test]
fn dpi_scaled_width_makes_the_full_drawn_thumb_grabbable() {
    let scale = 1.75; // e.g. a 175% display
                      // Width-8 (the old un-scaled hit-test): track starts at x=92.
    let unscaled = compute(10, 20, 0, pane(), ScrollbarMode::Always, 8.0).unwrap();
    // Width 8*scale=14 (what the renderer actually draws): track at x=86.
    let scaled = compute(10, 20, 0, pane(), ScrollbarMode::Always, 8.0 * scale).unwrap();
    assert_eq!(unscaled.track_rect.x, 92.0);
    assert_eq!(scaled.track_rect.x, 100.0 - 14.0);
    // A press at x=88 is inside the DRAWN bar but was missed by the old
    // 8px hit-test — the exact "thumb won't grab" bug.
    let press = Point::new(88.0, 10.0);
    assert_eq!(hit_test(&unscaled, press), HitTarget::None);
    assert_eq!(hit_test(&scaled, press), HitTarget::Thumb);
}

// Regression (second cause): the renderer draws the bar in
// the PADDED content rect, but hit-testing used the raw pane rect. With a
// right-aligned track, the grabbable band was shifted `padding_right` px
// to the right of the drawn thumb — at fractional DPI the two stopped
// overlapping entirely. This pins that the inset content rect is what must
// be hit-tested. (Mirrors `content_inset_rect` in the app crate.)
#[test]
fn track_must_be_computed_from_the_padded_content_rect() {
    let scale = 1.75_f32;
    let pad_right = 12.0 * scale; // 21px, the default right padding
    let bar = 8.0 * scale; // 14px
    let full = pane(); // 0..100 wide
                       // Inset content rect: width shrinks by the right padding.
    let content = Rect::new(full.x, full.y, full.w - pad_right, full.h);

    let from_full = compute(10, 20, 0, full, ScrollbarMode::Always, bar).unwrap();
    let from_content = compute(10, 20, 0, content, ScrollbarMode::Always, bar).unwrap();

    // Drawn (correct) track is left of the raw-rect track by pad_right.
    assert!((from_content.track_rect.x - (from_full.track_rect.x - pad_right)).abs() < 0.001);
    // A click on the VISIBLE thumb (center of the drawn/content track).
    let drawn_center_x = from_content.track_rect.x + from_content.track_rect.w / 2.0;
    let press = Point::new(drawn_center_x, 10.0);
    // Hits when geometry comes from the content rect...
    assert_eq!(hit_test(&from_content, press), HitTarget::Thumb);
    // ...but the old raw-rect geometry misses it entirely.
    assert_eq!(hit_test(&from_full, press), HitTarget::None);
}

