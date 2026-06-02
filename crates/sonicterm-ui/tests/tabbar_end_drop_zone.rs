//! Issue #541: the tab bar reserves a drop zone at its right edge so the
//! user can drop a dragged tab "after the last tab" (insertion slot
//! `tabs.len()`). The bar background still extends the full window width;
//! only the per-tab allocation shrinks.

use sonicterm_ui::tabbar_view::*;
use sonicterm_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b
}

#[test]
fn end_drop_zone_constant_is_positive() {
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(TAB_END_DROP_ZONE_PX > 0.0, "reserved end drop zone must be > 0");
    }
}

#[test]
fn reserved_zone_exists_between_last_tab_and_bar_right() {
    // With a few tabs and a roomy window, the last tab's right edge
    // must sit well inside the bar so a cursor at `last.right + 30`
    // is still "over the bar" but past every tab.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let last = layout.tabs.last().unwrap();
    let bar_right = layout.bar.x + layout.bar.w;
    let last_right = last.bg.x + last.bg.w;
    assert!(
        last_right + 30.0 < bar_right,
        "reserved end zone too small: last_right={last_right}, bar_right={bar_right}"
    );
    // And the bar still spans the full window width.
    assert!((layout.bar.w - 1200.0).abs() < 0.01, "bar width must be full window width");
}

#[test]
fn reserved_zone_exists_even_when_tabs_overflow_pressure() {
    // Many tabs => per-tab shrinks, but the reserved zone is taken off
    // the front of the budget so it still exists.
    let bar = bar_with(20);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let last = layout.tabs.last().unwrap();
    let bar_right = layout.bar.x + layout.bar.w;
    let last_right = last.bg.x + last.bg.w;
    assert!(
        last_right + 30.0 < bar_right,
        "reserved end zone collapsed under many-tab pressure: last_right={last_right}, bar_right={bar_right}"
    );
}

#[test]
fn drop_slot_in_reserved_zone_resolves_to_append_index() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let last = layout.tabs.last().unwrap();
    let probe_x = last.bg.x + last.bg.w + 30.0;
    let probe_y = layout.bar.y + layout.bar.h / 2.0;

    // Hit must still resolve to "over the bar" so the cross-window
    // merge / within-bar reorder path is entered.
    assert!(
        layout.point_over_bar(probe_x, probe_y),
        "probe ({probe_x}, {probe_y}) must be over the (full-width) bar"
    );
    // And the insertion slot is `tabs.len()` (append).
    assert_eq!(
        layout.drop_slot(probe_x, probe_y),
        layout.tabs.len(),
        "drop in reserved end zone must produce append-insertion slot"
    );
}

#[test]
fn same_window_reorder_index_converts_slot_len_to_len_minus_one() {
    // tab_drag::compute_action clamps insertion-slot `n` to tab-vec
    // index `n - 1` so a within-bar reorder dropping into the reserved
    // zone produces `to = len - 1` (append to end). We don't pull in
    // sonicterm-app here (cyclic) — we just verify the layout half of
    // the contract: drop_slot returns `n`, and the documented clamp
    // `to = raw_slot.min(n - 1)` lands on `n - 1`.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1200.0);
    let last = layout.tabs.last().unwrap();
    let probe_x = last.bg.x + last.bg.w + 30.0;
    let probe_y = layout.bar.y + layout.bar.h / 2.0;

    let n = layout.tabs.len();
    let raw_slot = layout.drop_slot(probe_x, probe_y);
    assert_eq!(raw_slot, n);
    let to = raw_slot.min(n - 1);
    assert_eq!(to, n - 1, "same-window reorder must produce index len-1");
}
