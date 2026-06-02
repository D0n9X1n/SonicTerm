// #538: regression test for the cluster-lead vs visible-cell codepoint
// mismatch in the shaped emit path of `flush_shape_run`. See the issue
// thread (Haiku Step-1 / Opus Step-2) and the in-file `#538:` comments
// in `crates/sonicterm-gpu/src/core.rs` for the diagnosis.
//
// Setup: simulate a Powerline chevron (U+E0B0..U+E0BF), Block Elements
// (U+2588 sample), and Box Drawing (U+2500 sample) where the shape
// cluster lead reported by HarfBuzz is a SPACE (the common harfbuzz
// behaviour for a single-glyph continuation cluster on these PUA
// codepoints). swash's natural rect for these is a tiny ~2-px top
// slice — the renderer MUST override it with the *visible* cell's
// codepoint classification.
//
// Stashed-main repro (proves test catches the bug):
// Set `classify_on_cluster_lead = true` in the seam call — that
// reproduces the pre-fix dispatch, the assertion on cell-fill fails.

use sonicterm_gpu::core::emit_one_glyph_for_trace;
use sonicterm_text::swash_rasterizer::SymbolFit;

const CELL_W: f32 = 10.0;
const CELL_H: f32 = 20.0;
const CELL_X: f32 = 100.0;
const CELL_Y: f32 = 200.0;

/// swash natural rect for a Powerline chevron when classified as
/// Natural — a ~2-px top slice (baseline-anchored, height << cell_h).
/// This is exactly what the pre-fix renderer emitted (see #538).
fn natural_baseline_slice() -> (f32, f32, f32, f32) {
    (CELL_X + 1.0, CELL_Y + 1.0, 6.0, 2.0)
}

fn assert_full_cell_fill(name: &str, ch: char, expect: SymbolFit) {
    let trace = emit_one_glyph_for_trace(
        ch,
        ' ', // cluster lead — the bug used this
        natural_baseline_slice(),
        (CELL_X, CELL_Y),
        (CELL_W, CELL_H),
        false, // classify on visible cell ch (the fix)
    );
    assert_eq!(
        trace.fit_used, expect,
        "{name} U+{:04X}: expected fit {:?}, got {:?}",
        ch as u32, expect, trace.fit_used
    );
    let (_, qy, _, qh) = trace.final_rect;
    assert!(
        qh >= CELL_H * 0.8,
        "{name} U+{:04X}: final quad height {qh} < 80% of cell_h {CELL_H} \
         (got rect {:?}) — symbol_fit did not cell-fill",
        ch as u32,
        trace.final_rect
    );
    // Top edge tolerance: within 1 logical px of the cell top.
    assert!(
        (qy - CELL_Y).abs() <= 1.0,
        "{name} U+{:04X}: final quad y0 {qy} not within ±1 of cell top {CELL_Y}",
        ch as u32
    );
}

#[test]
fn powerline_chevron_range_cell_fills() {
    // U+E0B0..=U+E0BF — Powerline Symbols
    for cp in 0xE0B0u32..=0xE0BFu32 {
        let ch = char::from_u32(cp).unwrap();
        assert_full_cell_fill("powerline", ch, SymbolFit::PowerlineCellFill);
    }
}

#[test]
fn block_full_block_cell_fills() {
    // U+2588 — FULL BLOCK, BlockCellFill bucket.
    assert_full_cell_fill("block", '\u{2588}', SymbolFit::BlockCellFill);
}

#[test]
fn box_drawing_horizontal_cell_fills() {
    // U+2500 — BOX DRAWINGS LIGHT HORIZONTAL, BoxDrawingCellFill bucket.
    // NB: BoxDrawingCellFill preserves natural vertical placement, so we
    // can't assert the full-height contract here. Instead assert the
    // FIT itself was chosen (vs. Natural) — that is the bug surface.
    let trace = emit_one_glyph_for_trace(
        '\u{2500}',
        ' ',
        natural_baseline_slice(),
        (CELL_X, CELL_Y),
        (CELL_W, CELL_H),
        false,
    );
    assert_eq!(
        trace.fit_used,
        SymbolFit::BoxDrawingCellFill,
        "U+2500: expected BoxDrawingCellFill, got {:?}",
        trace.fit_used
    );
    // The seam should stretch horizontally to fill cell_w.
    let (_, _, qw, _) = trace.final_rect;
    assert!((qw - CELL_W).abs() <= 0.01, "U+2500: final quad width {qw} != cell_w {CELL_W}");
}

// ----------------------------------------------------------------------
// Stashed-main repro: passing the cluster lead to the seam reproduces
// the pre-fix dispatch. This must FAIL (the SymbolFit lookup returns
// Natural, and the final rect stays at the 2-px baseline slice). We
// flip the assertion to confirm the bug shape — this is the negative
// control that proves the positive tests above are catching real
// behaviour, not just succeeding vacuously.
// ----------------------------------------------------------------------

#[test]
fn negative_control_cluster_lead_dispatch_emits_baseline_slice() {
    let trace = emit_one_glyph_for_trace(
        '\u{E0B0}',
        ' ',
        natural_baseline_slice(),
        (CELL_X, CELL_Y),
        (CELL_W, CELL_H),
        true, // the BUG: classify on cluster lead (space)
    );
    assert_eq!(
        trace.fit_used,
        SymbolFit::Natural,
        "negative control: space-lead dispatch should classify as Natural \
         (it did not — the bug surface has shifted, update this test)"
    );
    let (_, _, _, qh) = trace.final_rect;
    assert!(
        qh < CELL_H * 0.8,
        "negative control: with bug, final quad height {qh} should be < 80% cell_h {CELL_H}, \
         got rect {:?}",
        trace.final_rect
    );
}

// ----------------------------------------------------------------------
// Source-scan regression guard: assert that the shaped emit branch in
// `flush_shape_run` no longer passes `g.ch` to `classify_symbol`. The
// seam-level tests above prove that classifying on the visible cell ch
// is correct; this scan proves the production apply site still wires
// up to the correct codepoint after future edits.
//
// Stashed-main repro: revert the L4984 edit (replace `classify_ch` with
// `g.ch`) and this test FAILS with `pre-fix dispatch detected …`.
// ----------------------------------------------------------------------

#[test]
fn shaped_emit_branch_does_not_classify_on_cluster_lead() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/core.rs"))
        .expect("read core.rs");
    // Look for the exact buggy call shape inside the shaped-emit branch.
    // `g.ch` may still appear elsewhere (it's the shape cluster's lead
    // codepoint, used as a glyph-cache key etc.) — what we forbid is
    // passing it to `classify_symbol` or to `block_element_rect`.
    let bad_classify = "classify_symbol(g.ch)";
    let bad_block_rect_arg = "block_element_rect(\n                    g.ch,";
    assert!(
        !src.contains(bad_classify),
        "pre-fix dispatch detected in crates/sonicterm-gpu/src/core.rs — \
         `{bad_classify}` should not appear (#538). Use the visible cell \
         char (`lead_cell.ch` / `classify_ch`) instead."
    );
    assert!(
        !src.contains(bad_block_rect_arg),
        "pre-fix dispatch detected — `block_element_rect` is being called \
         with the cluster lead `g.ch` instead of the visible cell ch (#538)."
    );
}
