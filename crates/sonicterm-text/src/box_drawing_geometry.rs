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
//! Phase B1 adds the 11 heavy single-line counterparts (same geometric
//! shape, thicker stroke):
//!
//! - `━` U+2501 heavy horizontal line
//! - `┃` U+2503 heavy vertical line
//! - `┏` U+250F heavy top-left corner
//! - `┓` U+2513 heavy top-right corner
//! - `┗` U+2517 heavy bottom-left corner
//! - `┛` U+251B heavy bottom-right corner
//! - `┣` U+2523 heavy left-T
//! - `┫` U+252B heavy right-T
//! - `┳` U+2533 heavy top-T
//! - `┻` U+253B heavy bottom-T
//! - `╋` U+254B heavy cross
//!
//! Phase B2 adds the 11 double-line counterparts:
//!
//! - `═` U+2550 double horizontal line
//! - `║` U+2551 double vertical line
//! - `╔` U+2554 double top-left corner
//! - `╗` U+2557 double top-right corner
//! - `╚` U+255A double bottom-left corner
//! - `╝` U+255D double bottom-right corner
//! - `╠` U+2560 double left-T
//! - `╣` U+2563 double right-T
//! - `╦` U+2566 double top-T
//! - `╩` U+2569 double bottom-T
//! - `╬` U+256C double cross
//!
//! Double-line data model: straights (═ ║) are expressed as a single
//! logical centerline `LineSegment` with [`StrokeStyle::Double`]; the
//! renderer is responsible for splaying the centerline into two parallel
//! lanes offset by [`DOUBLE_LANE_OFFSET_PX`]. Corners / T-junctions /
//! the cross are expressed as **pre-clipped per-lane** `LineSegment`s
//! with [`StrokeStyle::Single`] — each lane is its own segment ending
//! at the inner corner `(cx ± DOUBLE_LANE_OFFSET_PX, cy ±
//! DOUBLE_LANE_OFFSET_PX)`. This avoids the renderer needing to know
//! how to "splay" a corner (which would require junction-context the
//! data table doesn't have), and the inner-corner coordinates are
//! asserted in tests to prevent overshoot through the junction window.
//!
//! All other codepoints in the Box Drawing block (dashed/arc/diagonal)
//! return `None`; callers fall back to the existing
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

/// Stroke weight classification for a Box Drawing line segment.
///
/// The numeric stroke width lives in [`LineSegment::thickness`] —
/// this enum is the *semantic* tag the renderer can use for theme-
/// aware adjustments (e.g. boosting heavy strokes another half-pixel
/// at low DPI). Heavy strokes are nominally 2 logical px; light
/// strokes are nominally 1 logical px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeWeight {
    /// 1-logical-pixel single stroke — Phase A codepoints (U+2500,
    /// U+2502, U+250C, …).
    Light,
    /// 2-logical-pixel single stroke — Phase B1 codepoints (U+2501,
    /// U+2503, U+250F, …).
    Heavy,
}

/// Stroke style — single line vs double parallel lines.
///
/// `Single` covers all Phase A + B1 codepoints and the pre-clipped lane
/// segments of Phase B2 corners / T-junctions / cross. `Double` is the
/// Phase B2 centerline tag for ═ ║ — the renderer splays it into two
/// parallel lanes offset by [`DOUBLE_LANE_OFFSET_PX`] perpendicular to
/// the segment axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeStyle {
    /// One single line along the segment.
    Single,
    /// Two parallel lines offset perpendicular to the segment axis.
    /// Phase B2: ═ ║ centerlines carry this tag and the renderer
    /// splays them at ±[`DOUBLE_LANE_OFFSET_PX`].
    Double,
}

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
    /// Stroke thickness in logical pixels (e.g. `1.0` for light, `2.0`
    /// for heavy). The renderer can clamp `>= 1px device` after scale.
    pub thickness: f32,
    /// Semantic stroke weight tag — derived from the source codepoint.
    pub weight: StrokeWeight,
    /// Stroke style (single vs double). Currently always `Single`;
    /// `Double` is reserved for Phase B2.
    pub style: StrokeStyle,
}

/// Geometry to emit for a single Box Drawing cell.
///
/// Phase A only produces line-segment geometry; future phases may add
/// `DashedLines` or `Arc` variants without breaking existing callers
/// (they should `match` exhaustively and fall back on the glyph-stretch
/// path for unknown variants).
#[derive(Clone, Debug, PartialEq)]
pub enum BoxGeometry {
    /// One or more straight line segments that together form the
    /// codepoint's glyph (e.g. a corner is two perpendicular segments
    /// meeting at the cell center).
    Lines(Vec<LineSegment>),
}

