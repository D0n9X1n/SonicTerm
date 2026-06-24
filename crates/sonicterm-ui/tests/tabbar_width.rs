//! Tab bar honours the configurable maximum tab width.
//!
//! These run in one `#[test]` because the active max width is process-global
//! state; splitting them into separate tests would let the parallel test
//! runner interleave the `set_max_tab_width` calls and observe each other's
//! values. Within a single test the steps run in order.

use sonicterm_ui::tabbar_view::{max_tab_width, set_max_tab_width, TabBarLayout, TAB_MAX_WIDTH};
use sonicterm_ui::tabs::{Tab, TabBar};

fn two_tab_bar() -> TabBar {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("one"));
    tabs.push(Tab::new("two"));
    tabs
}

/// In a window wide enough that the equal share exceeds the cap, each tab's
/// background width is pinned to the active max width at scale 1.0.
fn per_tab_width(bar: &TabBar) -> f32 {
    // bar_height 40.0 -> scale 1.0, so per-tab width caps at exactly the
    // configured max width in logical px.
    let layout = TabBarLayout::compute_with_height(bar, 4000.0, 40.0);
    layout.tabs.first().expect("at least one tab laid out").bg_rect.w
}

#[test]
fn configurable_max_tab_width_takes_effect() {
    let bar = two_tab_bar();

    // Default: the active value equals the built-in constant and the layout
    // caps tabs at it.
    set_max_tab_width(TAB_MAX_WIDTH);
    assert_eq!(max_tab_width(), TAB_MAX_WIDTH);
    assert!(
        (per_tab_width(&bar) - TAB_MAX_WIDTH).abs() < 0.5,
        "default cap should pin a wide-window tab to {TAB_MAX_WIDTH}"
    );

    // Wider: raising the cap lets each tab grow to the new value.
    set_max_tab_width(400.0);
    assert_eq!(max_tab_width(), 400.0);
    assert!(
        (per_tab_width(&bar) - 400.0).abs() < 0.5,
        "raised cap should widen the tab to 400"
    );

    // Narrower: lowering the cap shrinks tabs back down.
    set_max_tab_width(120.0);
    assert!(
        (per_tab_width(&bar) - 120.0).abs() < 0.5,
        "lowered cap should narrow the tab to 120"
    );

    // Invalid values (non-finite, non-positive) are ignored, leaving the
    // last good value in place so a bad config never collapses the bar.
    set_max_tab_width(f32::NAN);
    set_max_tab_width(0.0);
    set_max_tab_width(-50.0);
    assert_eq!(max_tab_width(), 120.0, "invalid widths must be rejected");

    // Restore the default so other tests in the binary are unaffected.
    set_max_tab_width(TAB_MAX_WIDTH);
}
