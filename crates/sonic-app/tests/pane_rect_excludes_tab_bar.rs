//! Regression for the `tab_bar_position` plumbing: when the bar lives
//! at the BOTTOM of the window, the pane area MUST NOT overlap the bar
//! strip — i.e. the same vertical span is reserved, just at the
//! opposite edge of the inner content area.
//!
//! We can't construct a real `GpuRenderer` in a unit test (no surface),
//! so this asserts the algebraic invariant that the renderer's
//! `top_inset + grid_h + bottom_inset + padding_bottom == window_h`
//! identity holds under both placements.

use sonic_core::config::TabBarPosition;
use sonic_ui::tabbar_view::tab_bar_height;

#[derive(Clone, Copy)]
struct Insets {
    top: f32,
    bottom: f32,
}

/// Mirror of the renderer's `top_inset` / `bottom_inset` formulas.
/// Kept simple — `padding_top` is folded into `top` so this matches
/// the geometry the grid actually sees.
fn insets(
    pos: TabBarPosition,
    tab_bar_visible: bool,
    titlebar_inset: f32,
    padding_top: f32,
    font_size: f32,
) -> Insets {
    let bar_h = tab_bar_height(font_size);
    match pos {
        TabBarPosition::Top => {
            let bar = if tab_bar_visible { bar_h + padding_top } else { padding_top };
            Insets { top: titlebar_inset + bar, bottom: 0.0 }
        }
        TabBarPosition::Bottom => Insets {
            top: titlebar_inset + padding_top,
            bottom: if tab_bar_visible { bar_h } else { 0.0 },
        },
    }
}

const WIN_H: f32 = 720.0;
const TITLE: f32 = 0.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 4.0;
const FONT: f32 = 14.0;

#[test]
fn bar_height_reserved_under_both_positions() {
    let bar_h = tab_bar_height(FONT);
    let top = insets(TabBarPosition::Top, true, TITLE, PAD_TOP, FONT);
    let bot = insets(TabBarPosition::Bottom, true, TITLE, PAD_TOP, FONT);
    let total_top = top.top + top.bottom;
    let total_bot = bot.top + bot.bottom;
    assert!(
        (total_top - total_bot).abs() < f32::EPSILON,
        "reserved space differs between top ({total_top}) and bottom ({total_bot})"
    );
    // The reserved bar height shows up either in `top` (Top placement)
    // or in `bottom` (Bottom placement), not both.
    assert!(top.bottom == 0.0 && top.top >= bar_h);
    assert!(bot.top < bar_h && bot.bottom >= bar_h);
}

#[test]
fn pane_rect_does_not_overlap_bar_when_bottom() {
    let in_bot = insets(TabBarPosition::Bottom, true, TITLE, PAD_TOP, FONT);
    let pane_top = in_bot.top;
    let pane_bottom = WIN_H - in_bot.bottom - PAD_BOTTOM;
    let bar_top = WIN_H - tab_bar_height(FONT);
    assert!(
        pane_bottom <= bar_top + f32::EPSILON,
        "pane bottom {pane_bottom} overlaps bar top {bar_top}"
    );
    assert!(pane_top >= 0.0 && pane_top < pane_bottom);
}

#[test]
fn pane_rect_does_not_overlap_bar_when_top() {
    let in_top = insets(TabBarPosition::Top, true, TITLE, PAD_TOP, FONT);
    let pane_top = in_top.top;
    let pane_bottom = WIN_H - in_top.bottom - PAD_BOTTOM;
    let bar_bottom = tab_bar_height(FONT) + PAD_TOP;
    assert!(
        pane_top >= bar_bottom - f32::EPSILON,
        "pane top {pane_top} overlaps bar bottom {bar_bottom}"
    );
    assert!(pane_bottom > pane_top);
}

#[test]
fn hidden_bar_reserves_no_extra_space_either_way() {
    let top = insets(TabBarPosition::Top, false, TITLE, PAD_TOP, FONT);
    let bot = insets(TabBarPosition::Bottom, false, TITLE, PAD_TOP, FONT);
    assert_eq!(top.bottom, 0.0);
    assert_eq!(bot.bottom, 0.0);
    assert!((top.top - bot.top).abs() < f32::EPSILON);
}
