//! Regression coverage for whole-widget tab hit-testing: each tab owns one
//! background rect, and points in inter-tab gaps / outer bar padding no
//! longer snap to a neighbouring tab through layout-level fallback logic.

use sonicterm_ui::tabbar_view::{TabBarLayout, BAR_LEFT_PAD, TAB_GAP};
use sonicterm_ui::tabs::{Tab, TabBar};

fn bar(n: usize, active: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("shell {}", i + 1)));
    }
    b.activate(active);
    b
}

#[test]
fn click_in_gap_between_tabs_misses() {
    let layout = TabBarLayout::compute(&bar(3, 0), 1200.0);
    // Pick the gap between tab 0 and tab 1.
    let t0_right = layout.tabs[0].bg.x + layout.tabs[0].bg.w;
    let t1_left = layout.tabs[1].bg.x;
    assert!(t1_left > t0_right, "tabs must have a real gap to test");
    // Whole-widget hit testing means the tab owns only its bg rect. Gap
    // clicks no longer snap to a nearest tab through layout-level fallback.
    let py = layout.tabs[0].bg.y + layout.tabs[0].bg.h * 0.5;
    let hit = layout.hit(t0_right + 1.0, py);
    assert_eq!(hit, None);
    // Symmetric: 1px before tab 1's left edge is still gap, not tab 1.
    let hit = layout.hit(t1_left - 1.0, py);
    assert_eq!(hit, None);
}

#[test]
fn gap_click_is_none_independent_of_active_tab() {
    // Flip the active tab and confirm a gap click is still not owned by
    // any tab widget.
    let py = 20.0;
    let layout_a = TabBarLayout::compute(&bar(3, 0), 1200.0);
    let layout_b = TabBarLayout::compute(&bar(3, 2), 1200.0);
    let t1_right = layout_a.tabs[1].bg.x + layout_a.tabs[1].bg.w;
    // 2px past tab 1's right edge is outside every tab bg rect.
    let probe_x = t1_right + 2.0;
    assert_eq!(layout_a.hit(probe_x, py), None);
    assert_eq!(layout_b.hit(probe_x, py), None);
}

#[test]
fn click_in_left_pad_misses_tabs() {
    let layout = TabBarLayout::compute(&bar(3, 1), 1200.0);
    // 1px inside the bar, well to the left of the first tab.
    let hit = layout.hit(BAR_LEFT_PAD * 0.5, 20.0);
    assert_eq!(hit, None);
}

#[test]
fn click_past_last_tab_before_plus_misses_tabs() {
    let layout = TabBarLayout::compute(&bar(3, 0), 1200.0);
    let last = layout.tabs.last().unwrap();
    // 1px past the last tab's right edge, still well left of the `+`.
    let probe_x = last.bg.x + last.bg.w + TAB_GAP * 0.5;
    let hit = layout.hit(probe_x, 20.0);
    assert_eq!(hit, None);
}
