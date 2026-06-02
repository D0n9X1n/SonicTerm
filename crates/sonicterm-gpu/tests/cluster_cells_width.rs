// #567: ligature + nerd-font cluster width regression.
//
// shape.rs emits one `ShapedGlyph` per shape cluster with
// `cluster_cells` indicating how many source columns the cluster
// reserves (e.g. `=>` with calt ligature → 2; long NF composed icons
// → 2 or 3). The pre-fix renderer computed `cell_pixel_width_snapped`
// using `span = if is_wide { 2 } else { 1 }` at every emit site,
// ignoring `cluster_cells`. The ligature glyph was then squeezed into
// the lead cell and the trailing cells were left visually empty
// (#563's calt fix exposed this surface).
//
// The fix derives `span = if is_wide { 2 } else { g.cluster_cells.max(1) }`
// at every emit site. CJK (WIDE flag, cluster_cells == 1) still uses
// 2 cells; standard 1-cell glyphs unchanged.
//
// The seam `emit_one_glyph_for_trace` accepts `cell_size` directly so
// we exercise it with the cell box that would result from each
// `cluster_cells` value, validating the post-symbol-fit rect honours
// the wider box. The source-scan tests below pin the production span
// calculation against future regressions.

use sonicterm_gpu::core::emit_one_glyph_for_trace;
use sonicterm_text::swash_rasterizer::SymbolFit;

const CELL_W: f32 = 10.0;
const CELL_H: f32 = 20.0;
const CELL_X: f32 = 0.0;
const CELL_Y: f32 = 0.0;

/// A glyph natural rect that's wider than a single cell — represents
/// what a ligature like `=>` or a multi-cell NF icon raster looks
/// like at the rasterizer output. The renderer's job is to keep this
/// width when the cell box is at least as wide (don't clamp).
fn wide_natural_rect(w: f32) -> (f32, f32, f32, f32) {
    (CELL_X, CELL_Y, w, CELL_H * 0.8)
}

#[test]
fn ligature_two_cell_cluster_uses_two_cell_box() {
    // Simulate the cell box derived from `cluster_cells == 2` for a
    // non-WIDE codepoint: width == cell_w * 2.
    let cell_box_w = CELL_W * 2.0;
    let trace = emit_one_glyph_for_trace(
        '=', // visible cell ch — non-Powerline, classifies as Natural
        '=',
        wide_natural_rect(cell_box_w * 0.95),
        (CELL_X, CELL_Y),
        (cell_box_w, CELL_H),
        false,
    );
    // For Natural fit the seam preserves the natural rect — proves the
    // renderer is willing to emit a glyph wider than one cell when the
    // cell_size argument reflects cluster_cells.
    let (_, _, qw, _) = trace.final_rect;
    assert!(
        qw > CELL_W,
        "ligature 2-cell cluster: final quad width {qw} should exceed single cell_w {CELL_W} \
         (cell_box_w was {cell_box_w}); got rect {:?}",
        trace.final_rect
    );
}

#[test]
fn three_cell_cluster_uses_three_cell_box() {
    // Simulate cluster_cells == 3 (long NF icon composed cluster).
    let cell_box_w = CELL_W * 3.0;
    let trace = emit_one_glyph_for_trace(
        '\u{f0a0}', // arbitrary PUA NF codepoint
        '\u{f0a0}',
        wide_natural_rect(cell_box_w * 0.9),
        (CELL_X, CELL_Y),
        (cell_box_w, CELL_H),
        false,
    );
    let (_, _, qw, _) = trace.final_rect;
    assert!(
        qw > CELL_W * 2.0,
        "3-cell cluster: final quad width {qw} should exceed 2 * cell_w {} \
         (cell_box_w was {cell_box_w}); got rect {:?}",
        CELL_W * 2.0,
        trace.final_rect
    );
}

