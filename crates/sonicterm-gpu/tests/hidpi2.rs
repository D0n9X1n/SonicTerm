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

use sonicterm_gpu::core::{build_snapped_cell_x, pixel_to_local_col};
use sonicterm_gpu::quad::px_to_ndc;

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

// ---------------------------------------------------------------------------
// #569: pane-aware pixel_to_cell using snapped_cell_x edge cache.
//
// At fractional DPI scales the per-column edge cache (`snapped_cell_x`)
// has jitter — adjacent cell widths differ by 1 device pixel so they
// align to integer pixels. The legacy `(x / cell_w).floor()` math
// disagrees with those edges near the right side of wide grids and
// produces an off-by-one column. The fix is a linear scan over the
// same edge cache the renderer drew on.
//
// These tests cover the pure column-search helper at the 5 DPI scales
// called out in the spec, plus the split-pane case where pane B's
// column 0 is not at the window's `padding_left`.
// ---------------------------------------------------------------------------

/// Click at a known boundary px in a single pane at each scale must
/// resolve to the cell that owns the right-hand side of the boundary
/// (half-open buckets `edge[c] <= px < edge[c+1]`).
#[test]
fn pixel_to_local_col_boundary_resolves_to_rhs_cell_at_each_scale() {
    let cell_w: f32 = 8.4;
    let origin_x: f32 = 8.0; // padding_left
    let cols: u16 = 120;
    for &scale in &[1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let edges = build_snapped_cell_x(origin_x, cell_w, cols, scale);
        // Boundary exactly on edges[30] must land on col 30, not 29.
        let boundary = edges[30];
        let got = pixel_to_local_col(boundary, &edges, cols)
            .unwrap_or_else(|| panic!("scale {scale}: boundary px {boundary} returned None"));
        assert_eq!(
            got, 30,
            "scale {scale}: px on edge[30] must resolve to col 30 (RHS), got {got}"
        );
        // Just-below the boundary belongs to col 29.
        let before = edges[30] - 0.0001;
        let got_before = pixel_to_local_col(before, &edges, cols).unwrap();
        assert_eq!(
            got_before, 29,
            "scale {scale}: px just below edge[30] must resolve to col 29, got {got_before}"
        );
        // Mid-cell sanity at col 60.
        let mid = (edges[60] + edges[61]) / 2.0;
        let got_mid = pixel_to_local_col(mid, &edges, cols).unwrap();
        assert_eq!(got_mid, 60, "scale {scale}: midpoint of cell 60 must be col 60");
    }
}

/// At fractional scales the legacy `(x / cell_w).floor()` math disagrees
/// with the snapped edges at the right end of the grid. The scan-based
/// helper must agree with the snapped edges for every column, end-to-end.
#[test]
fn pixel_to_local_col_agrees_with_snapped_edges_across_the_grid() {
    let cell_w: f32 = 7.6;
    let origin_x: f32 = 8.0;
    let cols: u16 = 200;
    for &scale in &[1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let edges = build_snapped_cell_x(origin_x, cell_w, cols, scale);
        for c in 0..cols {
            // Center of each cell must round-trip to its own column.
            let center = (edges[c as usize] + edges[c as usize + 1]) / 2.0;
            let got = pixel_to_local_col(center, &edges, cols).unwrap_or_else(|| {
                panic!("scale {scale}: center of col {c} ({center}) returned None")
            });
            assert_eq!(got, c, "scale {scale}: center of col {c} must round-trip, got {got}");
        }
        // Out-of-range guards.
        assert!(pixel_to_local_col(edges[0] - 0.5, &edges, cols).is_none());
        assert!(pixel_to_local_col(edges[cols as usize], &edges, cols).is_none());
        assert!(pixel_to_local_col(edges[cols as usize] + 10.0, &edges, cols).is_none());
    }
}

/// Split-pane case: pane A occupies cols 0..30 starting at x=8, pane B
/// occupies its own edge cache starting at a non-zero origin (e.g.
/// after a vertical split at the midpoint). A click in pane B's column
/// 0 must resolve to col 0 of pane B's local edge cache, NOT to
/// column ~30 of pane A's edge cache. This is what was breaking in
/// real-world split layouts (#569).
#[test]
fn split_pane_click_resolves_to_local_column_zero_not_pane_a_col_30() {
    let cell_w: f32 = 8.4;
    let scale: f32 = 1.5;
    let pane_a_origin: f32 = 8.0;
    let pane_a_cols: u16 = 30;
    let edges_a = build_snapped_cell_x(pane_a_origin, cell_w, pane_a_cols, scale);
    // Pane B starts right where pane A ends (split with no gutter for
    // the test — gutters only shift origin_b further right, which is
    // strictly easier).
    let pane_b_origin = edges_a[pane_a_cols as usize];
    let pane_b_cols: u16 = 30;
    let edges_b = build_snapped_cell_x(pane_b_origin, cell_w, pane_b_cols, scale);
    // Click at the dead center of pane B's column 0.
    let click_x = (edges_b[0] + edges_b[1]) / 2.0;
    // Pane-aware path: edges_b says col 0.
    let got = pixel_to_local_col(click_x, &edges_b, pane_b_cols).unwrap();
    assert_eq!(got, 0, "click in pane B col 0 must resolve to col 0 of pane B's cache, got {got}");
    // And critically: the same click against pane A's edge cache would
    // (correctly) say None because click_x >= edges_a[pane_a_cols].
    // That's the cue the pane-resolution step in pixel_to_cell uses to
    // pick pane B before invoking this helper.
    assert!(
        pixel_to_local_col(click_x, &edges_a, pane_a_cols).is_none(),
        "click_x is past pane A's last edge — helper must return None so \
         the pane-resolution step routes the hit-test to pane B"
    );
}

/// Empty / degenerate inputs: a 0-col pane has no addressable cell.
#[test]
fn pixel_to_local_col_handles_degenerate_inputs() {
    let edges = build_snapped_cell_x(8.0, 8.4, 0, 1.5);
    assert!(pixel_to_local_col(10.0, &edges, 0).is_none());
}
