//! Bridge between `sonicterm-text`'s renderer-agnostic per-codepoint
//! geometry tables ([`block_element_geometry`], [`box_drawing_geometry`])
//! and the GPU [`QuadInstance`] format.
//!
//! Two source modules in `sonicterm-text` describe Unicode-block-aware
//! sub-cell geometry without depending on wgpu:
//!
//! - `block_element_geometry` → `BlockGeometry::{SingleRect, MultiRect,
//!   ShadedRect}` for `U+2580..=U+259F`.
//! - `box_drawing_geometry` → `BoxGeometry::Lines(Vec<LineSegment>)` for
//!   the Phase A subset of `U+2500..=U+257F`.
//!
//! Before this helper, every GPU emit branch (ASCII fast path, char-
//! fallback path, shaped path) routed only `BlockGeometry`'s primary
//! rect through the glyph atlas — multi-rect quadrant chars and shaded
//! chars lost geometry, and Box Drawing was never expressed as quads at
//! all (it went through `BoxDrawingCellFill` glyph stretch in
//! `swash_rasterizer::apply_symbol_fit`).
//!
//! [`emit_geometry_for_char`] is the single funnel — one helper used by
//! all three emit paths (see `core.rs::flush_shape_run` and its ASCII
//! fast path). Returning the [`QuadInstance`] vector instead of pushing
//! to a parameter keeps the helper testable in isolation and avoids the
//! "fix only one branch" anti-pattern flagged in #542's diagnosis.
//!
//! Coordinate convention: cell coordinates are in **logical pixels**
//! (matching the input space of the source geometry modules).
//! Device-pixel snap and NDC conversion happen at the call site, so
//! this helper stays independent of surface size.
//!
//! See #542 (Box Drawing geometry epic — Phase A + A0).

use sonicterm_text::block_element_geometry::{block_element_rect, BlockGeometry};
use sonicterm_text::box_drawing_geometry::{box_drawing_geometry, BoxGeometry, LineSegment};

use crate::quad::{px_to_ndc, QuadInstance};

/// Phase-A foreground geometry for the codepoints covered by either
/// [`block_element_rect`] or [`box_drawing_geometry`], translated to
/// [`QuadInstance`]s.
///
/// Returns:
///
/// - `Some(Vec<_>)` — one or more `QuadInstance`s the caller should
///   append to the frame's quad list (NOT the glyph atlas list — these
///   are direct foreground quads). The caller should ALSO skip the
///   corresponding glyph atlas emit for this cell so the font glyph
///   doesn't double up on top of the geometry.
/// - `None` — `ch` isn't covered by Phase A or A0; fall back to the
///   existing glyph atlas path (`BoxDrawingCellFill` stretch for box
///   drawing, normal glyph for everything else).
///
/// `cell_origin` is the cell top-left in logical pixels;
/// `cell_size` is `(width, height)`. `fg_rgba` is the foreground
/// color in linear premultiplied RGBA. `sw` / `sh` are the surface
/// dimensions in physical pixels (needed for the NDC conversion).
/// `scale_factor` translates logical → physical pixels so the
/// line-SDF stroke width can be expressed in physical pixels.
#[must_use]
pub fn emit_geometry_for_char(
    ch: char,
    cell_origin: (f32, f32),
    cell_size: (f32, f32),
    fg_rgba: [f32; 4],
    sw: f32,
    sh: f32,
    scale_factor: f32,
) -> Option<Vec<QuadInstance>> {
    if let Some(geom) = box_drawing_geometry(ch, cell_origin, cell_size) {
        return Some(box_geometry_to_quads(&geom, fg_rgba, sw, sh, scale_factor));
    }
    if let Some(geom) = block_element_rect(ch, cell_origin, cell_size) {
        return Some(block_geometry_to_quads(&geom, fg_rgba, sw, sh));
    }
    None
}

fn block_geometry_to_quads(
    geom: &BlockGeometry,
    fg_rgba: [f32; 4],
    sw: f32,
    sh: f32,
) -> Vec<QuadInstance> {
    match geom {
        BlockGeometry::SingleRect(x, y, w, h) => {
            vec![QuadInstance::sharp(px_to_ndc(*x, *y, *w, *h, sw, sh), fg_rgba)]
        }
        BlockGeometry::MultiRect(rects) => rects
            .iter()
            .map(|(x, y, w, h)| QuadInstance::sharp(px_to_ndc(*x, *y, *w, *h, sw, sh), fg_rgba))
            .collect(),
        BlockGeometry::ShadedRect((x, y, w, h), alpha) => {
            // Multiply alpha into the premultiplied color so we get a
            // visible-but-faded fill for U+2591/2/3 without needing a
            // separate shader path.
            let a = fg_rgba[3] * *alpha;
            let shaded = [fg_rgba[0] * *alpha, fg_rgba[1] * *alpha, fg_rgba[2] * *alpha, a];
            vec![QuadInstance::sharp(px_to_ndc(*x, *y, *w, *h, sw, sh), shaded)]
        }
    }
}