/// Nominal stroke thickness for a light single line, in logical pixels.
pub const LIGHT_THICKNESS_PX: f32 = 1.0;
/// Nominal stroke thickness for a heavy single line, in logical pixels.
pub const HEAVY_THICKNESS_PX: f32 = 2.0;
/// Perpendicular distance from a `Double`-style centerline to each
/// emitted lane, in logical pixels. Lanes are spaced
/// `2 * DOUBLE_LANE_OFFSET_PX = 3.0` logical pixels center-to-center,
/// which leaves a ≥1 device-pixel inter-lane gap at 100/125/150% DPI
/// when the per-lane stroke is `LIGHT_THICKNESS_PX`. The same constant
/// is used to place the pre-clipped lane segments of double-line
/// corners / T-junctions / the cross so the renderer's `Double` splay
/// for ═ ║ meets the corner lanes at pixel-identical inner corners.
pub const DOUBLE_LANE_OFFSET_PX: f32 = 1.5;

/// Returns the procedural geometry for the Phase A + B1 Box Drawing
/// subset, or `None` for any codepoint outside that subset. `None`
/// indicates "fall back to the existing glyph stretch path" and is the
/// correct response for every Box Drawing codepoint not yet ported.
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
/// Heavy codepoints share the same join points as light, so mixed
/// `┌━┓` rows still meet at the cell-edge midline.
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

    let mk = |segs: Vec<((f32, f32), (f32, f32))>, weight: StrokeWeight| -> BoxGeometry {
        let thickness = match weight {
            StrokeWeight::Light => LIGHT_THICKNESS_PX,
            StrokeWeight::Heavy => HEAVY_THICKNESS_PX,
        };
        BoxGeometry::Lines(
            segs.into_iter()
                .map(|(from, to)| LineSegment {
                    from,
                    to,
                    thickness,
                    weight,
                    style: StrokeStyle::Single,
                })
                .collect(),
        )
    };

    // Phase B2 helper: emit per-lane `Single` segments for double-line
    // corners / T-junctions / cross. Each `(from, to)` is one pre-clipped
    // lane — the renderer does NOT splay these (they are already lane
    // geometry), so they carry `StrokeStyle::Single`.
    let mk_lanes = |segs: Vec<((f32, f32), (f32, f32))>| -> BoxGeometry {
        BoxGeometry::Lines(
            segs.into_iter()
                .map(|(from, to)| LineSegment {
                    from,
                    to,
                    thickness: LIGHT_THICKNESS_PX,
                    weight: StrokeWeight::Light,
                    style: StrokeStyle::Single,
                })
                .collect(),
        )
    };

    // Phase B2 helper: emit a single centerline `Double`-style segment.
    // The renderer is responsible for emitting the two lanes at
    // `± DOUBLE_LANE_OFFSET_PX` perpendicular to the segment axis.
    let mk_double_center = |segs: Vec<((f32, f32), (f32, f32))>| -> BoxGeometry {
        BoxGeometry::Lines(
            segs.into_iter()
                .map(|(from, to)| LineSegment {
                    from,
                    to,
                    thickness: LIGHT_THICKNESS_PX,
                    weight: StrokeWeight::Light,
                    style: StrokeStyle::Double,
                })
                .collect(),
        )
    };

    let off = DOUBLE_LANE_OFFSET_PX;

    match ch as u32 {
        // ── Phase A: light single-line ──────────────────────────────
        // ─ horizontal line
        0x2500 => Some(mk(vec![((left, cy), (right, cy))], StrokeWeight::Light)),
        // │ vertical line
        0x2502 => Some(mk(vec![((cx, top), (cx, bottom))], StrokeWeight::Light)),
        // ┌ top-left corner: from center down to bottom edge of cell
        //   center (i.e. cy → bottom) AND center → right edge
        0x250C => {
            Some(mk(vec![((cx, cy), (right, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Light))
        }
        // ┐ top-right corner
        0x2510 => {
            Some(mk(vec![((left, cy), (cx, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Light))
        }
        // └ bottom-left corner
        0x2514 => {
            Some(mk(vec![((cx, top), (cx, cy)), ((cx, cy), (right, cy))], StrokeWeight::Light))
        }
        // ┘ bottom-right corner
        0x2518 => {
            Some(mk(vec![((cx, top), (cx, cy)), ((left, cy), (cx, cy))], StrokeWeight::Light))
        }
        // ├ left-T: full vertical + half horizontal to right
        0x251C => {
            Some(mk(vec![((cx, top), (cx, bottom)), ((cx, cy), (right, cy))], StrokeWeight::Light))
        }
        // ┤ right-T: full vertical + half horizontal to left
        0x2524 => {
            Some(mk(vec![((cx, top), (cx, bottom)), ((left, cy), (cx, cy))], StrokeWeight::Light))
        }
        // ┬ top-T: full horizontal + half vertical down
        0x252C => {
            Some(mk(vec![((left, cy), (right, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Light))
        }
        // ┴ bottom-T: full horizontal + half vertical up
        0x2534 => {
            Some(mk(vec![((left, cy), (right, cy)), ((cx, top), (cx, cy))], StrokeWeight::Light))
        }
        // ┼ cross: full horizontal + full vertical
        0x253C => Some(mk(
            vec![((left, cy), (right, cy)), ((cx, top), (cx, bottom))],
            StrokeWeight::Light,
        )),

        // ── Phase B1: heavy single-line ─────────────────────────────
        // Geometric shape is identical to the light counterparts; only
        // stroke weight differs. Continuity points (cell-edge midlines)
        // are unchanged so mixed light/heavy rows still abut cleanly.
        // ━ heavy horizontal line
        0x2501 => Some(mk(vec![((left, cy), (right, cy))], StrokeWeight::Heavy)),
        // ┃ heavy vertical line
        0x2503 => Some(mk(vec![((cx, top), (cx, bottom))], StrokeWeight::Heavy)),
        // ┏ heavy top-left corner
        0x250F => {
            Some(mk(vec![((cx, cy), (right, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Heavy))
        }
        // ┓ heavy top-right corner
        0x2513 => {
            Some(mk(vec![((left, cy), (cx, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Heavy))
        }
        // ┗ heavy bottom-left corner
        0x2517 => {
            Some(mk(vec![((cx, top), (cx, cy)), ((cx, cy), (right, cy))], StrokeWeight::Heavy))
        }
        // ┛ heavy bottom-right corner
        0x251B => {
            Some(mk(vec![((cx, top), (cx, cy)), ((left, cy), (cx, cy))], StrokeWeight::Heavy))
        }
        // ┣ heavy left-T
        0x2523 => {
            Some(mk(vec![((cx, top), (cx, bottom)), ((cx, cy), (right, cy))], StrokeWeight::Heavy))
        }
        // ┫ heavy right-T
        0x252B => {
            Some(mk(vec![((cx, top), (cx, bottom)), ((left, cy), (cx, cy))], StrokeWeight::Heavy))
        }
        // ┳ heavy top-T
        0x2533 => {
            Some(mk(vec![((left, cy), (right, cy)), ((cx, cy), (cx, bottom))], StrokeWeight::Heavy))
        }
        // ┻ heavy bottom-T
        0x253B => {
            Some(mk(vec![((left, cy), (right, cy)), ((cx, top), (cx, cy))], StrokeWeight::Heavy))
        }
        // ╋ heavy cross
        0x254B => Some(mk(
            vec![((left, cy), (right, cy)), ((cx, top), (cx, bottom))],
            StrokeWeight::Heavy,
        )),

        // ── Phase B2: double-line ───────────────────────────────────
        // Straights: single centerline tagged `Double`; renderer splays
        // into two lanes at ±DOUBLE_LANE_OFFSET_PX perpendicular.
        // ═ double horizontal
        0x2550 => Some(mk_double_center(vec![((left, cy), (right, cy))])),
        // ║ double vertical
        0x2551 => Some(mk_double_center(vec![((cx, top), (cx, bottom))])),
        // ╔ double top-left corner — outer L (top-left of the inner
        // corner) + inner L. Outer lane: from (cx - off, cy - off)
        // out to right + down. Inner lane: from (cx + off, cy + off)
        // out to right + down. Pre-clipped so each lane is its own
        // axis-aligned segment.
        0x2554 => Some(mk_lanes(vec![
            // Upper-horizontal lane (outer): inner corner → right edge
            ((cx - off, cy - off), (right, cy - off)),
            // Lower-horizontal lane (inner): inner corner → right edge
            ((cx + off, cy + off), (right, cy + off)),
            // Left-vertical lane (outer): inner corner → bottom edge
            ((cx - off, cy - off), (cx - off, bottom)),
            // Right-vertical lane (inner): inner corner → bottom edge
            ((cx + off, cy + off), (cx + off, bottom)),
        ])),
        // ╗ double top-right corner
        0x2557 => Some(mk_lanes(vec![
            ((left, cy - off), (cx + off, cy - off)),
            ((left, cy + off), (cx - off, cy + off)),
            ((cx + off, cy - off), (cx + off, bottom)),
            ((cx - off, cy + off), (cx - off, bottom)),
        ])),
        // ╚ double bottom-left corner
        0x255A => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, cy + off)),
            ((cx + off, top), (cx + off, cy - off)),
            ((cx - off, cy + off), (right, cy + off)),
            ((cx + off, cy - off), (right, cy - off)),
        ])),
        // ╝ double bottom-right corner
        0x255D => Some(mk_lanes(vec![
            ((cx + off, top), (cx + off, cy + off)),
            ((cx - off, top), (cx - off, cy - off)),
            ((left, cy + off), (cx + off, cy + off)),
            ((left, cy - off), (cx - off, cy - off)),
        ])),
        // ╠ double left-T — left vertical lane spans full cell height
        // (it's continuous through the junction). Right vertical lane
        // is broken by the horizontal arms. Horizontal arms exit right.
        0x2560 => Some(mk_lanes(vec![
            // Left (outer) vertical lane: continuous top → bottom
            ((cx - off, top), (cx - off, bottom)),
            // Right (inner) vertical lane top half: top → upper inner corner
            ((cx + off, top), (cx + off, cy - off)),
            // Right (inner) vertical lane bottom half: lower inner corner → bottom
            ((cx + off, cy + off), (cx + off, bottom)),
            // Upper-horizontal arm: from right inner-vertical → right edge
            ((cx + off, cy - off), (right, cy - off)),
            // Lower-horizontal arm: from right inner-vertical → right edge
            ((cx + off, cy + off), (right, cy + off)),
        ])),
        // ╣ double right-T
        0x2563 => Some(mk_lanes(vec![
            // Right (outer) vertical lane: continuous top → bottom
            ((cx + off, top), (cx + off, bottom)),
            // Left (inner) vertical lane top half
            ((cx - off, top), (cx - off, cy - off)),
            // Left (inner) vertical lane bottom half
            ((cx - off, cy + off), (cx - off, bottom)),
            // Upper-horizontal arm: left edge → left inner-vertical
            ((left, cy - off), (cx - off, cy - off)),
            // Lower-horizontal arm
            ((left, cy + off), (cx - off, cy + off)),
        ])),
        // ╦ double top-T — top horizontal lane continuous; bottom
        // horizontal lane broken by the vertical arms descending.
        0x2566 => Some(mk_lanes(vec![
            // Top (outer) horizontal lane: continuous left → right
            ((left, cy - off), (right, cy - off)),
            // Bottom (inner) horizontal lane left half
            ((left, cy + off), (cx - off, cy + off)),
            // Bottom (inner) horizontal lane right half
            ((cx + off, cy + off), (right, cy + off)),
            // Left-vertical arm descending
            ((cx - off, cy + off), (cx - off, bottom)),
            // Right-vertical arm descending
            ((cx + off, cy + off), (cx + off, bottom)),
        ])),
        // ╩ double bottom-T
        0x2569 => Some(mk_lanes(vec![
            // Bottom (outer) horizontal lane: continuous left → right
            ((left, cy + off), (right, cy + off)),
            // Top (inner) horizontal lane left half
            ((left, cy - off), (cx - off, cy - off)),
            // Top (inner) horizontal lane right half
            ((cx + off, cy - off), (right, cy - off)),
            // Left-vertical arm ascending
            ((cx - off, top), (cx - off, cy - off)),
            // Right-vertical arm ascending
            ((cx + off, top), (cx + off, cy - off)),
        ])),
        // ╬ double cross — both horizontal and vertical lanes are
        // broken at the central junction; emit 8 segments (4 arms ×
        // 2 lanes each). The junction "window" between the inner
        // corners is intentionally empty — that's the canonical
        // double-cross look.
        0x256C => Some(mk_lanes(vec![
            // Top arm — left lane
            ((cx - off, top), (cx - off, cy - off)),
            // Top arm — right lane
            ((cx + off, top), (cx + off, cy - off)),
            // Bottom arm — left lane
            ((cx - off, cy + off), (cx - off, bottom)),
            // Bottom arm — right lane
            ((cx + off, cy + off), (cx + off, bottom)),
            // Left arm — top lane
            ((left, cy - off), (cx - off, cy - off)),
            // Left arm — bottom lane
            ((left, cy + off), (cx - off, cy + off)),
            // Right arm — top lane
            ((cx + off, cy - off), (right, cy - off)),
            // Right arm — bottom lane
            ((cx + off, cy + off), (right, cy + off)),
        ])),

        _ => None,
    }
}

/// Returns `true` if `ch` is a codepoint Phase A or Phase B1 covers
/// procedurally.
///
/// Useful for renderer fast-paths that want to skip the font glyph
/// emit entirely when we know we'll draw the cell as line-SDF quads
/// instead. Mirrors the predicate shape of `block_element_rect(...).is_some()`.
#[must_use]
pub fn is_covered_box_drawing(ch: char) -> bool {
    matches!(
        ch as u32,
        // Phase A — light
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
            // Phase B1 — heavy
            | 0x2501
            | 0x2503
            | 0x250F
            | 0x2513
            | 0x2517
            | 0x251B
            | 0x2523
            | 0x252B
            | 0x2533
            | 0x253B
            | 0x254B
            // Phase B2 — double
            | 0x2550
            | 0x2551
            | 0x2554
            | 0x2557
            | 0x255A
            | 0x255D
            | 0x2560
            | 0x2563
            | 0x2566
            | 0x2569
            | 0x256C
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
    fn all_eleven_phase_b1_heavy_codepoints_return_some() {
        // Phase B1: every heavy single-line counterpart must produce
        // geometry, must be tagged Heavy, and must be in the covered
        // predicate so cache invalidation (#559 wire-up) catches them.
        for ch in ['━', '┃', '┏', '┓', '┗', '┛', '┣', '┫', '┳', '┻', '╋'] {
            let geom = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H));
            assert!(
                geom.is_some(),
                "Phase B1 codepoint U+{:04X} ('{ch}') must return geometry",
                ch as u32
            );
            assert!(
                is_covered_box_drawing(ch),
                "Phase B1 codepoint U+{:04X} ('{ch}') must be in is_covered_box_drawing",
                ch as u32
            );
            let BoxGeometry::Lines(segs) = geom.unwrap();
            assert!(!segs.is_empty(), "U+{:04X} produced empty Lines", ch as u32);
            for s in &segs {
                assert_eq!(
                    s.weight,
                    StrokeWeight::Heavy,
                    "U+{:04X} segment must be StrokeWeight::Heavy",
                    ch as u32
                );
                assert_eq!(
                    s.style,
                    StrokeStyle::Single,
                    "Phase B1 codepoints are single-stroke (Double is B2)"
                );
                assert!(
                    (s.thickness - HEAVY_THICKNESS_PX).abs() < f32::EPSILON,
                    "U+{:04X} thickness must be HEAVY_THICKNESS_PX, got {}",
                    ch as u32,
                    s.thickness
                );
            }
        }
    }

    #[test]
    fn phase_a_codepoints_remain_tagged_light() {
        // Adding the StrokeWeight tag must not flip Phase A under us.
        for ch in ['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'] {
            let BoxGeometry::Lines(segs) =
                box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).unwrap();
            for s in &segs {
                assert_eq!(
                    s.weight,
                    StrokeWeight::Light,
                    "U+{:04X} ('{ch}') must remain StrokeWeight::Light",
                    ch as u32
                );
                assert!((s.thickness - LIGHT_THICKNESS_PX).abs() < f32::EPSILON);
            }
        }
    }

    #[test]
    fn out_of_scope_codepoints_return_none() {
        // Dashed / arc / diagonal variants are explicitly deferred to
        // phases C/D. They must keep returning `None` so the renderer
        // falls back to `BoxDrawingCellFill` glyph stretch. NOTE:
        // Phase B2 moved ═ ║ ╔ ╗ ╚ ╝ ╠ ╣ ╦ ╩ ╬ into the covered set;
        // they are no longer in this list.
        for ch in ['╌', '╍', '╭', '╮', '╱', '╲'] {
            assert!(
                box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_none(),
                "Codepoint U+{:04X} ('{ch}') is out of phase scope and must return None",
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
    fn heavy_horizontal_line_matches_light_endpoints() {
        // ━ shares the same endpoints as ─ (so mixed light/heavy rows
        // line up at the cell midline); only thickness/weight differ.
        let light = lines('─');
        let heavy = lines('━');
        assert_eq!(light.len(), 1);
        assert_eq!(heavy.len(), 1);
        assert_eq!(light[0].from, heavy[0].from, "heavy ━ must share light ─'s left endpoint");
        assert_eq!(light[0].to, heavy[0].to, "heavy ━ must share light ─'s right endpoint");
        assert!(heavy[0].thickness > light[0].thickness, "heavy must be thicker than light");
    }

    #[test]
    fn heavy_cross_emits_full_horizontal_and_full_vertical() {
        // ╋ must extend BOTH lines edge-to-edge — same continuity
        // contract as ┼ in Phase A.
        let segs = lines('╋');
        assert_eq!(segs.len(), 2);
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        assert!(
            segs.iter().any(|s| s.from == (ORIGIN.0, cy)
                && s.to == (ORIGIN.0 + CELL_W, cy)
                && s.weight == StrokeWeight::Heavy),
            "heavy cross missing full heavy horizontal line"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx, ORIGIN.1)
                && s.to == (cx, ORIGIN.1 + CELL_H)
                && s.weight == StrokeWeight::Heavy),
            "heavy cross missing full heavy vertical line"
        );
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
        assert!(segs.iter().any(|s| s.from == (cx, cy)
            && s.to == (ORIGIN.0 + CELL_W, cy)
            && s.weight == StrokeWeight::Light));
        // Other segment goes center → bottom edge along cx.
        assert!(segs.iter().any(|s| s.from == (cx, cy)
            && s.to == (cx, ORIGIN.1 + CELL_H)
            && s.weight == StrokeWeight::Light));
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
            segs.iter().any(|s| s.from == (ORIGIN.0, cy)
                && s.to == (ORIGIN.0 + CELL_W, cy)
                && s.thickness == 1.0),
            "cross missing full horizontal line"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx, ORIGIN.1)
                && s.to == (cx, ORIGIN.1 + CELL_H)
                && s.thickness == 1.0),
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

    #[test]
    fn heavy_three_by_three_continuity_at_100_percent_dpi() {
        // Phase B1 spec'd 3×3:
        //   ┏━┓
        //   ┃ ┃
        //   ┗━┛
        // — same continuity contract as the light variant, asserted at
        // 100% DPI. Heavy segments must abut their neighbours at the
        // cell midline pixel-identically.
        check_heavy_3x3_continuity(1.0);
    }

    #[test]
    fn heavy_three_by_three_continuity_at_125_percent_dpi() {
        check_heavy_3x3_continuity(1.25);
    }

    #[test]
    fn heavy_three_by_three_continuity_at_150_percent_dpi() {
        check_heavy_3x3_continuity(1.5);
    }

    fn check_heavy_3x3_continuity(scale: f32) {
        let cw = 8.0_f32 * scale;
        let ch = 16.0_f32 * scale;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);

        // Row 0: ┏━┓
        let tl = lines_at('┏', cell(0, 0), (cw, ch));
        let h0 = lines_at('━', cell(1, 0), (cw, ch));
        let tr = lines_at('┓', cell(2, 0), (cw, ch));
        let cy = ch * 0.5;
        let tl_right = tl.iter().find(|s| s.from.1 == cy && s.to.0 == cw).unwrap();
        let h0_seg = h0[0];
        assert_eq!(
            tl_right.to, h0_seg.from,
            "scale {scale}× ┏→━ horizontal join must be pixel-identical"
        );
        let tr_left = tr.iter().find(|s| s.from.0 == 2.0 * cw && s.from.1 == cy).unwrap();
        assert_eq!(
            h0_seg.to, tr_left.from,
            "scale {scale}× ━→┓ horizontal join must be pixel-identical"
        );

        // Row 2: ┗━┛
        let bl = lines_at('┗', cell(0, 2), (cw, ch));
        let h2 = lines_at('━', cell(1, 2), (cw, ch));
        let br = lines_at('┛', cell(2, 2), (cw, ch));
        let bot_cy = 2.0 * ch + ch * 0.5;
        let bl_right = bl.iter().find(|s| s.to.0 == cw && s.to.1 == bot_cy).unwrap();
        let h2_seg = h2[0];
        assert_eq!(bl_right.to, h2_seg.from, "scale {scale}× ┗→━ join");
        let br_left = br.iter().find(|s| s.from.0 == 2.0 * cw && s.from.1 == bot_cy).unwrap();
        assert_eq!(h2_seg.to, br_left.from, "scale {scale}× ━→┛ join");

        // Vertical continuity left column: ┏ bottom == ┃ top == ┗ top.
        let v1 = lines_at('┃', cell(0, 1), (cw, ch));
        let cx = cw * 0.5;
        let tl_down = tl.iter().find(|s| s.to.0 == cx && s.to.1 == ch).unwrap();
        let v1_top = v1[0];
        assert_eq!(tl_down.to, v1_top.from, "scale {scale}× ┏→┃ vertical join");
        let bl_up = bl.iter().find(|s| s.from.0 == cx && s.from.1 == 2.0 * ch).unwrap();
        assert_eq!(v1_top.to, bl_up.from, "scale {scale}× ┃→┗ vertical join");
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

    // ─── Phase B2 (double-line) tests ──────────────────────────────

    #[test]
    fn all_eleven_phase_b2_double_codepoints_return_some() {
        // Every double-line codepoint must produce geometry and be in
        // the covered predicate. ═ ║ ride the `Double` centerline
        // path; the rest are pre-clipped lane geometry tagged Single.
        for ch in ['═', '║', '╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let geom = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H));
            assert!(
                geom.is_some(),
                "Phase B2 codepoint U+{:04X} ('{ch}') must return geometry",
                ch as u32
            );
            assert!(
                is_covered_box_drawing(ch),
                "Phase B2 codepoint U+{:04X} ('{ch}') must be in is_covered_box_drawing",
                ch as u32
            );
            let BoxGeometry::Lines(segs) = geom.unwrap();
            assert!(!segs.is_empty(), "U+{:04X} produced empty Lines", ch as u32);
        }
    }

    #[test]
    fn double_straights_use_double_style_centerline() {
        // ═ and ║ are emitted as ONE centerline segment tagged
        // `StrokeStyle::Double` — the renderer splays into two lanes.
        for ch in ['═', '║'] {
            let segs = lines(ch);
            assert_eq!(
                segs.len(),
                1,
                "double-straight U+{:04X} must be one centerline segment",
                ch as u32
            );
            assert_eq!(
                segs[0].style,
                StrokeStyle::Double,
                "double-straight U+{:04X} centerline must be tagged StrokeStyle::Double",
                ch as u32
            );
        }
    }

    #[test]
    fn double_junctions_use_single_pre_clipped_lanes() {
        // Corners / T-junctions / cross are pre-clipped lane geometry;
        // each segment is its own axis-aligned `Single` lane. This is
        // load-bearing: if the renderer splays these too they would
        // produce 4× the strokes and overshoot the junction window.
        for ch in ['╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let segs = lines(ch);
            assert!(segs.len() >= 4, "double-junction U+{:04X} needs ≥ 4 lanes", ch as u32);
            for s in &segs {
                assert_eq!(
                    s.style,
                    StrokeStyle::Single,
                    "double-junction U+{:04X} lane must be tagged Single (pre-clipped)",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn double_cross_inner_corner_coordinates_no_overshoot() {
        // ╬ — inner-corner assertion (per Opus Step-2): every "arm
        // end facing the junction" must terminate at exactly cx ± off
        // / cy ± off. No segment may cross the central junction window
        // (i.e. no segment endpoint with x in (cx - off, cx + off)
        // strictly AND y in (cy - off, cy + off) strictly).
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let segs = lines('╬');
        assert_eq!(segs.len(), 8, "╬ must emit exactly 8 lane segments (4 arms × 2 lanes)");
        for s in &segs {
            for &p in &[s.from, s.to] {
                let inside_x = p.0 > cx - off && p.0 < cx + off;
                let inside_y = p.1 > cy - off && p.1 < cy + off;
                assert!(
                    !(inside_x && inside_y),
                    "╬ segment endpoint {:?} overshoots the junction window (cx={cx}, cy={cy}, off={off})",
                    p
                );
            }
        }
        // Spot-check: ╬ must have segments ending exactly at each of
        // the four inner corners.
        let inner = [
            (cx - off, cy - off),
            (cx + off, cy - off),
            (cx - off, cy + off),
            (cx + off, cy + off),
        ];
        for corner in &inner {
            assert!(
                segs.iter().any(|s| s.from == *corner || s.to == *corner),
                "╬ missing endpoint at inner corner {:?}",
                corner
            );
        }
    }

    #[test]
    fn double_top_left_corner_inner_corner_assertion() {
        // ╔ — outer L meets at (cx - off, cy - off), inner L at
        // (cx + off, cy + off). Asserting these explicitly per Opus
        // Step-2 to prevent any future "splay the corner" refactor
        // from sliding the inner corner inward.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let segs = lines('╔');
        assert_eq!(segs.len(), 4, "╔ = 4 lanes (2 horizontal + 2 vertical)");
        // Outer inner-corner (cx - off, cy - off) must be the shared
        // junction of the outer horizontal and outer vertical lanes.
        let outer = (cx - off, cy - off);
        let inner = (cx + off, cy + off);
        assert!(
            segs.iter().any(|s| s.from == outer && s.to == (ORIGIN.0 + CELL_W, cy - off)),
            "╔ outer horizontal lane must start at outer inner-corner"
        );
        assert!(
            segs.iter().any(|s| s.from == outer && s.to == (cx - off, ORIGIN.1 + CELL_H)),
            "╔ outer vertical lane must start at outer inner-corner"
        );
        assert!(
            segs.iter().any(|s| s.from == inner && s.to == (ORIGIN.0 + CELL_W, cy + off)),
            "╔ inner horizontal lane must start at inner inner-corner"
        );
        assert!(
            segs.iter().any(|s| s.from == inner && s.to == (cx + off, ORIGIN.1 + CELL_H)),
            "╔ inner vertical lane must start at inner inner-corner"
        );
    }

    #[test]
    fn double_t_junctions_have_one_continuous_through_lane() {
        // ╠ — left vertical lane MUST be continuous top→bottom (it's
        // the "outer" of the T's stem). Mirror for ╣ ╦ ╩.
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let off = DOUBLE_LANE_OFFSET_PX;

        let l = lines('╠');
        assert!(
            l.iter()
                .any(|s| s.from == (cx - off, ORIGIN.1) && s.to == (cx - off, ORIGIN.1 + CELL_H)),
            "╠ outer (left) vertical lane must be continuous top→bottom"
        );
        let r = lines('╣');
        assert!(
            r.iter()
                .any(|s| s.from == (cx + off, ORIGIN.1) && s.to == (cx + off, ORIGIN.1 + CELL_H)),
            "╣ outer (right) vertical lane must be continuous top→bottom"
        );
        let t = lines('╦');
        assert!(
            t.iter()
                .any(|s| s.from == (ORIGIN.0, cy - off) && s.to == (ORIGIN.0 + CELL_W, cy - off)),
            "╦ outer (top) horizontal lane must be continuous left→right"
        );
        let b = lines('╩');
        assert!(
            b.iter()
                .any(|s| s.from == (ORIGIN.0, cy + off) && s.to == (ORIGIN.0 + CELL_W, cy + off)),
            "╩ outer (bottom) horizontal lane must be continuous left→right"
        );
    }

    fn check_double_3x3_continuity(scale: f32) {
        // ╔══╗
        // ║  ║
        // ╚══╝
        // At each cell-to-cell join the lane endpoints must coincide.
        // Straight ═ ║ are `Double` centerlines; corners are
        // pre-clipped lanes. The continuity contract is:
        //   ╔ outer horizontal lane (y = cy - off) ends at right edge;
        //   ═ centerline at the same cell's right edge has y = cy
        //   (same row centerline) — and the renderer splays it into
        //   lanes at the *same* y ± off, so lane joins are
        //   pixel-identical.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cw = 8.0_f32 * scale;
        let ch = 16.0_f32 * scale;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);

        let tl = lines_at('╔', cell(0, 0), (cw, ch));
        let h0a = lines_at('═', cell(1, 0), (cw, ch));
        let _h0b = lines_at('═', cell(2, 0), (cw, ch));
        let tr = lines_at('╗', cell(3, 0), (cw, ch));

        let cy_top = ch * 0.5;
        // ╔ outer horizontal lane right-edge endpoint y must equal
        // the ═ centerline y minus off — i.e. lane upper edge lines
        // up with ═-splayed upper lane.
        let tl_outer_h_to = tl
            .iter()
            .find(|s| (s.to.1 - (cy_top - off)).abs() < 1e-3 && s.to.0 >= cw - 1e-3)
            .expect("╔ outer horizontal lane")
            .to;
        let h0a_center = h0a[0];
        assert_eq!(
            h0a_center.style,
            StrokeStyle::Double,
            "scale {scale}× ═ must be Double-style centerline"
        );
        // The renderer splays h0a_center into (y - off, y + off);
        // assert ╔'s outer-lane right edge (x = cw, y = cy - off)
        // sits exactly on the centerline-derived upper-lane left edge
        // (x = cw, y = cy - off).
        assert!(
            (tl_outer_h_to.0 - cw).abs() < 1e-3,
            "scale {scale}× ╔ outer horizontal right edge must be x=cw"
        );
        assert!(
            (h0a_center.from.0 - cw).abs() < 1e-3 && (h0a_center.from.1 - cy_top).abs() < 1e-3,
            "scale {scale}× ═ centerline must start at left cell edge on row centerline"
        );

        // Mirror checks on the closing ╗ side and the bottom row.
        let bl = lines_at('╚', cell(0, 2), (cw, ch));
        let h2a = lines_at('═', cell(1, 2), (cw, ch));
        let h2b = lines_at('═', cell(2, 2), (cw, ch));
        let br = lines_at('╝', cell(3, 2), (cw, ch));
        assert_eq!(h2a[0].style, StrokeStyle::Double);
        assert_eq!(h2b[0].style, StrokeStyle::Double);
        // Sanity: the closing corners produced 4 lanes each.
        assert_eq!(tr.len(), 4, "╗ must emit 4 lanes");
        assert_eq!(bl.len(), 4, "╚ must emit 4 lanes");
        assert_eq!(br.len(), 4, "╝ must emit 4 lanes");

        // Vertical continuity on the left column ╔ → ║ → ╚: ╔'s outer
        // vertical lane bottom must equal ║'s centerline-splayed-left
        // lane top, and ║'s centerline-splayed-left lane bottom must
        // equal ╚'s outer vertical lane top. Centerline gives the
        // canonical x of each lane (cx_left ± off); pre-clipped corner
        // lanes use the same off so x's match by construction.
        let v_mid = lines_at('║', cell(0, 1), (cw, ch));
        assert_eq!(v_mid[0].style, StrokeStyle::Double);
        let cx_left = cw * 0.5;
        assert!((v_mid[0].from.0 - cx_left).abs() < 1e-3);
        assert!((v_mid[0].from.1 - ch).abs() < 1e-3);
        assert!((v_mid[0].to.1 - 2.0 * ch).abs() < 1e-3);

        // ╔'s outer vertical lane (x = cx_left - off) bottom must be
        // y = ch (end of row-0 cell), which equals top of row-1.
        assert!(
            tl.iter().any(|s| (s.from.0 - (cx_left - off)).abs() < 1e-3
                && (s.to.0 - (cx_left - off)).abs() < 1e-3
                && (s.to.1 - ch).abs() < 1e-3),
            "scale {scale}× ╔ outer vertical lane must end at row boundary y=ch"
        );
    }

    #[test]
    fn double_three_by_three_continuity_at_100_percent_dpi() {
        check_double_3x3_continuity(1.0);
    }

    #[test]
    fn double_three_by_three_continuity_at_125_percent_dpi() {
        check_double_3x3_continuity(1.25);
    }

    #[test]
    fn double_three_by_three_continuity_at_150_percent_dpi() {
        check_double_3x3_continuity(1.5);
    }

    #[test]
    fn double_lane_offset_constant_gives_safe_gap_at_all_dpis() {
        // AA-safe lane gap: 2 * DOUBLE_LANE_OFFSET_PX is the
        // center-to-center spacing in logical pixels. At a stroke of
        // LIGHT_THICKNESS_PX (1.0 logical), the inter-lane gap in
        // logical pixels is 2*off - thickness. Multiply by scale to
        // get device-pixel gap; must be ≥ 1 at every supported DPI.
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let gap_logical = 2.0 * DOUBLE_LANE_OFFSET_PX - LIGHT_THICKNESS_PX;
            let gap_device = gap_logical * scale;
            assert!(
                gap_device >= 1.0,
                "scale {scale}×: inter-lane gap {gap_device} device-px must be ≥ 1 px"
            );
        }
    }

    #[test]
    fn predicate_matches_geometry_table_for_phase_b2() {
        // Helper/predicate sync: every codepoint that returns Some
        // from box_drawing_geometry MUST be true under
        // is_covered_box_drawing, and vice versa for the B2 set.
        for ch in ['═', '║', '╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let geom_some = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_some();
            let covered = is_covered_box_drawing(ch);
            assert_eq!(
                geom_some, covered,
                "predicate/geometry mismatch for U+{:04X} ('{ch}'): geom={geom_some} covered={covered}",
                ch as u32
            );
        }
    }
}
