//! Regression test for v0.6 user report:
//! "标题的tab选中区域不是一整个tab区域" — clicking on the padding
//! between the title text and the visual tab boundary felt unresponsive
//! because the click fell into the `TAB_GAP` gutter between two tab `bg`
//! rectangles. Before the fix, gap clicks returned `Activate(active)` —
//! a no-op when the user wanted to switch to the neighbour tab.
//!
//! After the fix, gap clicks snap to the *nearest* tab horizontally.

use sonic_ui::tabbar_view::{TabBarLayout, TabHit, BAR_LEFT_PAD, TAB_GAP};
use sonic_ui::tabs::{Tab, TabBar};

fn bar(n: usize, active: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("shell {}", i + 1)));
    }
    b.activate(active);
    b
}

#[test]
fn click_in_gap_between_tabs_activates_nearest() {
    let layout = TabBarLayout::compute(&bar(3, 0), 1200.0);
    // Pick the gap between tab 0 and tab 1.
    let t0_right = layout.tabs[0].bg.x + layout.tabs[0].bg.w;
    let t1_left = layout.tabs[1].bg.x;
    assert!(t1_left > t0_right, "tabs must have a real gap to test");
    // A click 1px past tab 0's right edge (still inside the gap) should
    // activate tab 0 — the nearest neighbour. Before the fix this
    // returned Activate(0) only because tab 0 was active; flipping the
    // active tab to 1 and re-running would have returned Activate(1)
    // even though the cursor sat next to tab 0.
    let py = layout.tabs[0].bg.y + layout.tabs[0].bg.h * 0.5;
    let hit = layout.hit(t0_right + 1.0, py);
    assert_eq!(hit, Some(TabHit::Activate(0)));
    // Symmetric: 1px before tab 1's left edge → tab 1.
    let hit = layout.hit(t1_left - 1.0, py);
    assert_eq!(hit, Some(TabHit::Activate(1)));
}

#[test]
fn gap_click_independent_of_active_tab() {
    // The pre-fix code returned Activate(self.active.unwrap_or(0)) for
    // every gap click — i.e. the result depended on which tab was
    // already active, not on cursor position. Flip the active tab and
    // confirm the same gap click still snaps to the nearest neighbour.
    let py = 20.0;
    let layout_a = TabBarLayout::compute(&bar(3, 0), 1200.0);
    let layout_b = TabBarLayout::compute(&bar(3, 2), 1200.0);
    let t1_right = layout_a.tabs[1].bg.x + layout_a.tabs[1].bg.w;
    // 2px past tab 1's right edge — closer to tab 1 than to tab 2.
    let probe_x = t1_right + 2.0;
    assert_eq!(layout_a.hit(probe_x, py), Some(TabHit::Activate(1)));
    assert_eq!(layout_b.hit(probe_x, py), Some(TabHit::Activate(1)));
}

#[test]
fn click_in_left_pad_activates_first_tab() {
    let layout = TabBarLayout::compute(&bar(3, 1), 1200.0);
    // 1px inside the bar, well to the left of the first tab.
    let hit = layout.hit(BAR_LEFT_PAD * 0.5, 20.0);
    assert_eq!(hit, Some(TabHit::Activate(0)));
}

#[test]
fn click_past_last_tab_before_plus_activates_last() {
    let layout = TabBarLayout::compute(&bar(3, 0), 1200.0);
    let last = *layout.tabs.last().unwrap();
    // 1px past the last tab's right edge, still well left of the `+`.
    let probe_x = last.bg.x + last.bg.w + TAB_GAP * 0.5;
    let hit = layout.hit(probe_x, 20.0);
    assert_eq!(hit, Some(TabHit::Activate(last.index)));
}
