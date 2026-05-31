//! Unit tests for the pure-function scrollbar model (#386 PR-A).

use sonic_cfg::config::ScrollbarMode;
use sonic_ui::scrollbar::{compute, hit_test, thumb_to_view_top, HitTarget, Point, Rect};

fn pane() -> Rect {
    Rect::new(0.0, 0.0, 800.0, 600.0)
}

#[test]
fn none_when_total_eq_viewport_in_auto() {
    assert!(compute(40, 40, 0, pane(), ScrollbarMode::Auto, 6.0).is_none());
}

#[test]
fn none_when_mode_never_even_with_huge_scrollback() {
    assert!(compute(40, 10_000, 5_000, pane(), ScrollbarMode::Never, 6.0).is_none());
}

#[test]
fn none_when_mode_always_without_scrollback() {
    assert!(compute(40, 40, 0, pane(), ScrollbarMode::Always, 6.0).is_none());
}

#[test]
fn none_when_viewport_zero() {
    assert!(compute(0, 100, 0, pane(), ScrollbarMode::Always, 6.0).is_none());
}

#[test]
fn none_when_width_zero() {
    assert!(compute(40, 4000, 0, pane(), ScrollbarMode::Auto, 0.0).is_none());
}

#[test]
fn thumb_height_proportional_to_viewport_total_ratio() {
    let g = compute(40, 400, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    // ratio = 40/400 = 0.1 → ~60 px in a 600 px track.
    assert!((g.thumb_rect.h - 60.0).abs() < 1.0, "thumb_h={}", g.thumb_rect.h);
}

#[test]
fn thumb_at_top_when_view_top_zero() {
    let g = compute(40, 4000, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    assert!((g.thumb_rect.y - g.track_rect.y).abs() < 0.001);
}

#[test]
fn thumb_at_bottom_when_view_top_at_live_edge() {
    let total = 4000u64;
    let vp = 40u16;
    let max_top = total - vp as u64;
    let g = compute(vp, total, max_top, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let expected_bottom = g.track_rect.y + g.track_rect.h;
    let thumb_bottom = g.thumb_rect.y + g.thumb_rect.h;
    assert!((thumb_bottom - expected_bottom).abs() < 0.5);
}

#[test]
fn thumb_in_middle_when_view_top_midway() {
    let total = 4000u64;
    let vp = 40u16;
    let max_top = total - vp as u64;
    let g = compute(vp, total, max_top / 2, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let mid_track = g.track_rect.y + (g.track_rect.h - g.thumb_rect.h) / 2.0;
    assert!((g.thumb_rect.y - mid_track).abs() < 1.0);
}

#[test]
fn thumb_has_minimum_height_on_huge_scrollback() {
    let g = compute(10, 1_000_000, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    assert!(g.thumb_rect.h >= 12.0 - 0.001);
}

#[test]
fn track_rect_pinned_to_right_edge() {
    let g = compute(40, 400, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    assert!((g.track_rect.x + g.track_rect.w - 800.0).abs() < 0.001);
    assert!((g.track_rect.w - 6.0).abs() < 0.001);
}

#[test]
fn hit_test_on_thumb() {
    let g = compute(40, 400, 100, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let p = Point::new(g.thumb_rect.x + 1.0, g.thumb_rect.y + 1.0);
    assert_eq!(hit_test(&g, p), HitTarget::Thumb);
}

#[test]
fn hit_test_track_above_and_below() {
    let g = compute(40, 400, 200, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let above = Point::new(g.track_rect.x + 1.0, g.thumb_rect.y - 5.0);
    let below = Point::new(g.track_rect.x + 1.0, g.thumb_rect.y + g.thumb_rect.h + 5.0);
    assert_eq!(hit_test(&g, above), HitTarget::TrackAbove);
    assert_eq!(hit_test(&g, below), HitTarget::TrackBelow);
}

#[test]
fn hit_test_off_track_returns_none() {
    let g = compute(40, 400, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let off = Point::new(10.0, 10.0); // far left of pane, away from track
    assert_eq!(hit_test(&g, off), HitTarget::None);
}

#[test]
fn drag_round_trip_top() {
    let g = compute(40, 4000, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let v = thumb_to_view_top(&g, g.track_rect.y, 40, 4000);
    assert_eq!(v, 0);
}

#[test]
fn drag_round_trip_bottom() {
    let total = 4000u64;
    let vp = 40u16;
    let g = compute(vp, total, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    let travel_bottom = g.track_rect.y + (g.track_rect.h - g.thumb_rect.h);
    let v = thumb_to_view_top(&g, travel_bottom, vp, total);
    assert_eq!(v, total - vp as u64);
}

#[test]
fn drag_round_trip_clamps_out_of_range() {
    let g = compute(40, 4000, 0, pane(), ScrollbarMode::Auto, 6.0).unwrap();
    // Far above track → 0.
    assert_eq!(thumb_to_view_top(&g, -9999.0, 40, 4000), 0);
    // Far below track → max.
    assert_eq!(thumb_to_view_top(&g, 99999.0, 40, 4000), 4000 - 40);
}

#[test]
fn compute_handles_single_row_total() {
    // total < viewport in Auto → no bar.
    assert!(compute(40, 1, 0, pane(), ScrollbarMode::Auto, 6.0).is_none());
}

#[test]
fn config_default_scrollbar_is_auto() {
    let cfg = sonic_cfg::config::AppearanceConfig::default();
    assert_eq!(cfg.scrollbar, ScrollbarMode::Auto);
}
