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
///
/// **Contract (#567 follow-up):** today the only codepoints covered
/// by this funnel are Box-Drawing (U+2500..=U+257F) and Block-Element
/// (U+2580..=U+259F) — both always single-cell clusters per shape.rs.
/// `cell_size` is therefore safe to be `(cell_w, cell_h)`. If a
/// future expansion admits a codepoint that can shape as a multi-cell
/// cluster (`cluster_cells > 1`), the caller MUST pass the wider
/// `cell_size` (matching the snapped-edge derivation in the shaped
/// emit branches) — otherwise the geometry will collapse into the
/// lead cell and the #567 bug shape returns. The assert in the
/// caller (`flush_shape_run`) keys on `is_covered_by_geometry_emit`
/// and is not re-checkable here.
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
    // #567 guard: the two covered ranges are single-cell-cluster only
    // (Box-Drawing / Block-Element). If a future expansion admits a
    // multi-cell-cluster codepoint, this funnel will collapse it into
    // the lead cell because cell_size is derived without consulting
    // cluster_cells at the call site (geometry_emit short-circuits
    // before the snapped-edge cell-box calc). The doc-comment above
    // pins the contract; we cannot debug_assert here because the
    // funnel is called speculatively for every glyph and returns None
    // for unhandled codepoints — an assert would trip on every ASCII
    // char. The Box/Block helpers below are charmap-keyed and return
    // None for anything else.
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

