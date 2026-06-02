//! Per-codepoint procedural geometry for a subset of Box Drawing
//! (`U+2500..=U+257F`).
//!
//! Box Drawing glyphs in a font are positioned via the font's bearings,
//! which produces visible inter-cell gaps when adjacent corner / line /
//! junction codepoints don't agree on where the centerline sits. The fix
//! is to bypass the font glyph entirely for the codepoints we cover and
//! emit cell-aligned line segments anchored to the cell's geometric
//! center and edges.
//!
//! Phase A covers the 11 light single-line codepoints:
//!
//! - `─` U+2500 horizontal line
//! - `│` U+2502 vertical line
//! - `┌` U+250C top-left corner
//! - `┐` U+2510 top-right corner
//! - `└` U+2514 bottom-left corner
//! - `┘` U+2518 bottom-right corner
//! - `├` U+251C left-T
//! - `┤` U+2524 right-T
//! - `┬` U+252C top-T
//! - `┴` U+2534 bottom-T
//! - `┼` U+253C cross
//!
//! All other codepoints in the Box Drawing block (heavy/double/dashed/
//! arc/diagonal) return `None`; callers fall back to the existing
//! `BoxDrawingCellFill` glyph stretch path in
//! `swash_rasterizer::apply_symbol_fit`.
//!
//! Coordinates returned here are in the same logical pixel space as the
//! `cell_origin` / `cell_size` inputs — the GPU translator
//! (`sonicterm-gpu`) is responsible for device-pixel snapping and the
//! final NDC conversion. See `crates/sonicterm-gpu/src/quad.rs` for the
//! `QuadInstance::line` primitive these segments are translated into.
//!
//! See #542 (Box Drawing geometry epic) and the diagnosis at
//! <https://github.com/D0n9X1n/SonicTerm/issues/542>.

/// One axis-aligned line segment in cell-local pixel coordinates.
///
/// `from` / `to` are absolute pixel coordinates in the same space as
/// the inputs to [`box_drawing_geometry`] (typically logical / pre-
/// device-pixel-snap). `thickness` is the stroke width in logical
/// pixels; the renderer is free to round it up for device-pixel
/// alignment but should not silently halve it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineSegment {
    /// First endpoint of the segment, in absolute pixel coordinates.
    pub from: (f32, f32),
    /// Second endpoint of the segment, in absolute pixel coordinates.
    pub to: (f32, f32),
    /// Stroke thickness in logical pixels (e.g. `1.0` for light, the
    /// renderer can clamp `>= 1px device` after scale).
    pub thickness: f32,
}

/// Geometry to emit for a single Box Drawing cell.
///
/// Phase A only produces line-segment geometry; future phases may add
/// `DashedLines`, `DoubleLines`, or `Arc` variants without breaking
/// existing callers (they should `match` exhaustively and fall back on
/// the glyph-stretch path for unknown variants).
#[derive(Clone, Debug, PartialEq)]
pub enum BoxGeometry {
    /// One or more straight line segments that together form the
    /// codepoint's glyph (e.g. a corner is two perpendicular segments
    /// meeting at the cell center).
    Lines(Vec<LineSegment>),
}

