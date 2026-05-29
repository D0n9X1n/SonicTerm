//! Regression: clicking the FIRST tab in the gap between its title text
//! and its close `×` button used to do nothing on macOS when another
//! tab was active (user report). The title-text region and the `×`
//! region both worked — only the gap between them was dead.
//!
//! This file pins the pure layout-level hit-test: every point inside
//! `bg` that is NOT inside the `close` rect MUST map to
//! `TabHit::Activate(idx)`, for every tab including the first. If the
//! assertions in this file pass, the bug is downstream of `hit()` —
//! see `crates/sonic-app/tests/click_without_drag_does_not_reorder.rs`
//! for the matching dispatcher-level pin.
//!
//! See PR fix/first-tab-gap-click-ignored for the root cause analysis.

use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize, active: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b.activate(active);
    b
}

/// For tab `idx`, sample a point inside `bg` strictly between
/// `title.right` and `close.left` (the "gap" the user clicked).
fn gap_point(layout: &TabBarLayout, idx: usize) -> (f32, f32) {
    let t = layout.tabs[idx];
    // Title ends at close.x - TAB_INNER_PAD/2 (see compute_with_height);
    // pick the midpoint between title.right and close.left so we are
    // guaranteed inside `bg`, outside `title`, and outside `close`.
    let gap_left = t.title.x + t.title.w;
    let gap_right = t.close.x;
    let cx = (gap_left + gap_right) * 0.5;
    let cy = t.bg.y + t.bg.h * 0.5;
    assert!(cx >= t.bg.x && cx < t.bg.x + t.bg.w, "gap x must be inside bg");
    assert!(cx >= gap_left && cx < gap_right, "gap x must be between title.right and close.left");
    (cx, cy)
}

#[test]
fn first_tab_gap_click_activates_first_tab() {
    // 3 tabs, tab #1 active. Click the title/× gap on tab #0.
    let bar = bar_with(3, 1);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let (cx, cy) = gap_point(&layout, 0);
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(0)));
}

#[test]
fn middle_tab_gap_click_activates_middle_tab() {
    let bar = bar_with(3, 0);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let (cx, cy) = gap_point(&layout, 1);
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(1)));
}

#[test]
fn last_tab_gap_click_activates_last_tab() {
    let bar = bar_with(3, 0);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let (cx, cy) = gap_point(&layout, 2);
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(2)));
}

#[test]
fn first_tab_gap_click_activates_even_with_two_tabs() {
    // Original user repro: 2 tabs, tab #1 active.
    let bar = bar_with(2, 1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let (cx, cy) = gap_point(&layout, 0);
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(0)));
}
