#![allow(deprecated)] // Pending migration to UiPalette (PR #119 follow-up).
use sonic_shared::tabbar_view::*;
use sonic_shared::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b
}

#[test]
fn empty_bar_still_has_new_tab_button() {
    let bar = TabBar::new();
    let layout = TabBarLayout::compute(&bar, 800.0);
    assert!(layout.tabs.is_empty());
    // Click squarely inside the 28×28 + button.
    let cx = layout.new_tab.x + layout.new_tab.w / 2.0;
    let cy = layout.new_tab.y + layout.new_tab.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::NewTab));
}

#[test]
fn click_inside_tab_returns_activate() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let t0 = layout.tabs[0];
    let cx = t0.bg.x + t0.bg.w / 2.0 - CLOSE_BUTTON_SIZE;
    let cy = t0.bg.y + t0.bg.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(0)));
}

#[test]
fn click_on_close_button_returns_close() {
    let mut bar = bar_with(2);
    // Close × only fires on the ACTIVE tab (Chrome/WezTerm semantics);
    // clicking the × of an inactive tab activates it instead.
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let t1 = layout.tabs[1];
    let cx = t1.close.x + t1.close.w / 2.0;
    let cy = t1.close.y + t1.close.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Close(1)));
}

#[test]
fn click_on_close_x_of_inactive_tab_activates_not_closes() {
    // Reproduces the PR #107 regression: clicking the × on an inactive
    // tab silently closed it instead of activating. Chrome/WezTerm
    // semantics — the × is only an active-tab affordance.
    let mut bar = bar_with(3);
    bar.activate(2); // tab 2 active; tab 1 inactive
    let layout = TabBarLayout::compute(&bar, 800.0);
    let t1 = layout.tabs[1];
    let cx = t1.close.x + t1.close.w / 2.0;
    let cy = t1.close.y + t1.close.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(1)));
}

#[test]
fn click_on_close_x_of_active_tab_closes_it() {
    let mut bar = bar_with(3);
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let t1 = layout.tabs[1];
    let cx = t1.close.x + t1.close.w / 2.0;
    let cy = t1.close.y + t1.close.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Close(1)));
}

#[test]
fn click_on_plus_button_returns_new_tab() {
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let cx = layout.new_tab.x + 4.0;
    let cy = layout.new_tab.y + 4.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::NewTab));
}

#[test]
fn click_below_bar_returns_none() {
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    assert!(layout.hit(100.0, TAB_BAR_HEIGHT + 4.0).is_none());
}

#[test]
fn tab_widths_shrink_when_many_tabs() {
    let bar = bar_with(20);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let last = layout.tabs.last().unwrap();
    assert!(last.bg.x + last.bg.w <= layout.new_tab.x + 1.0);
}

#[test]
fn tab_widths_clamp_at_max() {
    let bar = bar_with(1);
    let layout = TabBarLayout::compute(&bar, 4000.0);
    assert!((layout.tabs[0].bg.w - TAB_MAX_WIDTH).abs() < 0.5);
}

#[test]
fn rect_contains_is_half_open() {
    let r = Rect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
    assert!(r.contains(10.0, 10.0));
    assert!(r.contains(29.999, 29.999));
    assert!(!r.contains(30.0, 20.0));
    assert!(!r.contains(20.0, 30.0));
}

#[test]
fn bar_background_click_between_tabs_snaps_to_nearest_tab() {
    // v0.6 user report "标题的tab选中区域不是一整个tab区域": before
    // the fix this returned `Activate(active_tab)` for every click in
    // the inter-tab gap, which meant clicking the visual padding next
    // to tab 0 silently re-activated the already-active tab. After
    // the fix the gap click snaps to the nearest tab horizontally —
    // here, 1px past tab 0's right edge picks tab 0, not tab 1.
    let mut bar = bar_with(3);
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let just_past_tab0 = layout.tabs[0].bg.x + layout.tabs[0].bg.w + 1.0;
    let hit = layout.hit(just_past_tab0, 1.0);
    assert_eq!(hit, Some(TabHit::Activate(0)));
    // Symmetric: 1px before tab 1's left edge → tab 1, regardless of
    // which tab is currently active.
    let just_before_tab1 = layout.tabs[1].bg.x - 1.0;
    let hit = layout.hit(just_before_tab1, 1.0);
    assert_eq!(hit, Some(TabHit::Activate(1)));
}

