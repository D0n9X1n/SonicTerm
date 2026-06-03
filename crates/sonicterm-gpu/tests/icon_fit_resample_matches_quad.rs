//! #610d revise (Haiku Step-5 Blocker 2): end-to-end invariant proof
//! that the atlas TILE produced by `flush_shape_run`'s IconCellFit
//! resample path is 1:1 with the production QUAD emitted by
//! `apply_symbol_fit(IconCellFit)`. Pre-fix the resampler preserved
//! aspect ratio while the quad filled `cell_w` × `ICON_FIT_TARGET *
//! cell_h` without aspect preservation — the Nearest sampler then
//! over-stretched the narrow tile horizontally (the residual the user
//! reported as "horizontal stretch" in #610d).
//!
//! These tests drive both halves of the invariant against the SAME
//! (cell_box_w, cell_h, scale_factor) inputs and assert equality —
//! catching any future drift in either the resample-target derivation
//! (`core::icon_fit_resample_target_for_test`) or the quad emission
//! (`swash_rasterizer::apply_symbol_fit`).
//!
//! Negative case: an ASCII codepoint classifies as `Natural` → the
//! resample helper would never be called, proving the fast-path
//! glyphs are NOT routed through the resampler.

use sonicterm_gpu::core::icon_fit_resample_target_for_test;
use sonicterm_text::swash_rasterizer::{apply_symbol_fit, classify_symbol, SymbolFit};

/// Pixel-grid equality between the atlas TILE size (physical px) and
/// the QUAD size (logical px scaled by scale_factor). Round to nearest
/// to match the integer-pixel snap both sides do.
fn assert_tile_matches_quad(
    cell_box_w_logical: f32,
    cell_h: f32,
    scale_factor: f32,
    fit: SymbolFit,
) {
    assert!(matches!(fit, SymbolFit::IconCellFit), "test invariant: must use IconCellFit");

    // (A) tile size produced by the resample closure in flush_shape_run.
    let (tile_w_phys, tile_h_phys) =
        icon_fit_resample_target_for_test(cell_box_w_logical, cell_h, scale_factor);

    // (B) quad size produced by apply_symbol_fit(IconCellFit). The
    // natural-rect inputs are irrelevant: IconCellFit ignores them
    // (it derives target dims from cell_size only).
    let natural = (0.0_f32, 0.0_f32, 4.0_f32, 4.0_f32);
    let (_qx, _qy, gw_logical, gh_logical) =
        apply_symbol_fit(natural, (0.0, 0.0), (cell_box_w_logical, cell_h), fit);
    let quad_w_phys = (gw_logical * scale_factor).round().max(1.0) as u32;
    let quad_h_phys = (gh_logical * scale_factor).round().max(1.0) as u32;

    assert_eq!(
        tile_w_phys, quad_w_phys,
        "TILE width ({}) must equal QUAD width ({}) — Nearest sampler relies on 1:1",
        tile_w_phys, quad_w_phys
    );
    assert_eq!(
        tile_h_phys, quad_h_phys,
        "TILE height ({}) must equal QUAD height ({}) — Nearest sampler relies on 1:1",
        tile_h_phys, quad_h_phys
    );
}

#[test]
fn icon_fit_tile_and_quad_are_1to1_at_dpr2() {
    // NF lightning U+F0E7 classifies as IconCellFit; the production
    // flush_shape_run dispatches on this.
    assert!(matches!(classify_symbol('\u{f0e7}'), SymbolFit::IconCellFit));
    // Typical 10x20 cell at scale_factor=2.0 (DPR2).
    assert_tile_matches_quad(10.0, 20.0, 2.0, SymbolFit::IconCellFit);
}

#[test]
fn icon_fit_tile_and_quad_are_1to1_at_dpr1() {
    assert_tile_matches_quad(10.0, 20.0, 1.0, SymbolFit::IconCellFit);
}

#[test]
fn icon_fit_tile_and_quad_are_1to1_at_fractional_dpr() {
    // Fractional DPI (e.g. Windows 125%) — both sides round to the
    // same integer-pixel grid because they apply the same scale before
    // rounding.
    assert_tile_matches_quad(9.6, 19.2, 1.25, SymbolFit::IconCellFit);
    assert_tile_matches_quad(11.3, 22.7, 1.5, SymbolFit::IconCellFit);
}

#[test]
fn icon_fit_tile_and_quad_are_1to1_wide_cell() {
    // Wide cluster (cluster_cells=2) → cell_box_w_logical doubles.
    assert_tile_matches_quad(20.0, 20.0, 2.0, SymbolFit::IconCellFit);
}

#[test]
fn ascii_fast_path_is_not_iconcellfit_so_never_resampled() {
    // Negative case (Blocker 2): ASCII codepoints route through the
    // natural-size `get_or_insert` branch in flush_shape_run because
    // they classify as Natural. If a future regression reclassified
    // ASCII as IconCellFit, this test would fail-fast and signal that
    // the resample path would suddenly be on the per-frame text hot
    // path (perf disaster).
    for ch in ['a', 'A', '0', ' ', '!', '~'] {
        assert!(
            matches!(classify_symbol(ch), SymbolFit::Natural),
            "ASCII '{}' must stay Natural — resample is for icons only",
            ch
        );
    }
}