/// Translate a [`LineSegment`] into a single [`QuadInstance`].
///
/// Dispatches on segment orientation:
///
/// - **Axis-aligned** (purely horizontal `dy == 0` or purely vertical
///   `dx == 0`) → [`QuadInstance::sharp`] solid rectangle. This is the
///   #564 fix: the capsule SDF's `fwidth(d)` AA falloff at the segment
///   endpoints leaves a sub-pixel-alpha gap of the cell background
///   between adjacent cells when a horizontal run like `─────` is
///   composed from per-cell segments. A sharp rect has full alpha all
///   the way to the rect edge, so adjacent cells' rects meet flush at
///   the cell boundary with no dashed/dotted look. (Diagnosis on #542
///   issuecomment-4607154811 → tracked as #564.)
/// - **Diagonal** (both `dx != 0` and `dy != 0`) → keep the existing
///   capsule SDF path, since axis-aligned-rect substitution would
///   either staircase the diagonal or require a rotated quad. The tab
///   close `×` (`push_close_x_quads`) is the canonical diagonal
///   consumer and continues to take the SDF path through
///   `QuadInstance::line`.
///
/// Phase B heavy strokes (`━`, `┃`) and the future B2 double-line set
/// also flow through here once their geometry tables come online — they
/// are axis-aligned and so will pick up the sharp-rect path
/// automatically.
fn line_segment_to_quad(
    s: &LineSegment,
    fg_rgba: [f32; 4],
    sw: f32,
    sh: f32,
    scale_factor: f32,
) -> QuadInstance {
    let (ax, ay) = s.from;
    let (bx, by) = s.to;
    // Stroke is clamped to >= 1 device px after scale (matches the
    // legacy capsule path), then converted back to logical so we can
    // size the rect in the same coordinate space as the segment.
    let thickness_px = (s.thickness * scale_factor).max(1.0);
    let half_t_logical = (thickness_px * 0.5) / scale_factor;

    // Axis-aligned dispatch (#564). Use a strict equality check on the
    // axis-of-zero-extent — Box Drawing segments are constructed from
    // cell-aligned anchors (top/bottom/left/right/center) so the
    // endpoints are exactly equal on the orthogonal axis. Anything
    // even slightly off-axis stays on the capsule SDF so we don't
    // staircase a diagonal by accident.
    if (ay - by).abs() < f32::EPSILON {
        // Horizontal: rect spans full x extent, centered on y.
        let x_min = ax.min(bx);
        let x_max = ax.max(bx);
        let y_top = ay - half_t_logical;
        let w = x_max - x_min;
        let h = 2.0 * half_t_logical;
        return QuadInstance::sharp(px_to_ndc(x_min, y_top, w, h, sw, sh), fg_rgba);
    }
    if (ax - bx).abs() < f32::EPSILON {
        // Vertical: rect spans full y extent, centered on x.
        let y_min = ay.min(by);
        let y_max = ay.max(by);
        let x_left = ax - half_t_logical;
        let w = 2.0 * half_t_logical;
        let h = y_max - y_min;
        return QuadInstance::sharp(px_to_ndc(x_left, y_min, w, h, sw, sh), fg_rgba);
    }

    // Diagonal: bounding box for the line is the AABB of the two
    // endpoints inflated by half-thickness + 1 logical px of AA padding
    // so the SDF capsule and the 1-px AA band have room.
    // `QuadInstance::line` expects endpoints relative to the rect
    // *center* in physical pixels, with the rect itself in NDC and
    // `size_px` in physical pixels.
    let pad = half_t_logical + 1.0; // 1 logical px AA padding
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
    fn box_drawing_horizontal_emits_one_sharp_rect_quad() {
        // Phase A + #564: ─ emits one sharp-rect QuadInstance (NOT the
        // capsule SDF), so adjacent cells abut flush with no dashed
        // appearance at cell joins.
        let quads =
            emit_geometry_for_char('─', (10.0, 20.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 1);
        assert_eq!(
            quads[0].line_thickness_px, 0.0,
            "#564: axis-aligned ─ must use the sharp-rect path, not the capsule SDF"
        );
    }

    #[test]
    fn box_drawing_corner_emits_two_axis_aligned_sharp_rects() {
        // ┌ is two perpendicular axis-aligned segments meeting at the
        // cell center. After #564 both halves are sharp rects.
        let quads = emit_geometry_for_char('┌', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 2);
        for q in &quads {
            assert_eq!(
                q.line_thickness_px, 0.0,
                "#564: axis-aligned corner halves must use the sharp-rect path"
            );
        }
    }

    #[test]
    fn box_drawing_cross_emits_two_full_sharp_rects_at_fractional_dpi() {
        // ┼ at 1.5× — axis-aligned dispatch must hold regardless of
        // scale.
        let quads = emit_geometry_for_char('┼', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.5).unwrap();
        assert_eq!(quads.len(), 2);
        for q in &quads {
            assert_eq!(
                q.line_thickness_px, 0.0,
                "#564: axis-aligned ┼ halves must use the sharp-rect path"
            );
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
        // Double box drawing (U+2550 ═) is still out of Phase A/B1 scope
        // (deferred to B2), so it must fall back to glyph stretch.
        assert!(emit_geometry_for_char('═', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).is_none());
    }

    #[test]
    fn box_drawing_heavy_horizontal_emits_thicker_line_than_light() {
        // Phase B1: ━ must route through the line-SDF path with a
        // stroke thickness strictly greater than ─'s. The exact value
        // depends on scale_factor (we ask for 2 logical px @ 1.0×).
        let light = emit_geometry_for_char('─', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        let heavy = emit_geometry_for_char('━', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(light.len(), 1);
        assert_eq!(heavy.len(), 1);
        assert!(
            heavy[0].line_thickness_px > light[0].line_thickness_px,
            "heavy ━ stroke ({}) must exceed light ─ stroke ({})",
            heavy[0].line_thickness_px,
            light[0].line_thickness_px
        );
    }

    #[test]
    fn box_drawing_heavy_cross_emits_two_thick_line_quads() {
        // ╋ — Phase B1 heavy cross — same shape as ┼ but with the
        // heavy stroke width.
        let quads = emit_geometry_for_char('╋', (0.0, 0.0), (8.0, 16.0), FG, SW, SH, 1.0).unwrap();
        assert_eq!(quads.len(), 2);
        for q in &quads {
            assert!(q.line_thickness_px >= 2.0, "heavy stroke must be ≥ 2 device px @ 1×");
        }
    }

    #[test]
    fn three_by_three_top_row_continuity_after_quad_translation() {
        // End-to-end: after #564, `┌─┐` translate to sharp-rect
        // QuadInstances; the rects MUST abut flush at cell boundaries
        // (no bg-color gap), since the capsule-SDF endpoint falloff is
        // gone.
        let cw = 8.0_f32;
        let ch = 16.0_f32;
        // For an axis-aligned sharp-rect QuadInstance, reconstruct the
        // rect's logical (x_min, y_min, x_max, y_max) from its NDC
        // `rect` field by inverting `px_to_ndc`.
        let abs_rect = |q: &QuadInstance| -> (f32, f32, f32, f32) {
            let x = (q.rect[0] + 1.0) * SW * 0.5;
            let y = (1.0 - q.rect[1] - q.rect[3]) * SH * 0.5;
            let w = q.rect[2] * SW * 0.5;
            let h = q.rect[3] * SH * 0.5;
            (x, y, x + w, y + h)
        };
        let tl = emit_geometry_for_char('┌', (0.0, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        let h0 = emit_geometry_for_char('─', (cw, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        let tr = emit_geometry_for_char('┐', (2.0 * cw, 0.0), (cw, ch), FG, SW, SH, 1.0).unwrap();
        // The row centerline (y) lies at cell-top + ch/2.
        let cy = ch * 0.5;
        // Filter to each cell's horizontal half (rect whose vertical
        // range straddles the centerline and width > 0).
        let horiz = |quads: &[QuadInstance]| -> (f32, f32) {
            quads
                .iter()
                .map(abs_rect)
                .find(|(_, y0, _, y1)| *y0 <= cy + 1e-3 && *y1 >= cy - 1e-3 && (y1 - y0) <= 4.0)
                .map(|(x0, _, x1, _)| (x0, x1))
                .expect("expected an axis-aligned horizontal half")
        };
        let (tl_x0, tl_x1) = horiz(&tl);
        let (h0_x0, h0_x1) = horiz(&h0);
        let (tr_x0, tr_x1) = horiz(&tr);
        // ┌'s horizontal half goes from cell-0 center → cell-0 right edge,
        // ─ spans cell-1 entirely (left → right), ┐ goes cell-2 left → center.
        // For continuity the right edge of one rect must equal the left edge
        // of the next; no bg gap is allowed.
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(tl_x1, h0_x0), "┌→─ x-join {} vs {}", tl_x1, h0_x0);
        assert!(near(h0_x1, tr_x0), "─→┐ x-join {} vs {}", h0_x1, tr_x0);
        // Sanity: the row of three covers exactly cw..2*cw across the
        // junctions (the painted center band is contiguous from ┌-center
        // through to ┐-center).
        assert!(near(tl_x0, cw * 0.5), "┌ starts at cell-0 center");
        assert!(near(tr_x1, 2.0 * cw + cw * 0.5), "┐ ends at cell-2 center");
    }

    /// #564 continuity scan: a 5-cell row of `─────` translated to
    /// QuadInstances must paint a contiguous x-band with no bg-color
    /// gap at any cell boundary, at 100/125/150% DPI.
    #[test]
    fn horizontal_run_has_no_bg_gap_at_cell_boundaries() {
        let cw_logical = 8.0_f32;
        let ch_logical = 16.0_f32;
        for &scale in &[1.0_f32, 1.25, 1.5] {
            let mut x_intervals: Vec<(f32, f32)> = Vec::new();
            for cell in 0..5 {
                let origin = (cell as f32 * cw_logical, 0.0);
                let quads = emit_geometry_for_char(
                    '─',
                    origin,
                    (cw_logical, ch_logical),
                    FG,
                    SW,
                    SH,
                    scale,
                )
                .unwrap();
                assert_eq!(quads.len(), 1, "─ should emit exactly 1 quad at scale {scale}");
                let q = &quads[0];
                assert_eq!(
                    q.line_thickness_px, 0.0,
                    "#564: ─ must use sharp-rect at scale {scale}, not capsule SDF"
                );
                // Recover the logical x-range from the NDC rect.
                let x0 = (q.rect[0] + 1.0) * SW * 0.5;
                let w = q.rect[2] * SW * 0.5;
                x_intervals.push((x0, x0 + w));
            }
            // Adjacent intervals must meet flush — right edge of cell N
            // == left edge of cell N+1, no daylight (would imply a
            // bg-color column between cells).
            for w in x_intervals.windows(2) {
                let (_, right_n) = w[0];
                let (left_np1, _) = w[1];
                let gap = left_np1 - right_n;
                assert!(
                    gap.abs() < 1e-3,
                    "#564 dashed-line regression at scale {scale}: gap {} between cell rects (left {}, right {})",
                    gap,
                    right_n,
                    left_np1,
                );
            }
        }
    }

    /// #564 continuity scan: a 3-row column of `│││` (one per row) must
    /// paint a contiguous y-band with no bg-color gap at row joins,
    /// at 100/125/150% DPI.
    #[test]
    fn vertical_run_has_no_bg_gap_at_row_boundaries() {
        let cw_logical = 8.0_f32;
        let ch_logical = 16.0_f32;
        for &scale in &[1.0_f32, 1.25, 1.5] {
            let mut y_intervals: Vec<(f32, f32)> = Vec::new();
            for row in 0..3 {
                let origin = (0.0, row as f32 * ch_logical);
                let quads = emit_geometry_for_char(
                    '│',
                    origin,
                    (cw_logical, ch_logical),
                    FG,
                    SW,
                    SH,
                    scale,
                )
                .unwrap();
                assert_eq!(quads.len(), 1);
                let q = &quads[0];
                assert_eq!(
                    q.line_thickness_px, 0.0,
                    "#564: │ must use sharp-rect at scale {scale}"
                );
                // Recover logical y-range. Note: px_to_ndc Y-flips
                // (ndc_y is the *bottom* of the rect in NDC space),
                // so logical y_top = (1 - ndc_y - ndc_h) * sh / 2.
                let y_top = (1.0 - q.rect[1] - q.rect[3]) * SH * 0.5;
                let h = q.rect[3] * SH * 0.5;
                y_intervals.push((y_top, y_top + h));
            }
            for w in y_intervals.windows(2) {
                let (_, bottom_n) = w[0];
                let (top_np1, _) = w[1];
                let gap = top_np1 - bottom_n;
                assert!(
                    gap.abs() < 1e-3,
                    "#564 dashed-line regression at scale {scale}: gap {} between row rects",
                    gap,
                );
            }
        }
    }

    /// Regression guard for the tab-close `×`: diagonal segments MUST
    /// continue to flow through the capsule SDF path. The `×` itself
    /// is emitted by `push_close_x_quads` in `quad.rs`, but any
    /// future diagonal BoxGeometry (arcs, slashes) must also stay on
    /// the SDF path so they don't staircase. We construct a synthetic
    /// LineSegment to exercise the dispatch directly.
    #[test]
    fn diagonal_segment_stays_on_capsule_sdf() {
        let seg = LineSegment {
            from: (0.0, 0.0),
            to: (8.0, 16.0),
            thickness: 1.0,
            weight: sonicterm_text::box_drawing_geometry::StrokeWeight::Light,
            style: sonicterm_text::box_drawing_geometry::StrokeStyle::Single,
        };
        let quad = line_segment_to_quad(&seg, FG, SW, SH, 1.0);
        assert!(
            quad.line_thickness_px > 0.0,
            "diagonal must keep the capsule SDF — tab-close × depends on it"
        );
    }
}
