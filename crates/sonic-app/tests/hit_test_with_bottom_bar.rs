//! Regression for PR #323 / Haiku gap #1.
//!
//! When the tab bar is positioned at the bottom of the window, the
//! main-window hit-test code in `app/window_event.rs` must build
//! `TabBarLayout` using the renderer's **position-aware**
//! `tab_bar_y_offset()` (which returns `window_h - bar_h` for Bottom)
//! instead of the raw `titlebar_inset()` (always the top reservation).
//!
//! Pre-fix the layout was always anchored to the titlebar inset, so a
//! click at y = window_h - 10 (right in the bar's actual on-screen
//! rect) missed the bar entirely and was routed to the pane content
//! beneath.
//!
//! This test exercises the layout primitive directly with both anchors
//! and pins the contract that the Bottom anchor catches the
//! bottom-of-window click.

use sonic_ui::tabbar_view::{tab_bar_height, TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

const WINDOW_W: f32 = 1000.0;
const WINDOW_H: f32 = 700.0;
const FONT_SIZE: f32 = 14.0;

fn one_tab_bar() -> TabBar {
    let mut bar = TabBar::default();
    bar.push(Tab::new("shell"));
    bar
}

#[test]
fn click_in_bottom_strip_hits_tab_when_bar_anchored_to_bottom() {
    let tabs = one_tab_bar();
    let bar_h = tab_bar_height(FONT_SIZE);
    let bottom_y_offset = (WINDOW_H - bar_h).max(0.0);

    let layout = TabBarLayout::compute_with_height(&tabs, WINDOW_W, bar_h)
        .with_top_offset(bottom_y_offset)
        .with_visible(true);

    // Click 10 px above the very bottom edge — squarely inside the
    // on-screen bar strip when the bar is bottom-pinned.
    let hit = layout.hit(60.0, WINDOW_H - 10.0);
    assert!(
        matches!(hit, Some(TabHit::Activate(0))),
        "bottom-anchored layout must hit the tab strip near the window's bottom edge; got {:?}",
        hit
    );

    // Click near the top of the window — outside the bar strip.
    let miss = layout.hit(60.0, 4.0);
    assert!(
        miss.is_none(),
        "bottom-anchored layout must NOT hit the bar near the window's top edge; got {:?}",
        miss
    );
}

#[test]
fn click_at_top_with_top_anchored_layout_hits_bar_as_before() {
    // Sanity: Top-anchored behavior unchanged — this is the pre-#323
    // default path.
    let tabs = one_tab_bar();
    let bar_h = tab_bar_height(FONT_SIZE);

    let layout = TabBarLayout::compute_with_height(&tabs, WINDOW_W, bar_h)
        .with_top_offset(0.0)
        .with_visible(true);

    let hit = layout.hit(60.0, 4.0);
    assert!(matches!(hit, Some(TabHit::Activate(0))));
}