#[test]
fn cjk_wide_unchanged_at_two_cells() {
    // CJK uses WIDE flag, cluster_cells == 1 — the union pins span to
    // 2 cells (the WIDE branch wins). Simulate the resulting cell_size.
    let cell_box_w = CELL_W * 2.0;
    let trace = emit_one_glyph_for_trace(
        '中',
        '中',
        wide_natural_rect(cell_box_w * 0.9),
        (CELL_X, CELL_Y),
        (cell_box_w, CELL_H),
        false,
    );
    let (_, _, qw, _) = trace.final_rect;
    assert!(
        qw > CELL_W && qw <= cell_box_w + 0.01,
        "CJK WIDE: final width {qw} should land in (cell_w, 2*cell_w] = ({CELL_W}, {cell_box_w}]"
    );
}

#[test]
fn standard_single_cell_glyph_unchanged() {
    // cluster_cells == 1 + non-WIDE → single cell, no regression.
    let trace = emit_one_glyph_for_trace(
        'A',
        'A',
        (CELL_X, CELL_Y, CELL_W * 0.7, CELL_H * 0.8),
        (CELL_X, CELL_Y),
        (CELL_W, CELL_H),
        false,
    );
    let (_, _, qw, _) = trace.final_rect;
    assert!(
        qw <= CELL_W + 0.01,
        "standard 1-cell glyph: final width {qw} must not exceed cell_w {CELL_W}"
    );
}

#[test]
fn powerline_chevron_two_cell_cluster_cell_fills_full_two_cells() {
    // Mixed regression: a Powerline chevron that happens to land in a
    // 2-cell cluster (rare but possible with custom ligature fonts).
    // PowerlineCellFill MUST fill the full reserved cell box.
    let cell_box_w = CELL_W * 2.0;
    let trace = emit_one_glyph_for_trace(
        '\u{E0B0}',
        '\u{E0B0}',
        (CELL_X + 1.0, CELL_Y + 1.0, 6.0, 2.0),
        (CELL_X, CELL_Y),
        (cell_box_w, CELL_H),
        false,
    );
    assert_eq!(trace.fit_used, SymbolFit::PowerlineCellFill);
    let (_, _, qw, qh) = trace.final_rect;
    assert!(
        (qw - cell_box_w).abs() <= 0.01,
        "powerline 2-cell: width {qw} != cell_box_w {cell_box_w}"
    );
    assert!(qh >= CELL_H * 0.8, "powerline 2-cell: height {qh} not cell-filling");
}

// ----------------------------------------------------------------------
// Source-scan regression guards. Every `span = if is_wide ...` in
// `flush_shape_run` MUST consume `g.cluster_cells` for the non-WIDE
// branch. The pre-fix `1usize` literal is the bug surface.
// ----------------------------------------------------------------------

#[test]
fn shape_run_emit_branches_consume_cluster_cells() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/core.rs"))
        .expect("read core.rs");
    let bad = "let span = if is_wide { 2usize } else { 1usize };";
    assert!(
        !src.contains(bad),
        "pre-fix span calc detected (#567): `{bad}` should not appear in core.rs — \
         use `g.cluster_cells.max(1) as usize` for the non-WIDE branch."
    );
    // Sanity: at least one corrected site is present.
    let good = "g.cluster_cells.max(1) as usize";
    let count = src.matches(good).count();
    assert!(
        count >= 4,
        "expected ≥4 cluster_cells-aware span sites in core.rs (1 legacy + 3 snapped \
         emit branches + 1 entry), found {count}"
    );
}

#[test]
fn legacy_cell_pixel_width_consumes_cluster_cells() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/core.rs"))
        .expect("read core.rs");
    let bad = "let cell_pixel_width = if is_wide { cell_w * 2.0 } else { cell_w };";
    assert!(
        !src.contains(bad),
        "pre-fix legacy cell_pixel_width detected (#567): `{bad}` should not appear — \
         multiply by cluster_cells when not WIDE."
    );
}
