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
//! Phase B3a adds the 46 mixed-weight junction codepoints — each
//! junction has 2–4 edges meeting at the center, with explicit
//! per-edge `StrokeWeight`. The internal [`EdgeStroke`] descriptor
//! captures one half-edge (center→cell-edge) with its own weight; the
//! [`emit_edge_strokes`] helper expands a list of [`EdgeStroke`]s into
//! the public [`LineSegment`] vector consumed by the renderer. The
//! public `LineSegment` shape is unchanged — only the *construction*
//! gains per-edge weight resolution. Codepoints covered (U+250D … U+254A):
//! `┍ ┎ ┑ ┒ ┕ ┖ ┙ ┚ ┝ ┞ ┟ ┠ ┡ ┢ ┥ ┦ ┧ ┨ ┩ ┪ ┭ ┮ ┯ ┰ ┱ ┲ ┵ ┶ ┷ ┸ ┹ ┺
//!  ┽ ┾ ┿ ╀ ╁ ╂ ╃ ╄ ╅ ╆ ╇ ╈ ╉ ╊`.
//!
//! Phase B3b adds the 18 light↔double mixed-junction codepoints —
//! one axis Single (`Light`), the other Double — emitted as
//! pre-clipped `Single`/`Light` lanes exactly like the Phase B2
//! double corners / T-junctions / cross. The double-axis half-edges
//! become two parallel lanes at `cy ± DOUBLE_LANE_OFFSET_PX` (or
//! `cx ± DOUBLE_LANE_OFFSET_PX`); the single-axis stem terminates at
//! the near double-lane (the "junction kernel boundary") so the
//! stems never cross or overshoot the double lanes. Codepoints:
//! `╒ ╓ ╕ ╖ ╘ ╙ ╛ ╜ ╞ ╟ ╡ ╢ ╤ ╥ ╧ ╨ ╪ ╫`.
//!
//! All other codepoints in the Box Drawing block (double/dashed/arc/
//! diagonal) return `None`; callers fall back to the existing
//! `BoxDrawingCellFill` glyph stretch path in
//! `swash_rasterizer::apply_symbol_fit`. Double-line codepoints
//! (U+2550..) are reserved for Phase B2; the [`StrokeStyle::Double`]
//! variant exists on [`LineSegment`] in anticipation of that phase but
//! is not emitted by Phase B1.
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
/// `Single` covers all Phase A + B1 codepoints. `Double` is reserved
/// for Phase B2 (U+2550..) and is NOT currently emitted by
/// [`box_drawing_geometry`]; it exists on [`LineSegment`] so the
/// renderer and consumers can be coded against the final data shape
/// without a second breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeStyle {
    /// One single line along the segment.
    Single,
    /// Two parallel lines offset perpendicular to the segment axis.
    /// Reserved for Phase B2 — `box_drawing_geometry` does not emit
    /// this variant in Phase B1.
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
/// which leaves a ≥ 1 device-pixel inter-lane gap at 100/125/150% DPI
/// when the per-lane stroke is `LIGHT_THICKNESS_PX`. The same constant
/// is used to place the pre-clipped lane segments of Phase B2
/// double-line corners / T-junctions / cross so the renderer's `Double`
/// splay for ═ ║ meets the corner lanes at pixel-identical inner
/// corners.
pub const DOUBLE_LANE_OFFSET_PX: f32 = 1.5;

/// Compass direction of one half-edge radiating from the cell center.
///
/// INTERNAL to Phase B3a: each mixed-weight junction codepoint is
/// modelled as a small set of [`EdgeStroke`]s — one per direction the
/// glyph extends — that the builder converts into the public
/// [`LineSegment`] list. The public data shape (`LineSegment`) is
/// unchanged; this enum only exists inside the codepoint table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeDir {
    /// Center → top edge (upward stem).
    Up,
    /// Center → bottom edge (downward stem).
    Down,
    /// Center → left edge (leftward stem).
    Left,
    /// Center → right edge (rightward stem).
    Right,
}

/// One half-edge of a mixed-weight junction.
///
/// Captures the direction from the cell center to the cell edge and
/// the stroke weight that should be applied to that single half-edge.
/// Two opposite [`EdgeStroke`]s with the same weight (e.g. `Up`+`Down`
/// Light) collapse into one full edge-to-edge `LineSegment` during
/// emission so the renderer continues to see the same number of
/// segments as the Phase B1 table for uniform-weight cases.
#[derive(Clone, Copy, Debug)]
struct EdgeStroke {
    dir: EdgeDir,
    weight: StrokeWeight,
    /// Stroke style for this half-edge. Phase B3a only emits
    /// [`StrokeStyle::Single`]; the field exists now so Phase B3b
    /// (mixed light/double) can carry per-edge style without another
    /// data-model bump.
    style: StrokeStyle,
}

impl EdgeStroke {
    const fn light(dir: EdgeDir) -> Self {
        Self { dir, weight: StrokeWeight::Light, style: StrokeStyle::Single }
    }
    const fn heavy(dir: EdgeDir) -> Self {
        Self { dir, weight: StrokeWeight::Heavy, style: StrokeStyle::Single }
    }
}