#[test]
fn hidden_bar_does_not_capture_clicks() {
    // Regression: when the tab bar is toggled off, the visual is gone
    // but earlier code still routed clicks in that pixel region to the
    // tab bar — an invisible UI silently swallowing input. A hidden
    // layout must report no hits anywhere.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 800.0).with_visible(false);
    // (10, 5) would normally land squarely inside tab 0.
    assert_eq!(layout.hit(10.0, 5.0), None);
    // The new-tab button region is also dead.
    assert_eq!(layout.hit(790.0, 10.0), None);
    // And `point_over_bar` agrees.
    assert!(!layout.point_over_bar(10.0, 5.0));
}

#[test]
fn visible_bar_still_captures_clicks() {
    // Sanity: with_visible(true) is the default and preserves behavior.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 800.0).with_visible(true);
    // x must land within tab 0's horizontal range (which now starts at
    // BAR_LEFT_PAD = 12, not 4).
    let probe_x = layout.tabs[0].bg.x + 1.0;
    assert!(matches!(layout.hit(probe_x, 5.0), Some(TabHit::Activate(0))));
    assert!(layout.point_over_bar(probe_x, 5.0));
}

#[test]
fn with_top_offset_shifts_every_rect() {
    let bar = bar_with(2);
    let base = TabBarLayout::compute(&bar, 800.0);
    let shifted = TabBarLayout::compute(&bar, 800.0).with_top_offset(28.0);
    assert_eq!(shifted.bar.y, base.bar.y + 28.0);
    assert_eq!(shifted.new_tab.y, base.new_tab.y + 28.0);
    for (a, b) in shifted.tabs.iter().zip(base.tabs.iter()) {
        assert_eq!(a.bg.y, b.bg.y + 28.0);
        assert_eq!(a.close.y, b.close.y + 28.0);
        assert_eq!(a.title.y, b.title.y + 28.0);
    }
}

#[test]
fn with_top_offset_creates_dead_zone_above_bar() {
    // Regression for the macOS integrated-titlebar overlap: a click at
    // y=5 (under the OS traffic lights) must NOT activate a tab because
    // the layout has been shifted down by the titlebar inset.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 800.0).with_top_offset(28.0);
    assert!(layout.hit(50.0, 5.0).is_none(), "click in titlebar dead-zone must not hit tab");
    // A click ~just below the titlebar should still activate the first tab.
    let inside_y = 28.0 + (TAB_BAR_HEIGHT / 2.0);
    assert!(matches!(layout.hit(50.0, inside_y), Some(TabHit::Activate(0))));
}

#[test]
fn with_top_offset_zero_is_noop() {
    let bar = bar_with(2);
    let base = TabBarLayout::compute(&bar, 800.0);
    let same = TabBarLayout::compute(&bar, 800.0).with_top_offset(0.0);
    assert_eq!(same.bar.y, base.bar.y);
    assert_eq!(same.tabs[0].bg.y, base.tabs[0].bg.y);
}

#[test]
fn hit_test_blank_area_of_tab_activates_it() {
    // Regression: previously the hit-test used `t.bg.contains(px, py)`
    // and `bg` is inset by 2px on top and 2px on bottom from the full
    // bar height. A click in those thin slivers fell through to the
    // "click between tabs → activate currently-active tab" default,
    // making the user feel they had to aim at the title glyphs to
    // switch tabs. After the fix, ANY pixel inside the bar whose x is
    // within a tab's horizontal range activates that tab.
    let mut bar = bar_with(3);
    bar.activate(1); // tab 2 is currently active
    let layout = TabBarLayout::compute(&bar, 800.0);

    let t0 = layout.tabs[0];
    // 1) Click at tab0's right edge, vertically at the top sliver
    //    (y < bg.y) — must STILL activate tab 0, not snap to active=1.
    let click_x = t0.bg.x + t0.bg.w - 1.0;
    let click_y_top = 0.5; // above bg.y = 2.0
    assert_eq!(
        layout.hit(click_x, click_y_top),
        Some(TabHit::Activate(0)),
        "click at tab0 right edge / top sliver must activate tab 0"
    );

    // 2) Bottom sliver of tab 0 (y > bg.y + bg.h but y < bar.h).
    let click_y_bottom = TAB_BAR_HEIGHT - 0.5;
    assert_eq!(
        layout.hit(click_x, click_y_bottom),
        Some(TabHit::Activate(0)),
        "click at tab0 right edge / bottom sliver must activate tab 0"
    );

    // 3) Middle of tab 0 — already worked, stays working.
    let click_y_mid = TAB_BAR_HEIGHT / 2.0;
    assert_eq!(
        layout.hit(click_x, click_y_mid),
        Some(TabHit::Activate(0)),
        "click in middle of tab 0 activates tab 0"
    );
}

