//! Regression: a stationary click (press → release, no cursor movement)
//! on an inactive tab MUST NOT trigger a tab reorder via
//! `compute_action`.
//!
//! Bug report: with multiple tabs open, clicking on the FIRST tab in
//! the area BETWEEN the title text and the close button × did NOTHING.
//! Other tabs worked. Title clicks worked. × clicks worked. Only the
//! right-of-title-mid sliver on the first tab was dead.
//!
//! Root cause: the press handler correctly calls `tabs.activate(0)`
//! and seeds a `DragSession`. The release handler unconditionally
//! calls `compute_action`, which (since the cursor is still over the
//! source bar) entered the within-bar reorder branch. `drop_slot`
//! returns the index of the first tab whose midpoint exceeds `px` —
//! so a click on the RIGHT half of tab 0 (where the title/× gap
//! lives) returned `1`, triggering `ReorderTab { from: 0, to: 1 }`.
//! After `tabs.reorder(0, 1)`, the active tab `0` was moved to slot
//! `1`. Net visible effect: the user-clicked tab "stayed" at the
//! same on-screen slot (because the other tab swapped places with
//! it), so the user perceived "nothing happened." The last tab was
//! immune only because `drop_slot` returns `n` and gets clamped back
//! to `press_tab_index`.
//!
//! Fix: gate the reorder branch on `drag_moved_enough(session)` — a
//! sub-threshold movement is a click, not a drag, and a click must
//! not reorder. This matches Chrome/Firefox.

use sonicterm_app::tab_drag::{compute_action, DragAction, DragSession};
use sonicterm_ui::tabbar_view::TabBarLayout;
use sonicterm_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b
}

#[test]
fn stationary_click_in_first_tab_title_gap_does_not_reorder() {
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let t0 = &layout.tabs[0];
    // A point in the right half of tab 0 (specifically the gap between
    // the title text and the close button — past the midpoint). This
    // is exactly where `drop_slot` would, pre-fix, return slot 1.
    let gap_x = (t0.title_rect.x + t0.title_rect.w + t0.close_x_rect.x) * 0.5;
    let gap_y = t0.bg.y + t0.bg.h * 0.5;
    let session = DragSession::new(0, (gap_x, gap_y)); // current == press
    let action: DragAction<&str> = compute_action(&session, None, &layout);
    assert_eq!(
        action,
        DragAction::ReturnToOriginalBar,
        "stationary click must not reorder; got {action:?}"
    );
}

#[test]
fn stationary_click_on_any_tab_right_half_does_not_reorder() {
    // Generalize: every tab. The dispatcher must treat a press-release
    // without cursor movement as a no-op regardless of which tab and
    // which half got clicked.
    let bar = bar_with(4);
    let layout = TabBarLayout::compute(&bar, 1400.0);
    for idx in 0..layout.tabs.len() {
        let t = &layout.tabs[idx];
        // right-of-midpoint, inside bg, outside close
        let px = (t.title_rect.x + t.title_rect.w + t.close_x_rect.x) * 0.5;
        let py = t.bg.y + t.bg.h * 0.5;
        let s = DragSession::new(idx, (px, py));
        let a: DragAction<&str> = compute_action(&s, None, &layout);
        assert_eq!(
            a,
            DragAction::ReturnToOriginalBar,
            "tab {idx}: stationary click must not reorder; got {a:?}"
        );
    }
}

#[test]
fn drag_that_moves_far_enough_still_reorders() {
    // The fix MUST NOT regress real drag-to-reorder. Press on tab 0,
    // move cursor well past tab 1's midpoint, release — that's a real
    // drag and should produce ReorderTab { from: 0, to: 1 }.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let t0 = &layout.tabs[0];
    let t1 = &layout.tabs[1];
    let press = (t0.bg.x + t0.bg.w * 0.5, t0.bg.y + t0.bg.h * 0.5);
    let drop = (t1.bg.x + t1.bg.w * 0.5 - 5.0, t1.bg.y + t1.bg.h * 0.5);
    let mut s = DragSession::new(0, press);
    s.current_pos = drop;
    let a: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(a, DragAction::ReorderTab { from: 0, to: 1 });
}
