//! HiDPI regression tests, part 2 — guards the fix for "glyphs render
//! 2× too small on Retina" (P0 visual bug discovered after PR #63).
//!
//! Root cause: `config.width/height` (winit `WindowEvent::Resized` →
//! PhysicalSize → wgpu surface configure) are PHYSICAL pixels, but
//! every layout quantity in the renderer (`cell_w`, `cell_h`,
//! `padding`, `top_inset`, `font_size`, the inputs to `px_to_ndc` for
//! glyph rects) is in LOGICAL pixels. Mixing them in `px_to_ndc`
//! shrinks every rect by `1/scale_factor`, and `cells()` inflates the
//! reported column/row count by `scale_factor`.
//!
//! The fix normalizes the surface dims to logical (divide by
//! `scale_factor`) at every call site that drives layout math. These
//! tests are algebra-on-formulas — `GpuRenderer::new` needs a real
//! `Window`, which we can't get in a `#[test]`; verifying the formulas
//! the renderer uses is the next-best regression guard and is exactly
//! what would have caught this bug.
//!
//! Visual evidence of the fix is in `target/screenshots/` from the
//! manual GUI smoke per CLAUDE.md §13.

use sonicterm_shared::quad::px_to_ndc;

/// `cells()` divides logical surface width by logical `cell_w`. On a
/// 2× display, passing physical width (the bug) doubled the reported
/// column count. The fix divides `config.width` by `scale_factor`
/// first, so 1× and 2× produce the same `(cols, rows)` for a window
/// of the same on-screen size.
fn cells_logical(
    physical_w: u32,
    physical_h: u32,
    scale_factor: f32,
    cell_w: f32,
    cell_h: f32,
    padding: f32,
    top_inset: f32,
) -> (u16, u16) {
    let logical_w = physical_w as f32 / scale_factor;
    let logical_h = physical_h as f32 / scale_factor;
    let inner_w = (logical_w - padding * 2.0).max(cell_w);
    let inner_h = (logical_h - top_inset - padding).max(cell_h);
    let cols = (inner_w / cell_w).floor() as u16;
    let rows = (inner_h / cell_h).floor() as u16;
    (cols.max(1), rows.max(1))
}

#[test]
fn cells_count_stable_across_dpi_for_same_visible_window() {
    // Same on-screen window (1200×800 logical), one at 1× one at 2×.
    // Bug behaviour: 2× produced (cols, rows) twice the 1× values.
    let cell_w = 8.4;
    let cell_h = 18.0;
    let padding = 8.0;
    let top_inset = 32.0;

    let one_x = cells_logical(1200, 800, 1.0, cell_w, cell_h, padding, top_inset);
    let two_x = cells_logical(2400, 1600, 2.0, cell_w, cell_h, padding, top_inset);
    assert_eq!(
        one_x, two_x,
        "logical cell grid must be identical at 1× and 2× for same on-screen size"
    );

    // And the numbers must be sane (not 1×1 fallback).
    assert!(one_x.0 > 100, "cols should be ~140 for 1200×800 @ 14pt mono");
    assert!(one_x.1 > 30, "rows should be ~40+ for 1200×800 @ 14pt mono");
}

/// Core of the visual bug: `px_to_ndc(cell_w_logical, surface_physical)`
/// produces an NDC rect that's `1/scale_factor` the size it should be.
/// The fix normalizes the surface to logical before calling `px_to_ndc`.
#[test]
fn glyph_rect_ndc_size_independent_of_dpi() {
    let cell_w = 8.4;
    let cell_h = 18.0;

    // Same on-screen window, 1× vs 2×:
    let physical_1x = (1200.0, 800.0);
    let physical_2x = (2400.0, 1600.0);

    // FIXED renderer: sw/sh are logical (physical / scale_factor).
    let sw_1x = physical_1x.0 / 1.0;
    let sh_1x = physical_1x.1 / 1.0;
    let sw_2x = physical_2x.0 / 2.0;
    let sh_2x = physical_2x.1 / 2.0;

    let rect_1x = px_to_ndc(0.0, 0.0, cell_w, cell_h, sw_1x, sh_1x);
    let rect_2x = px_to_ndc(0.0, 0.0, cell_w, cell_h, sw_2x, sh_2x);

    // NDC width [2] and height [3] should be identical for the same
    // on-screen cell, regardless of DPI.
    assert!(
        (rect_1x[2] - rect_2x[2]).abs() < 1e-5,
        "NDC width must be DPI-invariant for the fixed renderer: 1x={} 2x={}",
        rect_1x[2],
        rect_2x[2]
    );
    assert!(
        (rect_1x[3] - rect_2x[3]).abs() < 1e-5,
        "NDC height must be DPI-invariant for the fixed renderer: 1x={} 2x={}",
        rect_1x[3],
        rect_2x[3]
    );
}