/// Returns the procedural geometry for the Phase-A Box Drawing subset,
/// or `None` for any codepoint outside that subset. `None` indicates
/// "fall back to the existing glyph stretch path" and is the correct
/// response for every Box Drawing codepoint not yet ported.
///
/// `cell_origin` is the cell top-left in logical pixels; `cell_size`
/// is `(width, height)` in the same units. The returned segments use
/// the same coordinate system.
///
/// The cell centerlines are placed at `cell_origin + 0.5 * cell_size`.
/// Horizontal lines terminate at the cell's left/right edges
/// (`x = cell_origin.0` and `x = cell_origin.0 + cell_size.0`) and
/// vertical lines terminate at the top/bottom edges. This is the
/// continuity contract that makes adjacent cells abut without gaps —
/// adjacent `┌─┐` cells share the same `(x, y_center)` join point.
#[must_use]
pub fn box_drawing_geometry(
    ch: char,
    cell_origin: (f32, f32),
    cell_size: (f32, f32),
) -> Option<BoxGeometry> {
    let (x, y) = cell_origin;
    let (w, h) = cell_size;
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let left = x;
    let right = x + w;
    let top = y;
    let bottom = y + h;
    // Light stroke is 1 logical pixel; the renderer will clamp to
    // `>= 1 device pixel` after scale.
    let t = 1.0_f32;

    let mk = |segs: Vec<((f32, f32), (f32, f32))>| -> BoxGeometry {
        BoxGeometry::Lines(
            segs.into_iter().map(|(from, to)| LineSegment { from, to, thickness: t }).collect(),
        )
    };

    match ch as u32 {
        // ─ horizontal line
        0x2500 => Some(mk(vec![((left, cy), (right, cy))])),
        // │ vertical line
        0x2502 => Some(mk(vec![((cx, top), (cx, bottom))])),
        // ┌ top-left corner: from center down to bottom edge of cell
        //   center (i.e. cy → bottom) AND center → right edge
        0x250C => Some(mk(vec![((cx, cy), (right, cy)), ((cx, cy), (cx, bottom))])),
        // ┐ top-right corner
        0x2510 => Some(mk(vec![((left, cy), (cx, cy)), ((cx, cy), (cx, bottom))])),
        // └ bottom-left corner
        0x2514 => Some(mk(vec![((cx, top), (cx, cy)), ((cx, cy), (right, cy))])),
        // ┘ bottom-right corner
        0x2518 => Some(mk(vec![((cx, top), (cx, cy)), ((left, cy), (cx, cy))])),
        // ├ left-T: full vertical + half horizontal to right
        0x251C => Some(mk(vec![((cx, top), (cx, bottom)), ((cx, cy), (right, cy))])),
        // ┤ right-T: full vertical + half horizontal to left
        0x2524 => Some(mk(vec![((cx, top), (cx, bottom)), ((left, cy), (cx, cy))])),
        // ┬ top-T: full horizontal + half vertical down
        0x252C => Some(mk(vec![((left, cy), (right, cy)), ((cx, cy), (cx, bottom))])),
        // ┴ bottom-T: full horizontal + half vertical up
        0x2534 => Some(mk(vec![((left, cy), (right, cy)), ((cx, top), (cx, cy))])),
        // ┼ cross: full horizontal + full vertical
        0x253C => Some(mk(vec![((left, cy), (right, cy)), ((cx, top), (cx, bottom))])),
        _ => None,
    }
}

