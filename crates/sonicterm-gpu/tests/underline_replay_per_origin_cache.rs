//! Regression test for PR #532 Step-4 REVISE blocker.
//!
//! Pre-fix: the underline emit loop in `core.rs` built per-origin
//! snapped-edge caches sized to the ACTIVE pane's `grid.cols`. When a
//! wider INACTIVE pane contributed an underline whose `col_b` reached
//! past the active pane's width, `cache.len().saturating_sub(1)`
//! clamped the lookup index and the resulting `w` was truncated — the
//! underline visually stopped at the active pane's right edge.
//!
//! Post-fix (option (a)): each underline entry carries its originating
//! pane's column count, the cache is keyed by `(pad_bits, pane_cols)`,
//! and the cache is sized from the originating `pane_cols`. This test
//! reproduces the geometry of the per-origin cache build + lookup at
//! the same scale factor (1.5) where #470's snapping matters.

use sonicterm_gpu::core::build_snapped_cell_x;

/// Mirror of the (post-fix) per-origin cache logic in `core.rs`'s
/// underline emit loop. Kept here as a focused harness so the test
/// validates the sizing contract independently of the full render
/// pipeline.
fn lookup_underline_w(
    origin_x: f32,
    pane_cols: u16,
    cell_w: f32,
    scale: f32,
    col_a: u16,
    col_b: u16,
) -> f32 {
    let cache = build_snapped_cell_x(origin_x, cell_w, pane_cols, scale);
    let end_exclusive = (col_b as usize).saturating_add(1);
    let cache_end = end_exclusive.min(cache.len().saturating_sub(1));
    let col_a_usize = (col_a as usize).min(cache_end);
    let x = cache.get(col_a_usize).copied().unwrap();
    let x_end = cache.get(cache_end).copied().unwrap();
    x_end - x
}

#[test]
fn inactive_wider_pane_underline_not_truncated() {
    // Layout: active pane has 40 cols; an inactive pane to the right is
    // 80 cols wide. Cell width 10 px, scale 1.5 (fractional DPI where
    // #470 snapping is non-trivial). The inactive pane's underline
    // covers cols 50..=70 — well past active.cols=40.
    let cell_w = 10.0;
    let scale = 1.5;
    let active_cols: u16 = 40;
    let inactive_cols: u16 = 80;
    let origin_x = 200.0; // arbitrary pane pad
    let (col_a, col_b) = (50u16, 70u16);

    // Pre-fix behaviour (BUG): cache sized to active.cols. The lookup
    // clamps to the cache end and the width collapses to (nearly) zero
    // because both col_a_usize and cache_end land at the last cache
    // slot.
    let buggy_w = lookup_underline_w(origin_x, active_cols, cell_w, scale, col_a, col_b);
    // Post-fix behaviour: cache sized to the originating pane's cols.
    // Width should equal ~21 cells (col_b - col_a + 1) of cell_w, up to
    // device-pixel snap rounding.
    let fixed_w = lookup_underline_w(origin_x, inactive_cols, cell_w, scale, col_a, col_b);

    let expected_cells = f32::from(col_b - col_a + 1);
    let expected_w_nominal = expected_cells * cell_w; // 210.0
                                                      // Snap rounding budget: ≤ 2 device pixels (one at each edge),
                                                      // converted back to logical px via /scale.
    let snap_budget = 2.0 / scale;

    assert!(
        (fixed_w - expected_w_nominal).abs() <= snap_budget,
        "post-fix underline width should be ~{expected_w_nominal} logical px (got {fixed_w})"
    );
    // Sanity: the bug would have produced something far smaller than
    // the true width. If this assertion ever stops holding, the
    // pre-fix code path is gone for a different reason and the
    // regression test should be re-evaluated.
    assert!(
        buggy_w < fixed_w * 0.5,
        "pre-fix (active.cols-sized cache) should truncate width vs. post-fix \
         (buggy_w={buggy_w}, fixed_w={fixed_w})"
    );
}

#[test]
fn per_origin_cache_keys_separate_inactive_widths() {
    // Two panes share the SAME pad (e.g. stacked vertically) but have
    // different col counts. The (pad_bits, pane_cols) key must keep
    // them in separate cache slots so the narrower pane never sees
    // the wider pane's snapped edges (and vice versa).
    let cell_w = 10.0;
    let scale = 1.5;
    let origin_x = 0.0;
    let cache_narrow = build_snapped_cell_x(origin_x, cell_w, 20, scale);
    let cache_wide = build_snapped_cell_x(origin_x, cell_w, 80, scale);
    assert_eq!(cache_narrow.len(), 21);
    assert_eq!(cache_wide.len(), 81);
    // Shared prefix matches up to the narrower pane's length — caches
    // are interchangeable in that range but the wider cache must
    // extend further so an underline at col 70 has a valid right edge.
    for i in 0..cache_narrow.len() {
        assert!((cache_narrow[i] - cache_wide[i]).abs() < 1e-3);
    }
}
