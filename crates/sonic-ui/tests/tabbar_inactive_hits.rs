//! Regression coverage for the user-reported "inactive tab clicks do
//! nothing" bug, captured from a live video at /tmp/tab2-frames/f*.png.
//!
//! Two distinct failures the video reproduces:
//!  1. Tab #2 is active. User clicks the × on tab #1 (inactive). The
//!     × is visually present (PR #196 made the renderer paint the
//!     close glyph on any hovered tab), but the click did nothing.
//!  2. Tab #2 is active. User clicks anywhere on tab #1's body. Only
//!     a narrow sub-region (the title text) actually activated; the
//!     surrounding chrome fell through to a no-op default.
//!
//! Both failures are now hard-pinned here so they cannot recur.

use sonic_ui::tabbar_view::{TabBarLayout, TabHit, CLOSE_BUTTON_SIZE};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b
}

// ─── Bug 1 (close on inactive tab) ─────────────────────────────────

#[test]
fn close_click_on_inactive_left_tab_returns_close_for_that_tab() {
    // Repro of f15 from the user video: 2 tabs, tab #1 active, user
    // hovers tab #0 (× appears), clicks it. Expected: Close(0).
    let mut bar = bar_with(2);
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let close = layout.tabs[0].close;
    let cx = close.x + close.w / 2.0;
    let cy = close.y + close.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Close(0)));
}

#[test]
fn close_close_then_drop_keeps_surviving_tab_active() {
    // Behavioural assertion via TabBar (the data model the dispatcher
    // delegates to): closing tab #0 while tab #1 is active leaves a
    // single surviving tab, and active points at it (index 0). The
    // pre-fix `close()` left `active` at 1, dangling past the end and
    // getting clamped down — same final index in this trivial case,
    // but the multi-tab variant below catches the real misbehaviour.
    let mut bar = bar_with(2);
    bar.activate(1);
    let target = bar.tabs()[0].id;
    bar.close(target);
    assert_eq!(bar.len(), 1);
    assert_eq!(bar.active_index(), 0);
}

#[test]
fn closing_left_inactive_tab_keeps_originally_active_tab_active() {
    // 3 tabs, tab #1 active, close tab #0. The originally-active tab
    // (label "tab1") MUST still be the active tab afterwards. Before
    // the fix, `close()` only adjusted on overflow, so active stayed
    // at index 1 and silently re-pointed at what used to be tab #2.
    let mut bar = bar_with(3);
    bar.activate(1);
    let prev_active_id = bar.tabs()[1].id;
    let to_close = bar.tabs()[0].id;
    bar.close(to_close);
    assert_eq!(bar.len(), 2);
    assert_eq!(bar.tabs()[bar.active_index()].id, prev_active_id);
}

#[test]
fn closing_right_inactive_tab_keeps_originally_active_tab_active() {
    // Mirror of the above: 3 tabs, tab #1 active, close tab #2.
    // active should stay at 1 (pos > active branch — no shift).
    let mut bar = bar_with(3);
    bar.activate(1);
    let prev_active_id = bar.tabs()[1].id;
    let to_close = bar.tabs()[2].id;
    bar.close(to_close);
    assert_eq!(bar.len(), 2);
    assert_eq!(bar.tabs()[bar.active_index()].id, prev_active_id);
}

#[test]
fn closing_active_tab_falls_back_to_neighbour() {
    // pos == active branch: closing the active tab shifts focus to
    // the next-right tab (which inherits the active index), or clamps
    // to the new last index if the closed tab was at the end.
    let mut bar = bar_with(3);
    bar.activate(1);
    let right_neighbour_id = bar.tabs()[2].id;
    let active_id = bar.tabs()[1].id;
    bar.close(active_id);
    assert_eq!(bar.len(), 2);
    assert_eq!(bar.tabs()[bar.active_index()].id, right_neighbour_id);
}

// ─── Bug 2 (activate inactive tab on body click) ───────────────────

#[test]
fn body_click_on_inactive_tab_returns_activate_for_that_tab() {
    // Repro of f25: 2 tabs, tab #1 active, user clicks the centre of
    // tab #0's body. The hit-test MUST return Activate(0) — not the
    // legacy "snap to currently-active tab" default.
    let mut bar = bar_with(2);
    bar.activate(1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let t0 = &layout.tabs[0];
    // Aim somewhere in the title region — well away from the close
    // rect on the right edge.
    let cx = t0.bg.x + t0.bg.w / 2.0 - CLOSE_BUTTON_SIZE;
    let cy = t0.bg.y + t0.bg.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(0)));
}

#[test]
fn body_click_left_edge_of_inactive_tab_activates_it() {
    // The activation hit zone must cover the FULL tab background —
    // not just the title text. A click 1 px past the tab's left
    // edge counts as that tab, even though it's in the inset gutter
    // a user might mistake for the inter-tab gap.
    let mut bar = bar_with(3);
    bar.activate(2);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let t1 = &layout.tabs[1];
    let cx = t1.bg.x + 1.0;
    let cy = t1.bg.y + t1.bg.h / 2.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(1)));
}

#[test]
fn body_click_far_right_of_rightmost_inactive_tab_activates_it() {
    // The third user concern from the prompt: clicking the far-right
    // edge of the rightmost tab (just left of the × button) must
    // activate that tab, NOT fall through to the inter-tab gap snap
    // logic and pick the wrong neighbour.
    let mut bar = bar_with(3);
    bar.activate(0); // tab #2 inactive
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let t2 = &layout.tabs[2];
    // 1 px inside the right edge of the tab background, well above
    // the vertical centre of the close rect so we don't accidentally
    // sample inside the × hit zone.
    let cx = t2.bg.x + t2.bg.w - 1.0;
    let cy = t2.bg.y + 1.0;
    assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(2)));
}

#[test]
fn body_click_top_pixel_above_inactive_tab_bg_misses() {
    // Whole-widget hit-testing is anchored to the tab's bg rect. A click
    // at y=0 in a tab's horizontal column is in the bar chrome, but not
    // owned by that tab widget.
    let mut bar = bar_with(3);
    bar.activate(2);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let t0 = &layout.tabs[0];
    let cx = t0.bg.x + t0.bg.w / 2.0 - CLOSE_BUTTON_SIZE;
    assert_eq!(layout.hit(cx, 0.0), None);
}
