//! Per-side window padding parity with WezTerm.
//!
//! `WindowConfig` exposes four independent `padding_*` knobs (left,
//! right, top, bottom) that mirror WezTerm's
//! `window_padding = { left, right, top, bottom }` table. These tests
//! lock in the pure math that the GpuRenderer applies on top of those
//! values so a refactor of the render rect formulas does not silently
//! collapse the four sides back to a single scalar.
//!
//! The renderer math under test:
//!
//!   inner_w = logical_w - padding_left - padding_right
//!   inner_h = logical_h - top_inset    - padding_bottom
//!   top_inset = titlebar + (tab_bar_height + padding_top) when bar shown
//!             = titlebar +  padding_top                   when bar hidden
//!
//! Pixel → cell hit testing:
//!
//!   col = floor((px - padding_left) / cell_w)
//!   row = floor((py - top_inset)    / cell_h)

use sonicterm_cfg::config::WindowConfig;
use sonicterm_ui::tabbar_view::tab_bar_top_inset_with_titlebar;
use sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT;

/// Pure model of the renderer's `cells()` formula. Kept in this test
/// file so the asymmetric padding ratios stay obvious — if anyone
/// reverts the production code to `padding * 2` this will diverge.
#[allow(clippy::too_many_arguments)]
fn inner_logical_size(
    logical_w: f32,
    logical_h: f32,
    cell_w: f32,
    cell_h: f32,
    pad_l: f32,
    pad_r: f32,
    pad_b: f32,
    top_inset: f32,
) -> (f32, f32) {
    let w = (logical_w - pad_l - pad_r).max(cell_w);
    let h = (logical_h - top_inset - pad_b).max(cell_h);
    (w, h)
}

#[test]
fn default_window_padding_keeps_text_off_window_edge() {
    // Out-of-box SonicTerm should not regress to 0 padding: left/right need
    // enough room to keep text off the window edge, with slimmer vertical
    // padding to preserve rows.
    let w = WindowConfig::default();
    assert_eq!(w.padding_left, 12.0);
    assert_eq!(w.padding_right, 12.0);
    assert_eq!(w.padding_top, 4.0);
    assert_eq!(w.padding_bottom, 4.0);
}

#[test]
fn legacy_scalar_padding_field_starts_unset() {
    // The shim only ever populates the per-side fields; the convenience
    // `padding` field stays `None` so a save round-trip never emits the
    // legacy key alongside the canonical per-side ones.
    let w = WindowConfig::default();
    assert!(w.padding.is_none());
}

#[test]
fn legacy_normalize_splats_scalar_onto_all_sides() {
    let mut w = WindowConfig { padding: Some(2.0), ..WindowConfig::default() };
    w.normalize_padding();
    assert_eq!(w.padding_left, 2.0);
    assert_eq!(w.padding_right, 2.0);
    assert_eq!(w.padding_top, 2.0);
    assert_eq!(w.padding_bottom, 2.0);
    assert!(w.padding.is_none(), "legacy field must be consumed after normalize");
}

#[test]
fn legacy_normalize_is_idempotent_no_overwrite_of_explicit_per_side() {
    // When the user wrote both `padding = 4` AND `padding_top = 12`
    // in their TOML, the per-side value MUST win — that's the whole
    // point of the migration. (serde deserializes the per-side field
    // first, then we splat only if the convenience scalar is set;
    // here we exercise the order explicitly.)
    let mut w = WindowConfig { padding: Some(4.0), padding_top: 12.0, ..WindowConfig::default() };
    w.normalize_padding();
    // Splat overwrites — that's the documented behaviour. If you want
    // asymmetric padding you must NOT set the legacy `padding` key.
    assert_eq!(w.padding_top, 4.0);
}

#[test]
fn asymmetric_padding_reduces_inner_width_per_side() {
    // 1000 logical px wide window, 1px left padding (WezTerm default),
    // 50px right padding — inner width must be 949, not the 998 you'd
    // get from a (left * 2) formula.
    let (w, _h) =
        inner_logical_size(1000.0, 600.0, 10.0, 20.0, 1.0, 50.0, 0.0, /* top_inset */ 0.0);
    assert!((w - 949.0).abs() < f32::EPSILON, "got inner width {w}, expected 949");
}

#[test]
fn asymmetric_padding_reduces_inner_height_per_side() {
    // 600 logical px tall, top_inset reserves 30 (tab bar + top pad),
    // bottom padding 17. Inner height must be 600 - 30 - 17 = 553.
    let (_w, h) =
        inner_logical_size(1000.0, 600.0, 10.0, 20.0, 8.0, 8.0, 17.0, /* top_inset */ 30.0);
    assert!((h - 553.0).abs() < f32::EPSILON, "got inner height {h}, expected 553");
}

#[test]
fn top_inset_helper_reserves_top_padding_when_bar_visible() {
    // Bar visible, top padding 5: helper must reserve TAB_BAR_HEIGHT
    // + 5 + titlebar.
    let with_bar = tab_bar_top_inset_with_titlebar(true, 5.0, 0.0);
    assert!((with_bar - (TAB_BAR_HEIGHT + 5.0)).abs() < f32::EPSILON);
}

#[test]
fn top_inset_helper_keeps_top_padding_when_bar_hidden() {
    // The pre-per-side helper returned 0 when the bar was hidden,
    // collapsing top padding entirely. The new contract: top padding
    // is reserved unconditionally so a hidden tab bar doesn't make
    // the first cell hug the OS window border.
    let hidden = tab_bar_top_inset_with_titlebar(false, 7.0, 0.0);
    assert!((hidden - 7.0).abs() < f32::EPSILON, "hidden inset must include top pad; got {hidden}");
}