fn box_geometry_to_quads(
    geom: &BoxGeometry,
    fg_rgba: [f32; 4],
    sw: f32,
    sh: f32,
    scale_factor: f32,
) -> Vec<QuadInstance> {
    match geom {
        BoxGeometry::Lines(segs) => {
            segs.iter().map(|s| line_segment_to_quad(s, fg_rgba, sw, sh, scale_factor)).collect()
        }
    }
}

fn line_segment_to_quad(
    s: &LineSegment,
    fg_rgba: [f32; 4],
    sw: f32,
    sh: f32,
    scale_factor: f32,
) -> QuadInstance {
    // Bounding box for the line: the AABB of the two endpoints inflated
    // by half-thickness + 1 logical px of AA padding so the SDF capsule
    // and the 1-px AA band have room. `QuadInstance::line` expects
    // endpoints relative to the rect *center* in physical pixels, with
    // the rect itself in NDC and `size_px` in physical pixels.
    let thickness_px = (s.thickness * scale_factor).max(1.0);
    let half_t_logical = (thickness_px * 0.5) / scale_factor;
    let pad = half_t_logical + 1.0; // 1 logical px AA padding
    let (ax, ay) = s.from;
    let (bx, by) = s.to;
    let x_min = ax.min(bx) - pad;
    let y_min = ay.min(by) - pad;
    let x_max = ax.max(bx) + pad;
    let y_max = ay.max(by) + pad;
    let w_logical = x_max - x_min;
    let h_logical = y_max - y_min;
    let cx_logical = x_min + w_logical * 0.5;
    let cy_logical = y_min + h_logical * 0.5;
    // Endpoints relative to rect center, in physical pixels.
    let line_a = [(ax - cx_logical) * scale_factor, (ay - cy_logical) * scale_factor];
    let line_b = [(bx - cx_logical) * scale_factor, (by - cy_logical) * scale_factor];
    let size_px = [w_logical * scale_factor, h_logical * scale_factor];
    let rect_ndc = px_to_ndc(x_min, y_min, w_logical, h_logical, sw, sh);
    QuadInstance::line(rect_ndc, fg_rgba, size_px, line_a, line_b, thickness_px)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: f32 = 800.0;
    const SH: f32 = 600.0;
    const FG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    #[test]
    fn box_drawing_horizontal_emits_one_line_quad() {
        // Phase A: ─ emits one line-SDF QuadInstance with thickness > 0.
        let quads =
            emit_geometry_for_char('─', (10.0, 20.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 1);
        assert!(quads[0].line_thickness_px >= 1.0, "line stroke must clamp to >= 1 device px");
    }

    #[test]
    fn box_drawing_corner_emits_two_line_quads() {
        // ┌ is two perpendicular segments meeting at the cell center —
        // exactly the case where the "fix only one branch" anti-pattern
        // would drop the second line.
        let quads = emit_geometry_for_char('┌', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 2);
        for q in &quads {
            assert!(q.line_thickness_px > 0.0, "corner segments must use the line-SDF path");
        }
    }

    #[test]
    fn box_drawing_cross_emits_two_full_line_quads_at_fractional_dpi() {
        // ┼ at 1.5× — the line-quad count and the use of the SDF path
        // must hold regardless of scale.
        let quads = emit_geometry_for_char('┼', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.5).unwrap();
        assert_eq!(quads.len(), 2);
        for q in &quads {
            // Stroke is clamped to >= 1 device px after scale.
            assert!(q.line_thickness_px >= 1.0);
        }
    }

    #[test]
    fn block_multirect_emits_multiple_quads_a0_regression() {
        // A0 regression: U+2599 (▙) is three-quadrant — must emit 3
        // QuadInstances, NOT 1. Before #542, the GPU paths collapsed
        // this through `primary_rect` and only the first quadrant
        // rendered.
        let quads = emit_geometry_for_char('▙', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 3, "U+2599 ▙ must emit 3 quadrant rects, not the primary only");
        for q in &quads {
            assert_eq!(
                q.line_thickness_px, 0.0,
                "block-element rects use the sharp-rect path, not line-SDF"
            );
        }
    }

    #[test]
    fn block_shaded_rect_emits_alpha_modulated_quad_a0_regression() {
        // A0 regression: U+2592 (▒, medium shade) must emit a single
        // full-cell rect with alpha multiplied by 0.5. Before #542, the
        // ShadedRect alpha multiplier was dropped at the GPU call site.
        let quads = emit_geometry_for_char('▒', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 1);
        // FG is premultiplied [1,1,1,1]; shaded should be [0.5, 0.5, 0.5, 0.5].
        let c = quads[0].color;
        assert!((c[3] - 0.5).abs() < 1e-5, "shaded alpha must be 0.5, got {}", c[3]);
        assert!((c[0] - 0.5).abs() < 1e-5, "premultiplied red must be 0.5, got {}", c[0]);
    }

    #[test]
    fn uncovered_char_returns_none() {
        // ASCII 'A' is neither block element nor Phase-A box drawing —
        // must NOT route through this helper. Returning None tells the
        // caller to keep using the glyph atlas path.
        assert!(emit_geometry_for_char('A', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).is_none());
        // Heavy box drawing (U+2501 ━) is out of Phase A scope, fall
        // back to glyph stretch.
        assert!(emit_geometry_for_char('━', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).is_none());
    }

    #[test]
    fn three_by_three_top_row_continuity_after_quad_translation() {
        // End-to-end: after translating `┌─┐` to QuadInstances, the
        // QuadInstances' line-SDF endpoints (in physical pixels offset
        // from rect center) must still describe a continuous line at
        // the row centerline. This catches off-by-one regressions in
        // `line_segment_to_quad`'s padding / center math.
        let cw = 8.0_f32;
        let ch = 16.0_f32;
        // Reconstruct each segment's absolute pixel endpoints from the
        // QuadInstance's rect (NDC → logical via the inverse formula)
        // and its line_a/line_b (physical offsets from rect center).
        let abs_endpoints = |q: &QuadInstance, scale: f32| -> ((f32, f32), (f32, f32)) {
            // rect = [nx, ny, nw, nh]; logical x = (nx + 1) * sw/2;
            // logical y = (1 - ny - nh) * sh/2; w = nw * sw/2; h = nh * sh/2.
            let x = (q.rect[0] + 1.0) * SW * 0.5;
            let y = (1.0 - q.rect[1] - q.rect[3]) * SH * 0.5;
            let w = q.rect[2] * SW * 0.5;
            let h = q.rect[3] * SH * 0.5;
            let cx = x + w * 0.5;
            let cy = y + h * 0.5;
            let a = (cx + q.line_a[0] / scale, cy + q.line_a[1] / scale);
            let b = (cx + q.line_b[0] / scale, cy + q.line_b[1] / scale);
            (a, b)
        };
        let tl = emit_geometry_for_char('┌', (0.0, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        let h0 = emit_geometry_for_char('─', (cw, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        let tr = emit_geometry_for_char('┐', (2.0 * cw, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        // Find ┌'s horizontal half (center → right edge of cell 0).
        let cy = ch * 0.5;
        let tl_horiz = tl
            .iter()
            .map(|q| abs_endpoints(q, 1.0))
            .find(|(a, b)| (a.1 - cy).abs() < 1e-3 && (b.1 - cy).abs() < 1e-3)
            .expect("┌ must have a horizontal half");
        let h0_seg = abs_endpoints(&h0[0], 1.0);
        let tr_horiz = tr
            .iter()
            .map(|q| abs_endpoints(q, 1.0))
            .find(|(a, b)| (a.1 - cy).abs() < 1e-3 && (b.1 - cy).abs() < 1e-3)
            .expect("┐ must have a horizontal half");
        // ┌ right endpoint ≈ ─ left endpoint; ─ right endpoint ≈ ┐ left.
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(
            near(tl_horiz.1 .0, h0_seg.0 .0),
            "┌→─ x-join {} vs {}",
            tl_horiz.1 .0,
            h0_seg.0 .0
        );
        assert!(
            near(h0_seg.1 .0, tr_horiz.0 .0),
            "─→┐ x-join {} vs {}",
            h0_seg.1 .0,
            tr_horiz.0 .0
        );
    }
}
