//! Regression: when the tab bar is configured for the bottom of the
//! window, `with_top_offset(window_h - bar_h)` shifts every rect so
//! the bar visually hugs the bottom edge while hit-testing remains
//! correct.
//!
//! The renderer integrates this via `GpuRenderer::tab_bar_y_offset()`
//! (gated behind a real wgpu surface, so not unit-tested here). This
//! file pins the pure-geometry contract that the renderer relies on.

use sonicterm_ui::tabbar_view::{tab_bar_height, TabBarLayout};
use sonicterm_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

const WIN_W: f32 = 1000.0;
const WIN_H: f32 = 700.0;

fn compute_top(bar: &TabBar, font_size: f32) -> TabBarLayout {
    // "Top" placement: anchor at y=0 (no titlebar inset in this synthetic
    // test). This is the historical behavior.
    TabBarLayout::compute_with_height(bar, WIN_W, tab_bar_height(font_size)).with_top_offset(0.0)
}

fn compute_bottom(bar: &TabBar, font_size: f32) -> TabBarLayout {
    let bar_h = tab_bar_height(font_size);
    let y = (WIN_H - bar_h).max(0.0);
    TabBarLayout::compute_at_y(bar, WIN_W, bar_h, y)
}

#[test]
fn bottom_layout_has_bar_y_near_window_bottom() {
    let bar = bar_with(3);
    let font_size = 14.0;
    let bar_h = tab_bar_height(font_size);
    let layout = compute_bottom(&bar, font_size);
    let expected_y = (WIN_H - bar_h).max(0.0);
    assert!(
        (layout.bar.y - expected_y).abs() < f32::EPSILON,
        "bottom bar.y={} expected={}",
        layout.bar.y,
        expected_y
    );
    assert!(
        (layout.bar.y + layout.bar.h - WIN_H).abs() < 1.0,
        "bottom edge should hug window: bar.y+h={} win_h={}",
        layout.bar.y + layout.bar.h,
        WIN_H
    );
}

#[test]
fn top_layout_has_bar_y_near_zero() {
    let bar = bar_with(3);
    let layout = compute_top(&bar, 14.0);
    assert!(layout.bar.y.abs() < f32::EPSILON, "top bar.y={}", layout.bar.y);
}

#[test]
fn bottom_layout_hit_tests_use_bottom_y() {
    let bar = bar_with(2);
    let font_size = 14.0;
    let layout = compute_bottom(&bar, font_size);
    // A click at the top of the window must NOT hit any tab — the bar
    // lives at the bottom now.
    assert!(layout.hit(50.0, 5.0).is_none(), "top-of-window click hit bar");
    // A click inside the actual bar rect must resolve.
    let cx = layout.tabs[0].bg_rect.x + layout.tabs[0].bg_rect.w * 0.5;
    let cy = layout.bar.y + layout.bar.h * 0.5;
    assert!(layout.hit(cx, cy).is_some(), "in-bar click missed");
}

#[test]
fn bottom_layout_drop_slot_still_works() {
    let bar = bar_with(3);
    let layout = compute_bottom(&bar, 14.0);
    let cy = layout.bar.y + layout.bar.h * 0.5;
    // Drop just past the first tab's midpoint -> slot 1
    let midx = layout.tabs[0].bg_rect.x + layout.tabs[0].bg_rect.w * 0.5;
    assert_eq!(layout.drop_slot(midx + 1.0, cy), 1);
}

#[test]
fn bottom_layout_active_accent_follows_bar() {
    let mut bar = bar_with(3);
    bar.activate(1);
    let layout = compute_bottom(&bar, 14.0);
    let accent = layout.active_accent_rect().expect("active accent");
    assert!(
        accent.y >= layout.bar.y - 0.5 && accent.y <= layout.bar.y + layout.bar.h,
        "accent y={} not inside bar y∈[{}, {}]",
        accent.y,
        layout.bar.y,
        layout.bar.y + layout.bar.h
    );
}