/// Returns `true` if `ch` is a codepoint Phase A covers procedurally.
///
/// Useful for renderer fast-paths that want to skip the font glyph
/// emit entirely when we know we'll draw the cell as line-SDF quads
/// instead. Mirrors the predicate shape of `block_element_rect(...).is_some()`.
#[must_use]
pub fn is_covered_box_drawing(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500
            | 0x2502
            | 0x250C
            | 0x2510
            | 0x2514
            | 0x2518
            | 0x251C
            | 0x2524
            | 0x252C
            | 0x2534
            | 0x253C
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_W: f32 = 10.0;
    const CELL_H: f32 = 20.0;
    const ORIGIN: (f32, f32) = (100.0, 200.0);

    fn lines(ch: char) -> Vec<LineSegment> {
        match box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).expect("covered") {
            BoxGeometry::Lines(v) => v,
        }
    }

    #[test]
    fn all_eleven_phase_a_codepoints_return_some() {
        for ch in ['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'] {
            assert!(
                box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_some(),
                "Phase A codepoint U+{:04X} ('{ch}') must return geometry",
                ch as u32
            );
            assert!(is_covered_box_drawing(ch));
        }
    }

    #[test]
    fn out_of_phase_a_codepoints_return_none() {
        // Heavy / double / dashed / arc variants are explicitly deferred
        // to phases B/C/D. They must keep returning `None` so the
        // renderer falls back to `BoxDrawingCellFill` glyph stretch.
        for ch in ['━', '┃', '╌', '╍', '═', '║', '╔', '╭', '╮', '╱'] {
            assert!(
                box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_none(),
                "Codepoint U+{:04X} ('{ch}') is out of Phase A scope and must return None",
                ch as u32
            );
            assert!(!is_covered_box_drawing(ch));
        }
    }

    #[test]
    fn horizontal_line_terminates_at_cell_edges() {
        // ─ must extend full cell width so two adjacent ─ cells visually
        // merge into one continuous line.
        let segs = lines('─');
        assert_eq!(segs.len(), 1);
        let s = segs[0];
        let expected_y = ORIGIN.1 + CELL_H * 0.5;
        assert_eq!(s.from, (ORIGIN.0, expected_y));
        assert_eq!(s.to, (ORIGIN.0 + CELL_W, expected_y));
    }

    #[test]
    fn vertical_line_terminates_at_cell_edges() {
        let segs = lines('│');
        assert_eq!(segs.len(), 1);
        let s = segs[0];
        let expected_x = ORIGIN.0 + CELL_W * 0.5;
        assert_eq!(s.from, (expected_x, ORIGIN.1));
        assert_eq!(s.to, (expected_x, ORIGIN.1 + CELL_H));
    }

    #[test]
    fn top_left_corner_meets_at_center() {
        // ┌ should emit a segment from center → right (the start of the
        // horizontal line) and from center → bottom (the start of the
        // vertical line). The shared center point is what guarantees
        // continuity with ─ on the right and │ below.
        let segs = lines('┌');
        assert_eq!(segs.len(), 2);
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        // One segment goes center → right edge along cy.
        assert!(segs.contains(&LineSegment {
            from: (cx, cy),
            to: (ORIGIN.0 + CELL_W, cy),
            thickness: 1.0,
        }));
        // Other segment goes center → bottom edge along cx.
        assert!(segs.contains(&LineSegment {
            from: (cx, cy),
            to: (cx, ORIGIN.1 + CELL_H),
            thickness: 1.0,
        }));
    }

    #[test]
    fn cross_emits_full_horizontal_and_full_vertical() {
        // ┼ must extend BOTH lines edge-to-edge. This is the load-bearing
        // test for the 3×3 `┼┼┼` continuity row in the GPU snapshot suite.
        let segs = lines('┼');
        assert_eq!(segs.len(), 2);
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        assert!(
            segs.contains(&LineSegment {
                from: (ORIGIN.0, cy),
                to: (ORIGIN.0 + CELL_W, cy),
                thickness: 1.0,
            }),
            "cross missing full horizontal line"
        );
        assert!(
            segs.contains(&LineSegment {
                from: (cx, ORIGIN.1),
                to: (cx, ORIGIN.1 + CELL_H),
                thickness: 1.0,
            }),
            "cross missing full vertical line"
        );
    }

    #[test]
    fn three_by_three_continuity_at_100_percent_dpi() {
        // Render the canonical 3×3 box:
        //   ┌─┐
        //   │ │
        //   └─┘
        // and assert that at each cell-to-cell join, the line endpoint
        // from the left cell coincides with the line start point in
        // the right cell (or top/bottom for vertical adjacency). Same
        // coordinate space = zero gap.
        let cw = 8.0;
        let ch = 16.0;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);

        // Row 0: ┌─┐
        let tl = lines_at('┌', cell(0, 0), (cw, ch));
        let h0 = lines_at('─', cell(1, 0), (cw, ch));
        let tr = lines_at('┐', cell(2, 0), (cw, ch));

        // Horizontal continuity on row 0: ┌ right endpoint must equal
        // ─ left endpoint; ─ right endpoint must equal ┐ left endpoint.
        let tl_right = tl.iter().find(|s| s.from.1 == ch * 0.5 && s.to.0 == cw).unwrap();
        let h0_seg = h0[0];
        assert_eq!(tl_right.to, h0_seg.from, "┌→─ horizontal join must be pixel-identical");

        let tr_left = tr.iter().find(|s| s.from.1 == ch * 0.5 && s.from.0 == 2.0 * cw).unwrap();
        assert_eq!(h0_seg.to, tr_left.from, "─→┐ horizontal join must be pixel-identical");

        // Row 2: └─┘ — same continuity check at bottom-row y center.
        let bl = lines_at('└', cell(0, 2), (cw, ch));
        let h2 = lines_at('─', cell(1, 2), (cw, ch));
        let br = lines_at('┘', cell(2, 2), (cw, ch));
        let bl_right = bl.iter().find(|s| s.to.0 == cw && s.to.1 == 2.0 * ch + ch * 0.5).unwrap();
        let h2_seg = h2[0];
        assert_eq!(bl_right.to, h2_seg.from);
        let br_left =
            br.iter().find(|s| s.from.0 == 2.0 * cw && s.from.1 == 2.0 * ch + ch * 0.5).unwrap();
        assert_eq!(h2_seg.to, br_left.from);

        // Vertical continuity left column: ┌ bottom == │ top == └ top.
        let v1 = lines_at('│', cell(0, 1), (cw, ch));
        let tl_down = tl.iter().find(|s| s.to.0 == cw * 0.5 && s.to.1 == ch).unwrap();
        let v1_top = v1[0];
        assert_eq!(tl_down.to, v1_top.from);
        let bl_up = bl.iter().find(|s| s.from.0 == cw * 0.5 && s.from.1 == 2.0 * ch).unwrap();
        assert_eq!(v1_top.to, bl_up.from);
    }

    #[test]
    fn three_by_three_continuity_at_125_percent_dpi() {
        // Per Opus Step-2 (#470/#489 lesson): re-run continuity check at
        // fractional DPI, since cell-edge snapping is where gaps reappear.
        check_continuity_at_scale(1.25);
    }

    #[test]
    fn three_by_three_continuity_at_150_percent_dpi() {
        check_continuity_at_scale(1.5);
    }

    #[test]
    fn cross_row_continuity_at_150_percent_dpi() {
        // ┼┼┼ row variant — three crosses must produce continuous
        // horizontal at the row centerline. This is the second 3×3
        // variant requested in the spec.
        let cw = 7.0_f32 * 1.5;
        let ch = 14.0_f32 * 1.5;
        let c0 = lines_at('┼', (0.0, 0.0), (cw, ch));
        let c1 = lines_at('┼', (cw, 0.0), (cw, ch));
        let c2 = lines_at('┼', (2.0 * cw, 0.0), (cw, ch));
        // Horizontal segment of each ┼ spans the full cell width at cy.
        let cy = ch * 0.5;
        let h0 = c0.iter().find(|s| s.from.1 == cy && s.to.1 == cy).unwrap();
        let h1 = c1.iter().find(|s| s.from.1 == cy && s.to.1 == cy).unwrap();
        let h2 = c2.iter().find(|s| s.from.1 == cy && s.to.1 == cy).unwrap();
        assert_eq!(h0.to, h1.from, "┼┼ join must be pixel-identical at 150% DPI");
        assert_eq!(h1.to, h2.from, "┼┼ second join must be pixel-identical at 150% DPI");
    }

    fn lines_at(ch: char, origin: (f32, f32), size: (f32, f32)) -> Vec<LineSegment> {
        match box_drawing_geometry(ch, origin, size).expect("covered") {
            BoxGeometry::Lines(v) => v,
        }
    }

    fn check_continuity_at_scale(scale: f32) {
        // Pre-multiply cell size by scale to simulate fractional DPI
        // happening BEFORE snap-to-device-pixels (which is what
        // sonicterm-render-model::geometry does in the live pipeline).
        let cw = 8.0_f32 * scale;
        let ch = 16.0_f32 * scale;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);

        let tl = lines_at('┌', cell(0, 0), (cw, ch));
        let h0 = lines_at('─', cell(1, 0), (cw, ch));
        let tr = lines_at('┐', cell(2, 0), (cw, ch));
        let cy = ch * 0.5;
        let tl_right = tl.iter().find(|s| s.from.1 == cy && s.to.0 == cw).unwrap();
        let h0_seg = h0[0];
        assert_eq!(tl_right.to, h0_seg.from, "scale {scale}× ┌→─ must be gap-free");
        let tr_left = tr.iter().find(|s| s.from.0 == 2.0 * cw && s.from.1 == cy).unwrap();
        assert_eq!(h0_seg.to, tr_left.from, "scale {scale}× ─→┐ must be gap-free");
    }
}
