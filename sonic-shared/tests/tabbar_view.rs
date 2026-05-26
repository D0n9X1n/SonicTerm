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
    assert_eq!(layout.hit(790.0, 10.0), Some(TabHit::NewTab));
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
    let bar = bar_with(2);
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
fn bar_background_click_between_tabs_swallows_to_active() {
    let mut bar = bar_with(3);
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let gap_x = layout.tabs[0].bg.x + layout.tabs[0].bg.w + TAB_GAP / 2.0;
    let hit = layout.hit(gap_x, 1.0);
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
    assert!(matches!(layout.hit(10.0, 5.0), Some(TabHit::Activate(0))));
    assert!(layout.point_over_bar(10.0, 5.0));
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