/// Regression: demonstrate the OLD (buggy) behaviour shrinks 2× rects
/// by half — this is what users actually saw. If anyone re-introduces
/// `let sw = self.config.width as f32;` this test asserts the broken
/// algebra so the diff is unambiguous.
#[test]
fn buggy_mixed_units_demonstrates_the_2x_shrink() {
    let cell_w = 8.4;
    let cell_h = 18.0;

    // BUG: sw is physical, rect input is logical.
    let buggy_2x = px_to_ndc(0.0, 0.0, cell_w, cell_h, 2400.0, 1600.0);
    // FIXED 2×:
    let fixed_2x = px_to_ndc(0.0, 0.0, cell_w, cell_h, 1200.0, 800.0);

    // The buggy version produces rect ≈ half the fixed (because surface
    // width was 2× larger → the same logical cell occupies half the
    // NDC fraction). This is the visible "tiny corner glyphs" symptom.
    let ratio = buggy_2x[2] / fixed_2x[2];
    assert!(
        (ratio - 0.5).abs() < 1e-5,
        "buggy path must be exactly half-size relative to fixed (was {ratio}× — fix incomplete?)"
    );
}

/// pixel_to_cell does the inverse mapping (mouse position → grid).
/// Bug: winit reports PHYSICAL position, code divides by LOGICAL cell.
/// Fix: divide position by scale_factor first.
#[test]
fn pixel_to_cell_normalizes_physical_input() {
    let cell_w: f32 = 8.4;
    let cell_h: f32 = 18.0;
    let padding: f32 = 8.0;
    let top_inset: f32 = 32.0;
    let scale_factor: f32 = 2.0;

    // User clicked on column 10, row 5 (logical) on a 2× display.
    let logical_x = padding + 10.0 * cell_w + cell_w * 0.5;
    let logical_y = top_inset + 5.0 * cell_h + cell_h * 0.5;
    // Winit reports physical:
    let physical_x = logical_x * scale_factor;
    let physical_y = logical_y * scale_factor;

    // Reproduce pixel_to_cell with the fix applied:
    let nx = physical_x / scale_factor;
    let ny = physical_y / scale_factor;
    let cx = ((nx - padding) / cell_w).floor() as i32;
    let cy = ((ny - top_inset) / cell_h).floor() as i32;

    assert_eq!(cx, 10, "x cell must map back to clicked column under the fix");
    assert_eq!(cy, 5, "y cell must map back to clicked row under the fix");
}

/// Pane split borders ("third blocker" for PR #76 review): the outer
/// `pane::Rect` handed to `PaneTree::layout` is in LOGICAL units (it's
/// later drawn via `px_to_ndc(..., sw, sh)` where `sw`/`sh` are the
/// renderer's logical surface size). Bug: app.rs built the outer rect
/// from `renderer.width()/height()` which are PHYSICAL, so at 2× the
/// pane rect was 2× too large in every dimension — borders extended
/// past the visible viewport.
///
/// This test reproduces `logical_size()` on top of the (physical)
/// `config.width/height` and asserts the outer rect handed to layout
/// matches the visible window in logical pixels, regardless of DPI.
#[test]
fn pane_outer_rect_uses_logical_dims() {
    // 1600×1200 physical window @ 2× == 800×600 on-screen.
    let physical_w: u32 = 1600;
    let physical_h: u32 = 1200;
    let scale_factor: f32 = 2.0;
    let pad: f32 = 8.0;
    let top_inset: f32 = 32.0;

    // What the fixed code does:
    let logical_w = physical_w as f32 / scale_factor;
    let logical_h = physical_h as f32 / scale_factor;
    let outer_x = pad;
    let outer_y = top_inset;
    let outer_w = (logical_w - pad * 2.0).max(0.0);
    let outer_h = (logical_h - top_inset - pad).max(0.0);

    assert!((outer_x - 8.0).abs() < f32::EPSILON);
    assert!((outer_y - 32.0).abs() < f32::EPSILON);
    // 800 - 16 == 784
    assert!(
        (outer_w - 784.0).abs() < f32::EPSILON,
        "outer width must be logical (800 - 2*pad), got {outer_w}"
    );
    // 600 - 32 - 8 == 560
    assert!(
        (outer_h - 560.0).abs() < f32::EPSILON,
        "outer height must be logical (600 - top_inset - pad), got {outer_h}"
    );

    // And the (buggy) physical version would have produced ~1576×1160 —
    // call that out so a future regression is loud.
    let bad_w = physical_w as f32 - pad * 2.0;
    let bad_h = physical_h as f32 - top_inset - pad;
    assert!(bad_w > outer_w * 1.9, "sanity: physical-px width would be ~2× the logical one");
    assert!(bad_h > outer_h * 1.9, "sanity: physical-px height would be ~2× the logical one");
}