// ------------------ Issue #112 Round 3 spec tests ------------------

#[test]
fn tab_max_width_is_240() {
    assert_eq!(TAB_MAX_WIDTH, 240.0);
    // And a single tab in a wide window clamps to it.
    let bar = bar_with(1);
    let layout = TabBarLayout::compute(&bar, 4000.0);
    assert!((layout.tabs[0].bg.w - 240.0).abs() < 0.5);
}

#[test]
fn tab_min_width_is_at_least_200() {
    // Bumped from 100 -> 200 by issue #171 so common shell titles
    // (`Administrator: cmd.exe`, `pwsh`, ...) stay legible in the
    // common 2-4 tab case at 1000 px wide.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(TAB_MIN_WIDTH >= 200.0, "got {TAB_MIN_WIDTH}");
    }
}

#[test]
fn tab_gap_is_6() {
    assert_eq!(TAB_GAP, 6.0);
    // Adjacent tabs in a real layout sit exactly TAB_GAP apart.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let gap = layout.tabs[1].bg.x - (layout.tabs[0].bg.x + layout.tabs[0].bg.w);
    assert!((gap - TAB_GAP).abs() < 0.5, "gap = {gap}");
}

#[test]
fn bar_left_pad_is_12() {
    assert_eq!(BAR_LEFT_PAD, 12.0);
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    assert!((layout.tabs[0].bg.x - 12.0).abs() < 0.01);
}

#[test]
fn tab_inner_pad_is_10() {
    assert_eq!(TAB_INNER_PAD, 10.0);
}

#[test]
fn active_tab_top_accent_2px_blue() {
    // The renderer draws a 2px top accent bar on the active tab,
    // ACCENT_BLUE, inset on each side by ACTIVE_TOP_ACCENT_INSET so
    // its width is `tab_w - 2 * inset`. Lock the constants so a
    // future tweak doesn't silently drift.
    assert_eq!(ACTIVE_TOP_ACCENT_H, 2.0);
    assert_eq!(ACTIVE_TOP_ACCENT_INSET, 6.0);
    // And the token color is the canonical Tokyo-night blue (#7AA2F7).
    let blue = sonic_shared::ui_tokens::color::ACCENT_BLUE();
    assert!((blue[3] - 1.0).abs() < 1e-4);
    // Premultiplied invariant.
    assert!(blue[0] <= blue[3] + 1e-5);
}

#[test]
fn inactive_tab_hover_bg_uses_white_6pct() {
    // The renderer paints the hover overlay on an inactive tab using
    // `ui_tokens::color::BG_HOVER` which is #FFFFFF @ 6 % — verify the
    // alpha here so the token is the source of truth.
    let c = sonic_shared::ui_tokens::color::BG_HOVER();
    let expected_a = 0x0F as f32 / 255.0; // hex "0F" ≈ 6 %
    assert!((c[3] - expected_a).abs() < 1e-3, "got alpha {}", c[3]);
}

#[test]
fn new_tab_button_size_28x28() {
    assert_eq!(NEW_TAB_BUTTON_WIDTH, 28.0);
    assert_eq!(NEW_TAB_BUTTON_HEIGHT, 28.0);
    // And the layout produces a 28x28 hit rect at the right edge of
    // the bar, vertically centered in a 40px bar.
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    assert!((layout.new_tab.w - 28.0).abs() < 0.01);
    assert!((layout.new_tab.h - 28.0).abs() < 0.01);
    // Centered vertically: y = (40 - 28) / 2 = 6.
    assert!((layout.new_tab.y - 6.0).abs() < 0.01);
}

#[test]
fn bar_height_default_is_40() {
    assert_eq!(TAB_BAR_HEIGHT, 40.0);
}
