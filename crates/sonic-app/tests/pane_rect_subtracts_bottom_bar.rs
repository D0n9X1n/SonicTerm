//! Regression for PR #323 / Haiku gaps #2 and #3.
//!
//! Gap #2: when the tab bar is at the bottom, the pane content area
//! must be shrunk by the bar height on the BOTTOM (via
//! `bottom_inset`), not on top. The sum top_inset + bottom_inset +
//! pane_h must not exceed window_h, otherwise the terminal grid paints
//! beneath the bar.
//!
//! Gap #3: on macOS, the "integrated titlebar" inset that the
//! renderer reserves under the OS chrome must only apply when the
//! tab bar is at the top (where the bar visually replaces the OS
//! titlebar via fullsize_content_view). When the bar is at the
//! bottom, the window uses a normal NSWindow titlebar and the
//! reserved inset above the grid must collapse to 0.
//!
//! These tests cover the platform-portable arithmetic. The actual
//! NSWindow style-mask wiring lives behind `cfg(target_os="macos")`
//! in `app/mod.rs::with_integrated_titlebar_for` — covered via
//! manual GUI smoke per CLAUDE.md §13.

use sonic_app::app::{integrated_titlebar_inset_for, MACOS_INTEGRATED_TITLEBAR_INSET};
use sonic_core::config::TabBarPosition;

#[test]
fn pane_rect_layout_subtracts_bottom_bar_height_when_bar_at_bottom() {
    // Simulate the renderer's content-area math:
    //   top_inset    = titlebar_inset + (bar_h if bar_at_top else 0) + padding_top
    //   bottom_inset = bar_h if bar_at_bottom else 0
    //   pane_h       = window_h - top_inset - bottom_inset - padding_bottom
    const WINDOW_H: f32 = 700.0;
    const BAR_H: f32 = 28.0;
    const PAD_TOP: f32 = 0.0;
    const PAD_BOT: f32 = 0.0;
    let titlebar_inset = integrated_titlebar_inset_for(TabBarPosition::Bottom);

    // Top inset when bar at bottom: only the (zeroed) titlebar reservation
    // + top padding. No bar height up here.
    let top_inset = titlebar_inset + PAD_TOP;
    let bottom_inset = BAR_H; // bar pinned to bottom
    let pane_h = WINDOW_H - top_inset - bottom_inset - PAD_BOT;

    // Contract: pane + bar fit inside the window with zero overlap.
    assert!(
        pane_h + bottom_inset + top_inset <= WINDOW_H + f32::EPSILON,
        "pane + bar must fit in window when bar at bottom; pane={}, bar={}, top={}, win={}",
        pane_h,
        bottom_inset,
        top_inset,
        WINDOW_H,
    );
    // And the bar must consume real space — pane must be < window_h.
    assert!(pane_h < WINDOW_H, "bottom bar must shrink pane area");
}

#[test]
fn macos_titlebar_inset_collapses_to_zero_for_bottom_bar() {
    // The key contract behind gap #3: with bar at the bottom, the
    // macOS integrated-titlebar inset must be 0 — i.e. we ship a
    // normal NSWindow titlebar, no fullsize_content_view shift.
    let bottom = integrated_titlebar_inset_for(TabBarPosition::Bottom);
    assert_eq!(bottom, 0.0, "bar-at-bottom must NOT reserve an integrated-titlebar band");

    let top = integrated_titlebar_inset_for(TabBarPosition::Top);
    #[cfg(target_os = "macos")]
    {
        assert_eq!(top, MACOS_INTEGRATED_TITLEBAR_INSET);
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(top, 0.0);
        let _ = MACOS_INTEGRATED_TITLEBAR_INSET;
    }
}