/// Translate a slice of [`EdgeStroke`]s into the [`LineSegment`]
/// vector that the renderer consumes.
///
/// Same-weight collinear pairs (e.g. `Up`+`Down` both `Light`) are
/// merged into a single edge-to-edge segment so the resulting geometry
/// matches the Phase A/B1 shapes when all edges share a weight. Mixed
/// weights stay as two separate center-anchored half-segments — that
/// is exactly what lets `┝` (light vert + heavy right) render as
/// `│` + heavy half-`━` with no overlap at the join.
fn emit_edge_strokes(
    edges: &[EdgeStroke],
    cell_origin: (f32, f32),
    cell_size: (f32, f32),
) -> Vec<LineSegment> {
    let (x, y) = cell_origin;
    let (w, h) = cell_size;
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let left = x;
    let right = x + w;
    let top = y;
    let bottom = y + h;

    let thickness_for = |weight: StrokeWeight| match weight {
        StrokeWeight::Light => LIGHT_THICKNESS_PX,
        StrokeWeight::Heavy => HEAVY_THICKNESS_PX,
    };
    let mk_seg = |from: (f32, f32), to: (f32, f32), weight: StrokeWeight, style: StrokeStyle| {
        LineSegment { from, to, thickness: thickness_for(weight), weight, style }
    };

    let find = |dir: EdgeDir| edges.iter().copied().find(|e| e.dir == dir);

    let mut out: Vec<LineSegment> = Vec::with_capacity(edges.len());

    // Vertical axis: merge Up+Down if same weight AND style, else emit independently.
    match (find(EdgeDir::Up), find(EdgeDir::Down)) {
        (Some(u), Some(d)) if u.weight == d.weight && u.style == d.style => {
            out.push(mk_seg((cx, top), (cx, bottom), u.weight, u.style));
        }
        (u_opt, d_opt) => {
            if let Some(u) = u_opt {
                out.push(mk_seg((cx, top), (cx, cy), u.weight, u.style));
            }
            if let Some(d) = d_opt {
                out.push(mk_seg((cx, cy), (cx, bottom), d.weight, d.style));
            }
        }
    }

    // Horizontal axis: merge Left+Right if same weight AND style, else emit independently.
    match (find(EdgeDir::Left), find(EdgeDir::Right)) {
        (Some(l), Some(r)) if l.weight == r.weight && l.style == r.style => {
            out.push(mk_seg((left, cy), (right, cy), l.weight, l.style));
        }
        (l_opt, r_opt) => {
            if let Some(l) = l_opt {
                out.push(mk_seg((left, cy), (cx, cy), l.weight, l.style));
            }
            if let Some(r) = r_opt {
                out.push(mk_seg((cx, cy), (right, cy), r.weight, r.style));
            }
        }
    }

    out
}

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

    // Phase B2 helper: pre-clipped per-lane `Single` segments for
    // double-line corners / T-junctions / cross.
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

    // Phase B2 helper: single centerline `Double`-style segment for
    // ═ ║. Renderer splays at ±DOUBLE_LANE_OFFSET_PX.
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
        // ═ U+2550 double horizontal — single centerline tagged Double.
        0x2550 => Some(mk_double_center(vec![((left, cy), (right, cy))])),
        // ║ U+2551 double vertical
        0x2551 => Some(mk_double_center(vec![((cx, top), (cx, bottom))])),
        // ╔ U+2554 double top-left corner — 4 pre-clipped lane segments.
        0x2554 => Some(mk_lanes(vec![
            ((cx - off, cy - off), (right, cy - off)),
            ((cx + off, cy + off), (right, cy + off)),
            ((cx - off, cy - off), (cx - off, bottom)),
            ((cx + off, cy + off), (cx + off, bottom)),
        ])),
        // ╗ U+2557 double top-right corner
        0x2557 => Some(mk_lanes(vec![
            ((left, cy - off), (cx + off, cy - off)),
            ((left, cy + off), (cx - off, cy + off)),
            ((cx + off, cy - off), (cx + off, bottom)),
            ((cx - off, cy + off), (cx - off, bottom)),
        ])),
        // ╚ U+255A double bottom-left corner
        0x255A => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, cy + off)),
            ((cx + off, top), (cx + off, cy - off)),
            ((cx - off, cy + off), (right, cy + off)),
            ((cx + off, cy - off), (right, cy - off)),
        ])),
        // ╝ U+255D double bottom-right corner
        0x255D => Some(mk_lanes(vec![
            ((cx + off, top), (cx + off, cy + off)),
            ((cx - off, top), (cx - off, cy - off)),
            ((left, cy + off), (cx + off, cy + off)),
            ((left, cy - off), (cx - off, cy - off)),
        ])),
        // ╠ U+2560 double left-T — outer (left) vertical lane is
        // continuous; inner (right) is broken by the horizontal arms.
        0x2560 => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, bottom)),
            ((cx + off, top), (cx + off, cy - off)),
            ((cx + off, cy + off), (cx + off, bottom)),
            ((cx + off, cy - off), (right, cy - off)),
            ((cx + off, cy + off), (right, cy + off)),
        ])),
        // ╣ U+2563 double right-T
        0x2563 => Some(mk_lanes(vec![
            ((cx + off, top), (cx + off, bottom)),
            ((cx - off, top), (cx - off, cy - off)),
            ((cx - off, cy + off), (cx - off, bottom)),
            ((left, cy - off), (cx - off, cy - off)),
            ((left, cy + off), (cx - off, cy + off)),
        ])),
        // ╦ U+2566 double top-T — outer (top) horizontal continuous.
        0x2566 => Some(mk_lanes(vec![
            ((left, cy - off), (right, cy - off)),
            ((left, cy + off), (cx - off, cy + off)),
            ((cx + off, cy + off), (right, cy + off)),
            ((cx - off, cy + off), (cx - off, bottom)),
            ((cx + off, cy + off), (cx + off, bottom)),
        ])),
        // ╩ U+2569 double bottom-T
        0x2569 => Some(mk_lanes(vec![
            ((left, cy + off), (right, cy + off)),
            ((left, cy - off), (cx - off, cy - off)),
            ((cx + off, cy - off), (right, cy - off)),
            ((cx - off, top), (cx - off, cy - off)),
            ((cx + off, top), (cx + off, cy - off)),
        ])),
        // ╬ U+256C double cross — 8 lane segments (4 arms × 2 lanes).
        // Central junction window is intentionally empty.
        0x256C => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, cy - off)),
            ((cx + off, top), (cx + off, cy - off)),
            ((cx - off, cy + off), (cx - off, bottom)),
            ((cx + off, cy + off), (cx + off, bottom)),
            ((left, cy - off), (cx - off, cy - off)),
            ((left, cy + off), (cx - off, cy + off)),
            ((cx + off, cy - off), (right, cy - off)),
            ((cx + off, cy + off), (right, cy + off)),
        ])),
        // ── Phase B3a: 46 mixed-weight junction codepoints ──────────
        // Each arm specifies the half-edges radiating from center with
        // explicit per-edge weight; `emit_edge_strokes` translates the
        // list into `LineSegment`s, merging same-weight collinear pairs
        // into single edge-to-edge segments (so e.g. `┿` returns 2
        // segments — one full heavy horizontal + one full light vertical
        // — exactly like `╋`/`┼` do for uniform weights). Unicode names
        // dictate weights: U+250D "BOX DRAWINGS DOWN LIGHT AND RIGHT
        // HEAVY" → Down=Light, Right=Heavy, etc.
        // ┍ U+250D — Down Light, Right Heavy
        0x250D => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::light(EdgeDir::Down), EdgeStroke::heavy(EdgeDir::Right)],
            cell_origin,
            cell_size,
        ))),
        // ┎ U+250E — Down Heavy, Right Light
        0x250E => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::heavy(EdgeDir::Down), EdgeStroke::light(EdgeDir::Right)],
            cell_origin,
            cell_size,
        ))),
        // ┑ U+2511 — Down Light, Left Heavy
        0x2511 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::light(EdgeDir::Down), EdgeStroke::heavy(EdgeDir::Left)],
            cell_origin,
            cell_size,
        ))),
        // ┒ U+2512 — Down Heavy, Left Light
        0x2512 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::heavy(EdgeDir::Down), EdgeStroke::light(EdgeDir::Left)],
            cell_origin,
            cell_size,
        ))),
        // ┕ U+2515 — Up Light, Right Heavy
        0x2515 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::light(EdgeDir::Up), EdgeStroke::heavy(EdgeDir::Right)],
            cell_origin,
            cell_size,
        ))),
        // ┖ U+2516 — Up Heavy, Right Light
        0x2516 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::heavy(EdgeDir::Up), EdgeStroke::light(EdgeDir::Right)],
            cell_origin,
            cell_size,
        ))),
        // ┙ U+2519 — Up Light, Left Heavy
        0x2519 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::light(EdgeDir::Up), EdgeStroke::heavy(EdgeDir::Left)],
            cell_origin,
            cell_size,
        ))),
        // ┚ U+251A — Up Heavy, Left Light
        0x251A => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[EdgeStroke::heavy(EdgeDir::Up), EdgeStroke::light(EdgeDir::Left)],
            cell_origin,
            cell_size,
        ))),
        // ┝ U+251D — Vertical Light, Right Heavy
        0x251D => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┞ U+251E — Up Heavy, Down Light, Right Light
        0x251E => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┟ U+251F — Up Light, Down Heavy, Right Light
        0x251F => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┠ U+2520 — Vertical Heavy, Right Light
        0x2520 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┡ U+2521 — Up Heavy, Down Light, Right Heavy
        0x2521 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┢ U+2522 — Up Light, Down Heavy, Right Heavy
        0x2522 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┥ U+2525 — Vertical Light, Left Heavy
        0x2525 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┦ U+2526 — Up Heavy, Down Light, Left Light
        0x2526 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┧ U+2527 — Up Light, Down Heavy, Left Light
        0x2527 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┨ U+2528 — Vertical Heavy, Left Light
        0x2528 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┩ U+2529 — Up Heavy, Down Light, Left Heavy
        0x2529 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┪ U+252A — Up Light, Down Heavy, Left Heavy
        0x252A => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┭ U+252D — Left Heavy, Right Light, Down Light
        0x252D => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┮ U+252E — Left Light, Right Heavy, Down Light
        0x252E => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┯ U+252F — Horizontal Heavy, Down Light
        0x252F => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┰ U+2530 — Horizontal Light, Down Heavy
        0x2530 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┱ U+2531 — Left Heavy, Right Light, Down Heavy
        0x2531 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┲ U+2532 — Left Light, Right Heavy, Down Heavy
        0x2532 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┵ U+2535 — Left Heavy, Right Light, Up Light
        0x2535 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┶ U+2536 — Left Light, Right Heavy, Up Light
        0x2536 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┷ U+2537 — Horizontal Heavy, Up Light
        0x2537 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┸ U+2538 — Horizontal Light, Up Heavy
        0x2538 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┹ U+2539 — Left Heavy, Right Light, Up Heavy
        0x2539 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┺ U+253A — Left Light, Right Heavy, Up Heavy
        0x253A => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┽ U+253D — Left Heavy, Right Light, Vertical Light
        0x253D => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┾ U+253E — Left Light, Right Heavy, Vertical Light
        0x253E => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ┿ U+253F — Horizontal Heavy, Vertical Light
        0x253F => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╀ U+2540 — Up Heavy, Down Light, Horizontal Light
        0x2540 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╁ U+2541 — Up Light, Down Heavy, Horizontal Light
        0x2541 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::light(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╂ U+2542 — Vertical Heavy, Horizontal Light
        0x2542 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╃ U+2543 — Left Heavy, Up Heavy, Right Light, Down Light
        0x2543 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╄ U+2544 — Right Heavy, Up Heavy, Left Light, Down Light
        0x2544 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╅ U+2545 — Left Heavy, Down Heavy, Right Light, Up Light
        0x2545 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╆ U+2546 — Right Heavy, Down Heavy, Left Light, Up Light
        0x2546 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╇ U+2547 — Horizontal Heavy, Up Heavy, Down Light
        0x2547 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::light(EdgeDir::Down),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╈ U+2548 — Horizontal Heavy, Down Heavy, Up Light
        0x2548 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::light(EdgeDir::Up),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╉ U+2549 — Vertical Heavy, Left Heavy, Right Light
        0x2549 => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Left),
                EdgeStroke::light(EdgeDir::Right),
            ],
            cell_origin,
            cell_size,
        ))),
        // ╊ U+254A — Vertical Heavy, Right Heavy, Left Light
        0x254A => Some(BoxGeometry::Lines(emit_edge_strokes(
            &[
                EdgeStroke::heavy(EdgeDir::Up),
                EdgeStroke::heavy(EdgeDir::Down),
                EdgeStroke::heavy(EdgeDir::Right),
                EdgeStroke::light(EdgeDir::Left),
            ],
            cell_origin,
            cell_size,
        ))),

        // ── Phase B3b: 18 light↔double mixed-junction codepoints ────
        // Each codepoint has one axis as light Single and the other as
        // Double. For Double edges we emit pre-clipped per-lane Single
        // segments (exactly as Phase B2 does for ╔/╗/etc.) — the inner
        // lane terminates at the junction kernel boundary so the
        // crossing single edge connects cleanly without overlap or
        // gap. All emitted segments are `Light`/`Single`: the doubled
        // appearance is encoded by emitting two parallel lanes per
        // double edge rather than by tagging a centerline `Double`.
        //
        // Naming convention from Unicode names: "DOWN SINGLE AND RIGHT
        // DOUBLE" → down-stem light, right-stem double, etc.
        // ╒ U+2552 — DOWN SINGLE AND RIGHT DOUBLE (top-left corner)
        0x2552 => Some(mk_lanes(vec![
            // Right edge double — two lanes from center to right edge
            ((cx, cy - off), (right, cy - off)),
            ((cx, cy + off), (right, cy + off)),
            // Down single — stem at cx spans both lanes down to bottom
            ((cx, cy - off), (cx, bottom)),
        ])),
        // ╓ U+2553 — DOWN DOUBLE AND RIGHT SINGLE (top-left corner)
        0x2553 => Some(mk_lanes(vec![
            // Down edge double — two lanes from cy down to bottom
            ((cx - off, cy), (cx - off, bottom)),
            ((cx + off, cy), (cx + off, bottom)),
            // Right single — spans both lanes out to right edge
            ((cx - off, cy), (right, cy)),
        ])),
        // ╕ U+2555 — DOWN SINGLE AND LEFT DOUBLE (top-right corner)
        0x2555 => Some(mk_lanes(vec![
            ((left, cy - off), (cx, cy - off)),
            ((left, cy + off), (cx, cy + off)),
            ((cx, cy - off), (cx, bottom)),
        ])),
        // ╖ U+2556 — DOWN DOUBLE AND LEFT SINGLE (top-right corner)
        0x2556 => Some(mk_lanes(vec![
            ((cx - off, cy), (cx - off, bottom)),
            ((cx + off, cy), (cx + off, bottom)),
            ((left, cy), (cx + off, cy)),
        ])),
        // ╘ U+2558 — UP SINGLE AND RIGHT DOUBLE (bottom-left corner)
        0x2558 => Some(mk_lanes(vec![
            ((cx, cy - off), (right, cy - off)),
            ((cx, cy + off), (right, cy + off)),
            ((cx, top), (cx, cy + off)),
        ])),
        // ╙ U+2559 — UP DOUBLE AND RIGHT SINGLE (bottom-left corner)
        0x2559 => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, cy)),
            ((cx + off, top), (cx + off, cy)),
            ((cx - off, cy), (right, cy)),
        ])),
        // ╛ U+255B — UP SINGLE AND LEFT DOUBLE (bottom-right corner)
        0x255B => Some(mk_lanes(vec![
            ((left, cy - off), (cx, cy - off)),
            ((left, cy + off), (cx, cy + off)),
            ((cx, top), (cx, cy + off)),
        ])),
        // ╜ U+255C — UP DOUBLE AND LEFT SINGLE (bottom-right corner)
        0x255C => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, cy)),
            ((cx + off, top), (cx + off, cy)),
            ((left, cy), (cx + off, cy)),
        ])),
        // ╞ U+255E — VERTICAL SINGLE AND RIGHT DOUBLE (left-T)
        0x255E => Some(mk_lanes(vec![
            ((cx, top), (cx, bottom)),
            ((cx, cy - off), (right, cy - off)),
            ((cx, cy + off), (right, cy + off)),
        ])),
        // ╟ U+255F — VERTICAL DOUBLE AND RIGHT SINGLE (left-T)
        0x255F => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, bottom)),
            ((cx + off, top), (cx + off, bottom)),
            ((cx + off, cy), (right, cy)),
        ])),
        // ╡ U+2561 — VERTICAL SINGLE AND LEFT DOUBLE (right-T)
        0x2561 => Some(mk_lanes(vec![
            ((cx, top), (cx, bottom)),
            ((left, cy - off), (cx, cy - off)),
            ((left, cy + off), (cx, cy + off)),
        ])),
        // ╢ U+2562 — VERTICAL DOUBLE AND LEFT SINGLE (right-T)
        0x2562 => Some(mk_lanes(vec![
            ((cx - off, top), (cx - off, bottom)),
            ((cx + off, top), (cx + off, bottom)),
            ((left, cy), (cx - off, cy)),
        ])),
        // ╤ U+2564 — DOWN SINGLE AND HORIZONTAL DOUBLE (top-T)
        0x2564 => Some(mk_lanes(vec![
            ((left, cy - off), (right, cy - off)),
            ((left, cy + off), (right, cy + off)),
            ((cx, cy + off), (cx, bottom)),
        ])),
        // ╥ U+2565 — DOWN DOUBLE AND HORIZONTAL SINGLE (top-T)
        0x2565 => Some(mk_lanes(vec![
            ((left, cy), (right, cy)),
            ((cx - off, cy), (cx - off, bottom)),
            ((cx + off, cy), (cx + off, bottom)),
        ])),
        // ╧ U+2567 — UP SINGLE AND HORIZONTAL DOUBLE (bottom-T)
        0x2567 => Some(mk_lanes(vec![
            ((left, cy - off), (right, cy - off)),
            ((left, cy + off), (right, cy + off)),
            ((cx, top), (cx, cy - off)),
        ])),
        // ╨ U+2568 — UP DOUBLE AND HORIZONTAL SINGLE (bottom-T)
        0x2568 => Some(mk_lanes(vec![
            ((left, cy), (right, cy)),
            ((cx - off, top), (cx - off, cy)),
            ((cx + off, top), (cx + off, cy)),
        ])),
        // ╪ U+256A — VERTICAL SINGLE AND HORIZONTAL DOUBLE (cross)
        0x256A => Some(mk_lanes(vec![
            ((cx, top), (cx, bottom)),
            ((left, cy - off), (right, cy - off)),
            ((left, cy + off), (right, cy + off)),
        ])),
        // ╫ U+256B — VERTICAL DOUBLE AND HORIZONTAL SINGLE (cross)
        0x256B => Some(mk_lanes(vec![
            ((left, cy), (right, cy)),
            ((cx - off, top), (cx - off, bottom)),
            ((cx + off, top), (cx + off, bottom)),
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
            // Phase B3a — 46 mixed light/heavy junctions
            | 0x250D
            | 0x250E
            | 0x2511
            | 0x2512
            | 0x2515
            | 0x2516
            | 0x2519
            | 0x251A
            | 0x251D
            | 0x251E
            | 0x251F
            | 0x2520
            | 0x2521
            | 0x2522
            | 0x2525
            | 0x2526
            | 0x2527
            | 0x2528
            | 0x2529
            | 0x252A
            | 0x252D
            | 0x252E
            | 0x252F
            | 0x2530
            | 0x2531
            | 0x2532
            | 0x2535
            | 0x2536
            | 0x2537
            | 0x2538
            | 0x2539
            | 0x253A
            | 0x253D
            | 0x253E
            | 0x253F
            | 0x2540
            | 0x2541
            | 0x2542
            | 0x2543
            | 0x2544
            | 0x2545
            | 0x2546
            | 0x2547
            | 0x2548
            | 0x2549
            | 0x254A
            // Phase B3b — 18 light↔double mixed-junction codepoints
            | 0x2552
            | 0x2553
            | 0x2555
            | 0x2556
            | 0x2558
            | 0x2559
            | 0x255B
            | 0x255C
            | 0x255E
            | 0x255F
            | 0x2561
            | 0x2562
            | 0x2564
            | 0x2565
            | 0x2567
            | 0x2568
            | 0x256A
            | 0x256B
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
        // phases C/D. NOTE: Phase B2 moved ═ ║ ╔ ╗ ╚ ╝ ╠ ╣ ╦ ╩ ╬ into
        // the covered set; they are no longer here.
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

    #[test]
    fn b3a_all_46_codepoints_return_some_and_are_covered() {
        // Phase B3a: every mixed-weight junction codepoint must produce
        // geometry AND be in the covered predicate so cache invalidation
        // (#559) catches it and the renderer doesn't fall back to glyph
        // stretch.
        for ch in B3A_CODEPOINTS {
            let geom = box_drawing_geometry(*ch, ORIGIN, (CELL_W, CELL_H));
            assert!(
                geom.is_some(),
                "Phase B3a codepoint U+{:04X} ('{ch}') must return geometry",
                *ch as u32
            );
            assert!(
                is_covered_box_drawing(*ch),
                "Phase B3a codepoint U+{:04X} ('{ch}') must be in is_covered_box_drawing",
                *ch as u32
            );
            let BoxGeometry::Lines(segs) = geom.unwrap();
            assert!(!segs.is_empty(), "U+{:04X} produced empty Lines", *ch as u32);
            // Every B3a codepoint must contain at least one heavy AND at
            // least one light stroke — that's the definition of "mixed".
            let has_light = segs.iter().any(|s| s.weight == StrokeWeight::Light);
            let has_heavy = segs.iter().any(|s| s.weight == StrokeWeight::Heavy);
            assert!(
                has_light && has_heavy,
                "U+{:04X} must mix both Light and Heavy strokes (got light={has_light} heavy={has_heavy})",
                *ch as u32
            );
            // Each emitted segment must have a thickness matching its
            // weight tag — the renderer (line_segment_to_quad) reads
            // seg.thickness directly so this is the load-bearing
            // invariant for B3a.
            for s in &segs {
                let expected = match s.weight {
                    StrokeWeight::Light => LIGHT_THICKNESS_PX,
                    StrokeWeight::Heavy => HEAVY_THICKNESS_PX,
                };
                assert!(
                    (s.thickness - expected).abs() < f32::EPSILON,
                    "U+{:04X} seg thickness {} != expected {} for weight {:?}",
                    *ch as u32,
                    s.thickness,
                    expected,
                    s.weight
                );
                assert_eq!(
                    s.style,
                    StrokeStyle::Single,
                    "B3a codepoints are single-stroke; Double is B2"
                );
            }
        }
    }

    const B3A_CODEPOINTS: &[char] = &[
        '┍', '┎', '┑', '┒', '┕', '┖', '┙', '┚', '┝', '┞', '┟', '┠', '┡', '┢', '┥', '┦', '┧', '┨',
        '┩', '┪', '┭', '┮', '┯', '┰', '┱', '┲', '┵', '┶', '┷', '┸', '┹', '┺', '┽', '┾', '┿', '╀',
        '╁', '╂', '╃', '╄', '╅', '╆', '╇', '╈', '╉', '╊',
    ];

    #[test]
    fn b3a_codepoint_count_is_46() {
        // Guard: spec is 46 codepoints. Trip if anyone edits the table
        // without updating the spec.
        assert_eq!(B3A_CODEPOINTS.len(), 46);
    }

    #[test]
    fn b3a_edge_weights_match_unicode_names() {
        // Spot-check a representative set covering each junction family
        // (corners, side-Ts, top/bottom-Ts, crosses, asymmetric crosses).
        // The check: each expected (direction, weight) appears as either
        // a center-anchored half-segment OR as an edge-to-edge segment
        // that crosses the cell center along that axis.
        struct Case {
            ch: char,
            // (dir_label, weight) pairs we expect to find geometry for.
            edges: &'static [(EdgeDir, StrokeWeight)],
        }
        let cases = [
            // ┍: Down Light, Right Heavy
            Case {
                ch: '┍',
                edges: &[
                    (EdgeDir::Down, StrokeWeight::Light),
                    (EdgeDir::Right, StrokeWeight::Heavy),
                ],
            },
            // ┝: Vertical Light + Right Heavy
            Case {
                ch: '┝',
                edges: &[
                    (EdgeDir::Up, StrokeWeight::Light),
                    (EdgeDir::Down, StrokeWeight::Light),
                    (EdgeDir::Right, StrokeWeight::Heavy),
                ],
            },
            // ┿: Horizontal Heavy + Vertical Light
            Case {
                ch: '┿',
                edges: &[
                    (EdgeDir::Left, StrokeWeight::Heavy),
                    (EdgeDir::Right, StrokeWeight::Heavy),
                    (EdgeDir::Up, StrokeWeight::Light),
                    (EdgeDir::Down, StrokeWeight::Light),
                ],
            },
            // ╂: Vertical Heavy + Horizontal Light
            Case {
                ch: '╂',
                edges: &[
                    (EdgeDir::Up, StrokeWeight::Heavy),
                    (EdgeDir::Down, StrokeWeight::Heavy),
                    (EdgeDir::Left, StrokeWeight::Light),
                    (EdgeDir::Right, StrokeWeight::Light),
                ],
            },
            // ╃: Left Heavy, Up Heavy, Right Light, Down Light
            Case {
                ch: '╃',
                edges: &[
                    (EdgeDir::Left, StrokeWeight::Heavy),
                    (EdgeDir::Up, StrokeWeight::Heavy),
                    (EdgeDir::Right, StrokeWeight::Light),
                    (EdgeDir::Down, StrokeWeight::Light),
                ],
            },
        ];
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let left = ORIGIN.0;
        let right = ORIGIN.0 + CELL_W;
        let top = ORIGIN.1;
        let bottom = ORIGIN.1 + CELL_H;
        for c in &cases {
            let segs = lines(c.ch);
            for &(dir, weight) in c.edges {
                let edge_pt = match dir {
                    EdgeDir::Up => (cx, top),
                    EdgeDir::Down => (cx, bottom),
                    EdgeDir::Left => (left, cy),
                    EdgeDir::Right => (right, cy),
                };
                let center = (cx, cy);
                let found = segs.iter().any(|s| {
                    s.weight == weight
                        && ((s.from == edge_pt && s.to == center)
                            || (s.from == center && s.to == edge_pt)
                            || (dir == EdgeDir::Up || dir == EdgeDir::Down)
                                && s.from == (cx, top)
                                && s.to == (cx, bottom)
                            || (dir == EdgeDir::Left || dir == EdgeDir::Right)
                                && s.from == (left, cy)
                                && s.to == (right, cy))
                });
                assert!(
                    found,
                    "U+{:04X} ('{}') missing edge ({:?}, {:?}); segs = {:#?}",
                    c.ch as u32, c.ch, dir, weight, segs
                );
            }
        }
    }

    #[test]
    fn b3a_horizontal_continuity_with_neighbours() {
        // The whole point of per-edge weights: a row like `─┿━` must
        // join pixel-identically at both cell-cell midline points and
        // the renderer must be told each side's weight independently so
        // the visible stroke transitions cleanly mid-row.
        let cw = 8.0_f32;
        let ch = 16.0_f32;
        let cy = ch * 0.5;

        let light_left = lines_at('─', (0.0, 0.0), (cw, ch));
        let mid = lines_at('┿', (cw, 0.0), (cw, ch));
        let heavy_right = lines_at('━', (2.0 * cw, 0.0), (cw, ch));

        let left_h = light_left[0];
        let mid_h = mid.iter().find(|s| s.from.1 == cy && s.to.1 == cy).unwrap();
        assert_eq!(
            left_h.to, mid_h.from,
            "─→┿ horizontal join must be pixel-identical (Phase B3a continuity)"
        );
        let right_h = heavy_right[0];
        assert_eq!(
            mid_h.to, right_h.from,
            "┿→━ horizontal join must be pixel-identical (Phase B3a continuity)"
        );
        assert_eq!(mid_h.weight, StrokeWeight::Heavy);
        let mid_v =
            mid.iter().find(|s| s.from.0 == cw + cw * 0.5 && s.to.0 == cw + cw * 0.5).unwrap();
        assert_eq!(mid_v.weight, StrokeWeight::Light);
    }

    #[test]
    fn b3a_vertical_continuity_with_neighbours() {
        // Column like:  │      (light vert)
        //               ╂      (heavy vert + light horiz cross)
        //               ┃      (heavy vert)
        let cw = 8.0_f32;
        let ch = 16.0_f32;
        let cx = cw * 0.5;

        let v_top = lines_at('│', (0.0, 0.0), (cw, ch));
        let mid = lines_at('╂', (0.0, ch), (cw, ch));
        let v_bot = lines_at('┃', (0.0, 2.0 * ch), (cw, ch));

        let v_top_seg = v_top[0];
        let mid_v = mid.iter().find(|s| s.from.0 == cx && s.to.0 == cx).unwrap();
        assert_eq!(v_top_seg.to, mid_v.from, "│→╂ vertical join must be pixel-identical");
        assert_eq!(mid_v.weight, StrokeWeight::Heavy, "╂ vertical lane must be Heavy");

        let v_bot_seg = v_bot[0];
        assert_eq!(mid_v.to, v_bot_seg.from, "╂→┃ vertical join must be pixel-identical");

        let mid_h =
            mid.iter().find(|s| s.from.1 == ch + ch * 0.5 && s.to.1 == ch + ch * 0.5).unwrap();
        assert_eq!(mid_h.weight, StrokeWeight::Light);
    }

    #[test]
    fn b3a_heavy_cross_baseline_still_emits_two_full_segments() {
        // Regression guard for B1's ╋ shape — it must keep emitting
        // exactly 2 segments (full heavy horizontal + full heavy
        // vertical), unchanged by the B3a additions.
        let segs = lines('╋');
        assert_eq!(segs.len(), 2);
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        assert!(segs.iter().any(|s| s.from == (ORIGIN.0, cy)
            && s.to == (ORIGIN.0 + CELL_W, cy)
            && s.weight == StrokeWeight::Heavy));
        assert!(segs.iter().any(|s| s.from == (cx, ORIGIN.1)
            && s.to == (cx, ORIGIN.1 + CELL_H)
            && s.weight == StrokeWeight::Heavy));
    }

    #[test]
    fn b3a_same_weight_opposite_edges_merge_into_full_segment() {
        // Internal contract of emit_edge_strokes: when Up+Down (or
        // Left+Right) share a weight, they collapse into one full
        // edge-to-edge segment — so e.g. ┝ produces exactly 2 segs
        // (full light vertical + half heavy horizontal).
        let segs = lines('┝');
        assert_eq!(segs.len(), 2, "┝ should produce one merged vertical + one half horizontal");
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        assert!(segs.iter().any(|s| s.from == (cx, ORIGIN.1)
            && s.to == (cx, ORIGIN.1 + CELL_H)
            && s.weight == StrokeWeight::Light));
        assert!(segs.iter().any(|s| s.from == (cx, cy)
            && s.to == (ORIGIN.0 + CELL_W, cy)
            && s.weight == StrokeWeight::Heavy));
    }

    fn check_continuity_at_scale(scale: f32) {
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
        for ch in ['═', '║', '╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let geom = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H));
            assert!(
                geom.is_some(),
                "Phase B2 codepoint U+{:04X} ('{ch}') must return geometry",
                ch as u32
            );
            assert!(is_covered_box_drawing(ch));
            let BoxGeometry::Lines(segs) = geom.unwrap();
            assert!(!segs.is_empty(), "U+{:04X} produced empty Lines", ch as u32);
        }
    }

    #[test]
    fn double_straights_use_double_style_centerline() {
        for ch in ['═', '║'] {
            let segs = lines(ch);
            assert_eq!(segs.len(), 1, "U+{:04X} = one centerline segment", ch as u32);
            assert_eq!(
                segs[0].style,
                StrokeStyle::Double,
                "U+{:04X} centerline must be StrokeStyle::Double",
                ch as u32
            );
        }
    }

    #[test]
    fn double_junctions_use_single_pre_clipped_lanes() {
        for ch in ['╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let segs = lines(ch);
            assert!(segs.len() >= 4, "U+{:04X} needs ≥ 4 lanes", ch as u32);
            for s in &segs {
                assert_eq!(
                    s.style,
                    StrokeStyle::Single,
                    "U+{:04X} lane must be Single (pre-clipped)",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn double_cross_inner_corner_coordinates_no_overshoot() {
        // ╬ — every endpoint must be outside the strict junction
        // window (cx ± off, cy ± off). Per Opus Step-2.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let segs = lines('╬');
        assert_eq!(segs.len(), 8, "╬ = 8 lane segments");
        for s in &segs {
            for &p in &[s.from, s.to] {
                let inside_x = p.0 > cx - off && p.0 < cx + off;
                let inside_y = p.1 > cy - off && p.1 < cy + off;
                assert!(!(inside_x && inside_y), "╬ endpoint {:?} overshoots junction window", p);
            }
        }
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
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let segs = lines('╔');
        assert_eq!(segs.len(), 4, "╔ = 4 lanes");
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
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let off = DOUBLE_LANE_OFFSET_PX;
        let l = lines('╠');
        assert!(
            l.iter()
                .any(|s| s.from == (cx - off, ORIGIN.1) && s.to == (cx - off, ORIGIN.1 + CELL_H)),
            "╠ outer vertical lane must be continuous"
        );
        let r = lines('╣');
        assert!(
            r.iter()
                .any(|s| s.from == (cx + off, ORIGIN.1) && s.to == (cx + off, ORIGIN.1 + CELL_H)),
            "╣ outer vertical lane must be continuous"
        );
        let t = lines('╦');
        assert!(
            t.iter()
                .any(|s| s.from == (ORIGIN.0, cy - off) && s.to == (ORIGIN.0 + CELL_W, cy - off)),
            "╦ outer horizontal lane must be continuous"
        );
        let b = lines('╩');
        assert!(
            b.iter()
                .any(|s| s.from == (ORIGIN.0, cy + off) && s.to == (ORIGIN.0 + CELL_W, cy + off)),
            "╩ outer horizontal lane must be continuous"
        );
    }

    fn check_double_3x3_continuity(scale: f32) {
        // ╔══╗ / ║  ║ / ╚══╝
        let off = DOUBLE_LANE_OFFSET_PX;
        let cw = 8.0_f32 * scale;
        let ch = 16.0_f32 * scale;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);

        let tl = lines_at('╔', cell(0, 0), (cw, ch));
        let h0a = lines_at('═', cell(1, 0), (cw, ch));
        let _h0b = lines_at('═', cell(2, 0), (cw, ch));
        let _tr = lines_at('╗', cell(3, 0), (cw, ch));

        let cy_top = ch * 0.5;
        assert_eq!(h0a[0].style, StrokeStyle::Double, "scale {scale}× ═ must be Double");
        assert!(
            (h0a[0].from.0 - cw).abs() < 1e-3 && (h0a[0].from.1 - cy_top).abs() < 1e-3,
            "scale {scale}× ═ centerline starts at left cell edge"
        );
        // ╔ outer vertical lane (x = cx_left - off) must end at row
        // boundary y = ch.
        let cx_left = cw * 0.5;
        assert!(
            tl.iter().any(|s| (s.from.0 - (cx_left - off)).abs() < 1e-3
                && (s.to.0 - (cx_left - off)).abs() < 1e-3
                && (s.to.1 - ch).abs() < 1e-3),
            "scale {scale}× ╔ outer vertical lane must end at y=ch"
        );
        // ║ centerline in row 1 must span row 1 from y=ch to y=2ch.
        let v_mid = lines_at('║', cell(0, 1), (cw, ch));
        assert_eq!(v_mid[0].style, StrokeStyle::Double);
        assert!((v_mid[0].from.0 - cx_left).abs() < 1e-3);
        assert!((v_mid[0].from.1 - ch).abs() < 1e-3);
        assert!((v_mid[0].to.1 - 2.0 * ch).abs() < 1e-3);
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
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let gap_logical = 2.0 * DOUBLE_LANE_OFFSET_PX - LIGHT_THICKNESS_PX;
            let gap_device = gap_logical * scale;
            assert!(
                gap_device >= 1.0,
                "scale {scale}×: inter-lane gap {gap_device} device-px must be ≥ 1"
            );
        }
    }

    #[test]
    fn predicate_matches_geometry_table_for_phase_b2() {
        for ch in ['═', '║', '╔', '╗', '╚', '╝', '╠', '╣', '╦', '╩', '╬'] {
            let geom_some = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_some();
            let covered = is_covered_box_drawing(ch);
            assert_eq!(geom_some, covered, "predicate/geometry mismatch for U+{:04X}", ch as u32);
        }
    }

    // ─── Phase B3b (18 light↔double mixed-junction) tests ──────────

    /// Canonical list of the 18 Phase B3b codepoints.
    const B3B_CODEPOINTS: &[char] =
        &['╒', '╓', '╕', '╖', '╘', '╙', '╛', '╜', '╞', '╟', '╡', '╢', '╤', '╥', '╧', '╨', '╪', '╫'];

    #[test]
    fn all_eighteen_phase_b3b_codepoints_return_some() {
        for &ch in B3B_CODEPOINTS {
            let geom = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H));
            assert!(
                geom.is_some(),
                "Phase B3b codepoint U+{:04X} ('{ch}') must return geometry",
                ch as u32
            );
            let BoxGeometry::Lines(segs) = geom.unwrap();
            assert!(segs.len() >= 3, "U+{:04X} ('{ch}') needs ≥ 3 lane segments", ch as u32);
        }
    }

    #[test]
    fn phase_b3b_codepoints_emit_only_single_style_pre_clipped_lanes() {
        // Like Phase B2 corner/T/cross, Phase B3b emits pre-clipped
        // Single lanes (the double-axis is expressed by emitting two
        // parallel lanes rather than one Double centerline). The
        // renderer-side splay therefore never sees these segments —
        // mixed junctions stay self-contained per codepoint.
        for &ch in B3B_CODEPOINTS {
            let segs = lines(ch);
            for s in &segs {
                assert_eq!(
                    s.style,
                    StrokeStyle::Single,
                    "U+{:04X} ('{ch}') lane must be Single (pre-clipped); got {:?}",
                    ch as u32,
                    s.style
                );
                assert_eq!(
                    s.weight,
                    StrokeWeight::Light,
                    "U+{:04X} ('{ch}') lane weight must be Light",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn b3b_predicate_matches_geometry_table() {
        for &ch in B3B_CODEPOINTS {
            let geom_some = box_drawing_geometry(ch, ORIGIN, (CELL_W, CELL_H)).is_some();
            let covered = is_covered_box_drawing(ch);
            assert!(
                geom_some && covered,
                "predicate/geometry mismatch for B3b U+{:04X}",
                ch as u32
            );
        }
    }

    #[test]
    fn b3b_cross_horiz_double_vert_single_has_correct_lane_pattern() {
        // ╪ U+256A: horizontal double + vertical single. Expect:
        //   - 1 vertical centerline at cx, top→bottom
        //   - 2 horizontal lanes at cy ± off, left→right
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let left = ORIGIN.0;
        let right = ORIGIN.0 + CELL_W;
        let top = ORIGIN.1;
        let bottom = ORIGIN.1 + CELL_H;
        let segs = lines('╪');
        assert_eq!(segs.len(), 3, "╪ = 1 vertical + 2 horizontal lanes");
        assert!(
            segs.iter().any(|s| s.from == (cx, top) && s.to == (cx, bottom)),
            "╪ vertical single centerline missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (left, cy - off) && s.to == (right, cy - off)),
            "╪ upper horizontal lane missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (left, cy + off) && s.to == (right, cy + off)),
            "╪ lower horizontal lane missing"
        );
    }

    #[test]
    fn b3b_cross_vert_double_horiz_single_has_correct_lane_pattern() {
        // ╫ U+256B: vertical double + horizontal single.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let left = ORIGIN.0;
        let right = ORIGIN.0 + CELL_W;
        let top = ORIGIN.1;
        let bottom = ORIGIN.1 + CELL_H;
        let segs = lines('╫');
        assert_eq!(segs.len(), 3, "╫ = 1 horizontal + 2 vertical lanes");
        assert!(
            segs.iter().any(|s| s.from == (left, cy) && s.to == (right, cy)),
            "╫ horizontal single centerline missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx - off, top) && s.to == (cx - off, bottom)),
            "╫ left vertical lane missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx + off, top) && s.to == (cx + off, bottom)),
            "╫ right vertical lane missing"
        );
    }

    #[test]
    fn b3b_bottom_t_horiz_double_vert_single_continuity() {
        // ╧ U+2567: horizontal double + up-stem single. The up stem
        // must terminate at the upper lane (cy - off) — NOT at cy —
        // so it kisses the inside of the double horizontal without
        // crossing it. This is the junction-kernel pre-clip contract
        // from B2 carried into B3b.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let left = ORIGIN.0;
        let right = ORIGIN.0 + CELL_W;
        let top = ORIGIN.1;
        let segs = lines('╧');
        assert_eq!(segs.len(), 3, "╧ = 2 horizontal lanes + 1 up stem");
        assert!(
            segs.iter().any(|s| s.from == (left, cy - off) && s.to == (right, cy - off)),
            "╧ upper lane missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (left, cy + off) && s.to == (right, cy + off)),
            "╧ lower lane missing"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx, top) && s.to == (cx, cy - off)),
            "╧ up stem must terminate at upper lane (junction-kernel pre-clip)"
        );
    }

    #[test]
    fn b3b_top_left_corner_right_double_down_single() {
        // ╒ U+2552: right edge double + down stem single.
        // The down stem starts at the upper lane (cy - off) so both
        // lanes are visually anchored to it.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cx = ORIGIN.0 + CELL_W * 0.5;
        let cy = ORIGIN.1 + CELL_H * 0.5;
        let right = ORIGIN.0 + CELL_W;
        let bottom = ORIGIN.1 + CELL_H;
        let segs = lines('╒');
        assert_eq!(segs.len(), 3, "╒ = 2 right lanes + 1 down stem");
        assert!(
            segs.iter().any(|s| s.from == (cx, cy - off) && s.to == (right, cy - off)),
            "╒ upper right lane must originate at (cx, cy-off)"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx, cy + off) && s.to == (right, cy + off)),
            "╒ lower right lane must originate at (cx, cy+off)"
        );
        assert!(
            segs.iter().any(|s| s.from == (cx, cy - off) && s.to == (cx, bottom)),
            "╒ down single stem must span from upper lane to bottom"
        );
    }

    #[test]
    fn b3b_baseline_phase_b2_corner_still_unchanged() {
        // Regression guard: ╔ baseline from Phase B2 must keep its
        // exact 4-lane shape — B3b additions must not perturb it.
        let segs = lines('╔');
        assert_eq!(segs.len(), 4, "╔ must still be exactly 4 lanes (Phase B2 baseline)");
    }

    fn check_b3b_horiz_double_continuity(scale: f32) {
        // ═╪═ — the horizontal double centerline crossing a Phase B3b
        // mixed cross must abut pixel-identically. The neighbour ═ is
        // a Double centerline (renderer splays to two lanes at cy±off);
        // ╪'s pre-clipped lanes sit at the same cy±off so the lanes
        // are visually continuous post-splay.
        let off = DOUBLE_LANE_OFFSET_PX;
        let cw = 8.0_f32 * scale;
        let ch = 16.0_f32 * scale;
        let cell = |col: usize, row: usize| (col as f32 * cw, row as f32 * ch);
        let left = lines_at('═', cell(0, 0), (cw, ch));
        let mid = lines_at('╪', cell(1, 0), (cw, ch));
        let right = lines_at('═', cell(2, 0), (cw, ch));
        let cy = ch * 0.5;
        // Left ═ centerline ends at x=cw.
        assert_eq!(left.len(), 1);
        assert!((left[0].to.0 - cw).abs() < 1e-3);
        assert!((left[0].to.1 - cy).abs() < 1e-3);
        // Mid ╪ upper lane starts at x=cw, y=cy-off.
        assert!(
            mid.iter().any(|s| (s.from.0 - cw).abs() < 1e-3
                && (s.from.1 - (cy - off)).abs() < 1e-3
                && (s.to.0 - 2.0 * cw).abs() < 1e-3),
            "scale {scale}× ═→╪ upper lane must start at left cell-edge midline"
        );
        // Right ═ centerline starts at x=2cw.
        assert!((right[0].from.0 - 2.0 * cw).abs() < 1e-3);
    }

    #[test]
    fn b3b_horizontal_double_continuity_at_100_percent_dpi() {
        check_b3b_horiz_double_continuity(1.0);
    }

    #[test]
    fn b3b_horizontal_double_continuity_at_125_percent_dpi() {
        check_b3b_horiz_double_continuity(1.25);
    }

    #[test]
    fn b3b_horizontal_double_continuity_at_150_percent_dpi() {
        check_b3b_horiz_double_continuity(1.5);
    }
}
