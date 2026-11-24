//! Cursor-related rendering helpers.
//!
//! These helpers consume `QuadInstance` / `GlyphInstance` and emit
//! pixel-to-NDC quads, keeping cursor geometry on the GPU side of the
//! renderer-model boundary.

use crate::quad::{px_to_ndc, QuadInstance};
use sonicterm_render_model::PixelRect;
use sonicterm_text::GlyphInstance;

/// Where a cursor recolor rewrote glyph colors, in surface pixels.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecolorBounds {
    /// No glyph was recolored.
    #[default]
    Empty,
    /// The outward-rounded union of every recolored glyph's pixel rectangle.
    Rect(PixelRect),
    /// A glyph or the cursor rectangle was not finite, so where recolored ink lands is unknown.
    Unbounded,
}

impl RecolorBounds {
    /// The bounds covering both `self` and `other`; `Unbounded` absorbs everything.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unbounded, _) | (_, Self::Unbounded) => Self::Unbounded,
            (Self::Empty, bounds) | (bounds, Self::Empty) => bounds,
            (Self::Rect(first), Self::Rect(second)) => Self::Rect(first.union(second)),
        }
    }
}

/// What one frame's cursor recolors rewrote: where, and a digest of the recolored instances.
///
/// Two records are equal only when the same instances (rectangle, atlas coordinates and color)
/// were recolored in the same order, so a replaced glyph under a stationary cursor changes it.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecolorRecord {
    /// Where the recolored glyphs draw.
    pub bounds: RecolorBounds,
    /// Order-sensitive digest of every recolored instance; 0 when nothing was recolored.
    pub hash: u64,
}

impl RecolorRecord {
    /// The record of `self` followed by `other`, as one sequence of recolors.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        let hash = match (self.hash, other.hash) {
            (0, hash) | (hash, 0) => hash,
            (first, second) => mix_hash(first, second),
        };
        Self { bounds: self.bounds.merge(other.bounds), hash }
    }

    /// Record one recolored glyph drawn at `glyph_px` (`x, y, w, h`, finite surface pixels).
    fn note_glyph(&mut self, glyph: &GlyphInstance, glyph_px: (f32, f32, f32, f32)) {
        let instance =
            Self { bounds: RecolorBounds::Rect(outward_rect(glyph_px)), hash: glyph_hash(glyph) };
        *self = self.merge(instance);
    }

    /// Record that a non-finite glyph or cursor rectangle was skipped.
    fn note_unbounded(&mut self) {
        self.bounds = RecolorBounds::Unbounded;
    }
}

/// The row-pruned recolor's result: glyphs examined, and what was recolored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecolorOutcome {
    /// Glyphs examined, for the `recolor_glyphs_visited` counter.
    pub visited: usize,
    /// Where and what the recolor rewrote.
    pub record: RecolorRecord,
}

/// Order-sensitive combination of two nonzero digests; never returns 0 for nonzero input.
fn mix_hash(first: u64, second: u64) -> u64 {
    let mixed = first.rotate_left(17) ^ second.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed.max(1)
}

/// Digest of one glyph instance's rectangle, atlas coordinates and color bits; never 0.
fn glyph_hash(glyph: &GlyphInstance) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    glyph.rect.map(f32::to_bits).hash(&mut hasher);
    glyph.uv.map(f32::to_bits).hash(&mut hasher);
    glyph.color.map(f32::to_bits).hash(&mut hasher);
    glyph.flags.map(f32::to_bits).hash(&mut hasher);
    hasher.finish().max(1)
}

/// The smallest whole-pixel rectangle containing finite `(x, y, w, h)`.
pub(crate) fn outward_rect((left, top, width, height): (f32, f32, f32, f32)) -> PixelRect {
    // Float-to-int `as` saturates, so huge finite ink clamps rather than wrapping.
    let x0 = left.floor() as i32;
    let y0 = top.floor() as i32;
    let x1 = (left + width).ceil() as i32;
    let y1 = (top + height).ceil() as i32;
    PixelRect {
        x: x0,
        y: y0,
        w: (i64::from(x1) - i64::from(x0)).clamp(0, i64::from(u32::MAX)) as u32,
        h: (i64::from(y1) - i64::from(y0)).clamp(0, i64::from(u32::MAX)) as u32,
    }
}

/// True when every component of a pixel rectangle is finite.
fn finite_rect((left, top, width, height): (f32, f32, f32, f32)) -> bool {
    left.is_finite() && top.is_finite() && width.is_finite() && height.is_finite()
}

/// Inactive pane cursor: grid row/column plus scalar pane bounds in physical window pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct InactivePaneCursor {
    /// Row (within the pane's grid) where the inactive cursor sits.
    pub row: u16,
    /// Column (within the pane's grid) where the inactive cursor sits.
    pub col: u16,
    /// Pane rect x in window pixels.
    pub rect_x: f32,
    /// Pane rect y in window pixels.
    pub rect_y: f32,
    /// Pane rect width in window pixels.
    pub rect_w: f32,
    /// Pane rect height in window pixels.
    pub rect_h: f32,
}

/// Push four thin quad rects forming the outline of `(cell_x, cell_y,
/// cell_w, cell_h)` with thickness `t` in pixels. Used for the
/// unfocused/inactive hollow cursor — the interior stays empty so the
/// glyph underneath remains readable.
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn push_hollow_rect(
    quads: &mut Vec<QuadInstance>,
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
    t: f32,
) {
    if sw <= 0.0 || sh <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        // When: `sw`, `sh`, `cell_w`, or `cell_h` is nonpositive, no valid cursor outline can be projected.
        return;
    }
    let t = t.min(cell_w * 0.5).min(cell_h * 0.5);
    // top
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y, cell_w, t, sw, sh),
        color,
        ..Default::default()
    });
    // bottom
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y + cell_h - t, cell_w, t, sw, sh),
        color,
        ..Default::default()
    });
    // left
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y, t, cell_h, sw, sh),
        color,
        ..Default::default()
    });
    // right
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x + cell_w - t, cell_y, t, cell_h, sw, sh),
        color,
        ..Default::default()
    });
}

/// Clip cursor edges without coupling this geometry helper to the composite renderer.
#[inline]
fn clip_rect_to_pane_local(
    rect: (f32, f32, f32, f32),
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = rect;
    let clipped_x = x.max(pane_x);
    let clipped_right = (x + w).min(pane_x + pane_w);
    let clipped_y = y.max(pane_y);
    let clipped_bottom = (y + h).min(pane_y + pane_h);
    let clipped_w = clipped_right - clipped_x;
    let clipped_h = clipped_bottom - clipped_y;
    if clipped_w > 0.0 && clipped_h > 0.0 {
        Some((clipped_x, clipped_y, clipped_w, clipped_h))
    } else {
        // When: `clipped_w` or `clipped_h` is nonpositive, this edge contributes no visible pane pixels.
        None
    }
}

/// Push a hollow rect outline clipped to a pane rect. Each of the four
/// edges is clipped independently so a cursor whose cell would extend
/// past the pane edge still draws the visible portion of the outline
/// without bleeding into a neighbouring split pane.
///
/// `pane_*` arguments are in physical pixels (same coordinate space as
/// `cell_*`).
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn push_hollow_rect_clipped(
    quads: &mut Vec<QuadInstance>,
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
    t: f32,
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) {
    if sw <= 0.0 || sh <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        // When: `sw`, `sh`, `cell_w`, or `cell_h` is nonpositive, no valid cursor outline can be projected.
        return;
    }
    let t = t.min(cell_w * 0.5).min(cell_h * 0.5);
    let edges = [
        // top
        (cell_x, cell_y, cell_w, t),
        // bottom
        (cell_x, cell_y + cell_h - t, cell_w, t),
        // left
        (cell_x, cell_y, t, cell_h),
        // right
        (cell_x + cell_w - t, cell_y, t, cell_h),
    ];
    for (ex, ey, ew, eh) in edges {
        if let Some((cx, cy, cw, ch)) =
            clip_rect_to_pane_local((ex, ey, ew, eh), pane_x, pane_y, pane_w, pane_h)
        {
            quads.push(QuadInstance {
                rect: px_to_ndc(cx, cy, cw, ch, sw, sh),
                color,
                ..Default::default()
            });
        }
    }
}

/// Axis-aligned bounding-box intersection test.
///
/// Treats each rect as `(x, y, w, h)` in the same coordinate space and
/// returns `true` iff the two rects overlap on both axes. Touching
/// edges (zero-area overlap) do NOT count as an intersection.
#[inline]
fn aabb_intersects(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by
}

#[inline]
fn aabb_overlap_area(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> f32 {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let x0 = ax.max(bx);
    let y0 = ay.max(by);
    let x1 = (ax + aw).min(bx + bw);
    let y1 = (ay + ah).min(by + bh);
    ((x1 - x0).max(0.0)) * ((y1 - y0).max(0.0))
}

/// Recolor every glyph instance whose rectangle intersects the cursor
/// cell to `bg_rgba`. Used to produce the wezterm-style "inverted"
/// block cursor: the foreground glyph is painted in the theme
/// background colour so it stays readable on top of the solid
/// cursor accent quad.
///
/// Walks the already-emitted instance list and rewrites their `color`
/// in place. Glyph rectangles are stored in NDC; we invert the
/// [`crate::quad::px_to_ndc`] mapping to test rect intersection in
/// pixel space (cleaner than reasoning about NDC sign conventions).
///
/// **AABB intersection** rather than a center-point test:
/// shaped glyphs for ligatures (`=>`, `===`) and wide characters
/// (CJK, emoji) emit a single [`GlyphInstance`] whose `rect` spans the
/// full cluster of cells. A center-point test misses every cluster
/// whose geometric centre lies in a cell other than the cursor cell,
/// so the lead cell of a `=>` ligature or the trail cell of a CJK
/// pair would render with the wrong foreground colour. The intersect
/// test recolours the glyph whenever any pixel of its rect falls
/// inside the cursor cell, matching the user's "cursor is on this
/// glyph" intuition.
///
/// O(N) over visible glyphs, with N being one frame's instance count.
/// In practice only a handful of glyphs overlap the cursor cell, so
/// this is effectively a single rewrite per frame.
///
/// Returns where the recolored glyphs draw and a digest of them. A non-finite glyph or
/// cursor rectangle is never recolored and makes the bounds [`RecolorBounds::Unbounded`].
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn recolor_cursor_glyphs(
    glyphs: &mut [GlyphInstance],
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    bg_rgba: [f32; 4],
) -> RecolorRecord {
    let mut record = RecolorRecord::default();
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, NDC inversion cannot produce a meaningful cursor overlap.
        return record;
    }
    let target = (cell_x, cell_y, cell_w, cell_h);
    if !finite_rect(target) {
        // When: `finite_rect` rejects the cursor `target`, no overlap is decidable and its ink is unbounded.
        record.note_unbounded();
        return record;
    }
    recolor_span(glyphs, target, sw, sh, bg_rgba, &mut record);
    record
}

/// Where `glyphs` draw on a `sw` x `sh` surface: the outward union of their pixel rectangles,
/// `Empty` for no glyph, and `Unbounded` for a non-finite glyph or an empty surface, whose ink
/// cannot be located.
pub(crate) fn glyph_ink_bounds(glyphs: &[GlyphInstance], sw: f32, sh: f32) -> RecolorBounds {
    if glyphs.is_empty() {
        // When: `glyphs` is empty, nothing was drawn.
        return RecolorBounds::Empty;
    }
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, no NDC rectangle maps back to pixels.
        return RecolorBounds::Unbounded;
    }
    glyphs.iter().fold(RecolorBounds::Empty, |bounds, glyph| {
        let glyph_px = glyph_rect_px(glyph, sw, sh);
        let this = if finite_rect(glyph_px) {
            RecolorBounds::Rect(outward_rect(glyph_px))
        } else {
            // When: `finite_rect` rejects `glyph_px`, where the glyph draws is unknown.
            RecolorBounds::Unbounded
        };
        bounds.merge(this)
    })
}

/// The atlas key of the test seams' solid tile: a noncharacter in a font slot and glyph id no font
/// occupies, so no real glyph ever shares its entry.
const SEAM_TILE_KEY: sonicterm_types::GlyphKey = sonicterm_types::GlyphKey {
    ch: '\u{FFFF}',
    font_slot: u8::MAX,
    weight_bold: false,
    italic: false,
    glyph_id: u32::MAX,
    raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
};

/// Rasterizes the test seams' tile: 4 x 4 pixels of full coverage.
struct SeamTile;

impl sonicterm_text::glyph_atlas::Rasterizer for SeamTile {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 4,
            height: 4,
            offset_x: 0,
            offset_y: 0,
            advance: 4.0,
            coverage: vec![u8::MAX; 16],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// A solid glyph drawing `rect_px` (`x, y, w, h` in surface pixels) in `color` from the test
/// seams' own atlas tile, inserted on first use; `None` on an empty surface or when the atlas
/// cannot hold the tile.
///
/// Only the renderer's test glyph seams call it. The tile is its own, so the glyph draws the same
/// whichever rows the frame emitted before it, including none.
pub(crate) fn seam_glyph(
    atlas: &mut sonicterm_text::glyph_atlas::GlyphAtlas,
    rect_px: (f32, f32, f32, f32),
    color: [f32; 4],
    sw: f32,
    sh: f32,
) -> Option<GlyphInstance> {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, no NDC rectangle can place the glyph.
        return None;
    }
    // The tile is a real insertion, so it counts like every other one.
    let info = atlas.get_or_insert(
        SEAM_TILE_KEY,
        &mut crate::frame_stats::CountingRasterizer::new(&mut SeamTile),
    )?;
    // Sample the tile's middle half, so bilinear filtering never reaches a neighbouring tile.
    let [u_min, v_min, u_max, v_max] = info.uv;
    let (inset_u, inset_v) = ((u_max - u_min) / 4.0, (v_max - v_min) / 4.0);
    let (left, top, width, height) = rect_px;
    Some(GlyphInstance {
        rect: px_to_ndc(left, top, width, height, sw, sh),
        uv: [u_min + inset_u, v_min + inset_v, u_max - inset_u, v_max - inset_v],
        color,
        flags: crate::core::glyph_flags(false, false),
    })
}

/// Recolor every quad in `quads` that overlaps `target` (`x, y, w, h` in surface px) to `rgba`.
///
/// Field tofu outlines are quads, not glyphs; a selection or caret drawn under them recolors
/// them as it recolors the field's glyphs, so a placeholder stays legible on that background.
pub(crate) fn recolor_cursor_quads(
    quads: &mut [QuadInstance],
    target: (f32, f32, f32, f32),
    sw: f32,
    sh: f32,
    rgba: [f32; 4],
) {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, NDC inversion cannot place a quad.
        return;
    }
    for quad in quads.iter_mut() {
        let [ndc_x, ndc_y, ndc_w, ndc_h] = quad.rect;
        let quad_px = (
            (ndc_x + 1.0) * sw * 0.5,
            (1.0 - ndc_y - ndc_h) * sh * 0.5,
            ndc_w * sw * 0.5,
            ndc_h * sh * 0.5,
        );
        // Any part of the quad on the target recolors the whole edge.
        if aabb_overlap_area(target, quad_px) > 0.0 {
            quad.color = rgba;
        }
    }
}

/// One mark on a text field: a block (`left, top, width, height` in surface px) drawn under the
/// field's text, and the foreground the glyphs and tofu outlines it covers take.
pub(crate) struct FieldMark {
    /// The block's rectangle in surface pixels.
    pub(crate) rect: (f32, f32, f32, f32),
    /// The block's premultiplied fill.
    pub(crate) background: [f32; 4],
    /// The color covered glyphs and tofu outlines take.
    pub(crate) foreground: [f32; 4],
}

/// Draw a text field's marks (selection, then caret) and then its tofu outlines. Each mark's block
/// is pushed and recolors the glyphs and tofu it covers; the tofu is pushed last, so a placeholder
/// under a mark stays visible above that mark's block. Search and palette fields both draw here.
pub(crate) fn paint_field_marks(
    quads: &mut Vec<QuadInstance>,
    glyphs: &mut [GlyphInstance],
    mut tofu: Vec<QuadInstance>,
    marks: &[FieldMark],
    sw: f32,
    sh: f32,
) {
    for mark in marks {
        let (left, top, width, height) = mark.rect;
        quads.push(QuadInstance {
            rect: px_to_ndc(left, top, width, height, sw, sh),
            color: mark.background,
            ..Default::default()
        });
        // Field marks draw on overlay slices whose changes are full-surface, so the record is unused.
        let _ = recolor_cursor_glyphs(glyphs, left, top, width, height, sw, sh, mark.foreground);
        recolor_cursor_quads(&mut tofu, mark.rect, sw, sh, mark.foreground);
    }
    quads.extend(tofu);
}

/// A glyph's `[x, y, w, h]` rectangle in surface pixels, inverted from its NDC rect.
///
/// The one reconstruction shared by recoloring and row ink bounds, so a row's ink
/// contains every rectangle the recolor test sees.
#[inline]
fn glyph_rect_px(glyph: &GlyphInstance, sw: f32, sh: f32) -> (f32, f32, f32, f32) {
    ndc_rect_px(glyph.rect, sw, sh)
}

/// An NDC `[x, y, w, h]` instance rectangle, as `px_to_ndc` encodes glyphs and quads, inverted to
/// `(x, y, w, h)` in surface pixels.
#[inline]
pub(crate) fn ndc_rect_px([gx, gy, gw, gh]: [f32; 4], sw: f32, sh: f32) -> (f32, f32, f32, f32) {
    // Invert px_to_ndc: nx = (x/sw)*2 - 1 → x = (nx + 1) * sw / 2.
    // ny encodes the BOTTOM of the rect (after the +nh shift), so
    // y_top_px = (1 - gy - gh) * sh / 2.
    let px = (gx + 1.0) * sw * 0.5;
    let pw = gw * sw * 0.5;
    let py = (1.0 - gy - gh) * sh * 0.5;
    let ph = gh * sh * 0.5;
    (px, py, pw, ph)
}

/// Recolor every glyph in `glyphs` that lies at least 20% inside `target`, noting each
/// recolored glyph, and any non-finite glyph, in `record`.
fn recolor_span(
    glyphs: &mut [GlyphInstance],
    target: (f32, f32, f32, f32),
    sw: f32,
    sh: f32,
    bg_rgba: [f32; 4],
    record: &mut RecolorRecord,
) {
    for glyph in glyphs.iter_mut() {
        let glyph_rect = glyph_rect_px(glyph, sw, sh);
        if !finite_rect(glyph_rect) {
            // When: `finite_rect` rejects `glyph_rect`, the glyph is never recolored (the strict
            // overlap test rejects it) but where it draws cannot be bounded.
            record.note_unbounded();
            continue;
        }
        let (_, _, pw, ph) = glyph_rect;
        let overlap = aabb_overlap_area(target, glyph_rect);
        let glyph_area = (pw * ph).max(1.0);
        // Recolor glyphs that are actually under the cursor. A tiny right-edge
        // overhang from the previous glyph (common with italic/script fonts)
        // should not recolor the whole glyph to background and make it vanish.
        if aabb_intersects(target, glyph_rect) && overlap >= glyph_area * 0.20 {
            glyph.color = bg_rgba;
            record.note_glyph(glyph, glyph_rect);
        }
    }
}

/// One terminal row's glyphs in the main glyph list and the union of their ink.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowGlyphSpan {
    /// The row's glyph indices in the main glyph list.
    pub glyphs: std::ops::Range<usize>,
    /// `[left, top, right, bottom]` in surface pixels over every glyph rectangle the
    /// recolor test reconstructs; `None` for a row with no glyphs or no surface. A row
    /// holding a non-finite glyph has infinite ink, so every target meets it and it is scanned.
    pub ink_px: Option<[f32; 4]>,
}

impl RowGlyphSpan {
    /// Record the row whose glyphs occupy `range` of `glyphs`, against a `sw` x `sh` surface.
    pub(crate) fn new(
        glyphs: &[GlyphInstance],
        range: std::ops::Range<usize>,
        sw: f32,
        sh: f32,
    ) -> Self {
        let ink_px = (sw > 0.0 && sh > 0.0)
            .then(|| {
                glyphs[range.clone()].iter().fold(None, |ink: Option<[f32; 4]>, glyph| {
                    let glyph_rect = glyph_rect_px(glyph, sw, sh);
                    let (px, py, pw, ph) = glyph_rect;
                    let edges = if finite_rect(glyph_rect) {
                        [px, py, px + pw, py + ph]
                    } else {
                        // When: `finite_rect` rejects `glyph_rect`, `min`/`max` would drop its NaN
                        // edges, so the row's ink is made infinite and the row is always scanned.
                        [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY]
                    };
                    Some(ink.map_or(edges, |acc| {
                        [
                            acc[0].min(edges[0]),
                            acc[1].min(edges[1]),
                            acc[2].max(edges[2]),
                            acc[3].max(edges[3]),
                        ]
                    }))
                })
            })
            .flatten();
        Self { glyphs: range, ink_px }
    }
}

/// Recolor the main glyph list's glyphs under `(cell_x, cell_y, cell_w, cell_h)`, scanning
/// only rows whose ink meets the target plus every glyph outside the recorded rows.
///
/// Recolors exactly what [`recolor_cursor_glyphs`] over the whole list would, and returns the
/// same record with the number of glyphs examined.
#[allow(clippy::too_many_arguments)]
pub(crate) fn recolor_cursor_glyphs_in(
    glyphs: &mut [GlyphInstance],
    rows: &[RowGlyphSpan],
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    bg_rgba: [f32; 4],
) -> RecolorOutcome {
    let mut outcome = RecolorOutcome::default();
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, the full scan recolors nothing, so neither does this.
        return outcome;
    }
    let target = (cell_x, cell_y, cell_w, cell_h);
    if !finite_rect(target) {
        // When: `finite_rect` rejects the cursor `target`, the full scan recolors nothing and is unbounded.
        outcome.record.note_unbounded();
        return outcome;
    }
    let record = &mut outcome.record;
    let total = glyphs.len();
    let mut visited = 0;
    let mut next = 0;
    for row in rows {
        // Rows are recorded in emission order and never overlap; clamping keeps a stale span
        // from indexing past the list.
        let start = row.glyphs.start.clamp(next, total);
        let end = row.glyphs.end.clamp(start, total);
        recolor_span(&mut glyphs[next..start], target, sw, sh, bg_rgba, record);
        visited += start - next;
        if row.ink_px.is_some_and(|ink| ink_meets(ink, target)) {
            // The row's ink union meets the target, so one of its glyphs may; scan them all.
            recolor_span(&mut glyphs[start..end], target, sw, sh, bg_rgba, record);
            visited += end - start;
        }
        next = end;
    }
    recolor_span(&mut glyphs[next..], target, sw, sh, bg_rgba, record);
    outcome.visited = visited + (total - next);
    outcome
}

/// Whether `ink` (`[left, top, right, bottom]`) meets `target` under the strict test
/// [`aabb_intersects`] applies to each glyph, so a row whose ink misses holds no glyph that hits.
#[inline]
fn ink_meets(ink: [f32; 4], target: (f32, f32, f32, f32)) -> bool {
    let (tx, ty, tw, th) = target;
    tx < ink[2] && tx + tw > ink[0] && ty < ink[3] && ty + th > ink[1]
}

/// A cut closer than this to a glyph edge, in surface pixels, is treated as that edge, so a glyph
/// already aligned to the selected cells is recolored whole rather than split into a sliver.
const CUT_EPSILON_PX: f32 = 1.0 / 64.0;

/// Draw the parts of the glyphs of `rows[pane]` (the owning pane's rows) that lie inside the
/// selected cells `targets` (surface-pixel rectangles) in `color`, and return the number of
/// glyphs examined.
///
/// A glyph wholly inside a target is recolored in place. A glyph crossing a target's edge, such as
/// a ligature, a wide glyph or one that overhangs its cell, is cut at that edge: the inside part
/// takes `color` and each outside part keeps its own color, each keeping the atlas coordinates of
/// the pixels it covers, so only the selected cells' ink changes. Edges are snapped to the pixel
/// boundary where the highlight's rows and columns begin, so the parts tile the glyph on both
/// presenters. The outside parts are placed right after the glyph they were cut from, so every
/// glyph keeps its paint order, and every row span in `rows` is moved to cover its glyphs again.
/// Only glyphs of the owning pane's rows, and parts cut from them, are read, so a neighbour's
/// overhang is never recolored. A color glyph ignores its color and keeps its pixels, so it is
/// never cut.
pub(crate) fn recolor_selected_cells(
    glyphs: &mut Vec<GlyphInstance>,
    rows: &mut [RowGlyphSpan],
    pane: std::ops::Range<usize>,
    targets: impl IntoIterator<Item = (f32, f32, f32, f32)>,
    (sw, sh): (f32, f32),
    color: [f32; 4],
) -> usize {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, no glyph rectangle can be reconstructed.
        return 0;
    }
    // Each outside part with the index of the glyph it was cut from, kept aside so the glyph
    // indices the rows record stay valid until the parts are placed.
    let mut cut_parts: Vec<(usize, GlyphInstance)> = Vec::new();
    let mut visited = 0;
    for target in targets.into_iter().filter(|target| finite_rect(*target)) {
        // Parts an earlier target cut lie outside it and may lie inside this one.
        let earlier_cuts = cut_parts.len();
        for row in rows.get(pane.clone()).unwrap_or_default() {
            if !row.ink_px.is_some_and(|ink| ink_meets(ink, target)) {
                // When: the row's ink misses `target`, none of its glyphs can lie inside it.
                continue;
            }
            for index in row.glyphs.clone() {
                visited += 1;
                let parts = recolor_glyph_part(&mut glyphs[index], target, (sw, sh), color);
                cut_parts.extend(parts.into_iter().flatten().map(|part| (index, part)));
            }
        }
        for part_index in 0..earlier_cuts {
            visited += 1;
            let origin = cut_parts[part_index].0;
            let parts = recolor_glyph_part(&mut cut_parts[part_index].1, target, (sw, sh), color);
            cut_parts.extend(parts.into_iter().flatten().map(|part| (origin, part)));
        }
    }
    if !cut_parts.is_empty() {
        // Some glyph was cut: its outside parts join the list after it and the rows move.
        place_cut_parts(glyphs, rows, cut_parts);
    }
    visited
}

/// Insert each of `cut_parts` right after the glyph it was cut from, in one pass from the end of
/// `glyphs`, and move every span of `rows` by the parts inserted before each of its bounds.
fn place_cut_parts(
    glyphs: &mut Vec<GlyphInstance>,
    rows: &mut [RowGlyphSpan],
    mut cut_parts: Vec<(usize, GlyphInstance)>,
) {
    // A stable sort keeps the parts of one glyph in the order they were cut.
    cut_parts.sort_by_key(|(origin, _)| *origin);
    let origins: Vec<usize> = cut_parts.iter().map(|(origin, _)| *origin).collect();
    let old_len = glyphs.len();
    glyphs.resize(old_len + cut_parts.len(), cut_parts[0].1);
    // From the end: `read` bounds the glyphs not yet moved, `write` the slots not yet filled.
    let (mut read, mut write) = (old_len, glyphs.len());
    for (origin, part) in cut_parts.into_iter().rev() {
        let tail = read - (origin + 1);
        glyphs.copy_within(origin + 1..read, write - tail);
        write -= tail + 1;
        read = origin + 1;
        glyphs[write] = part;
    }
    // A glyph at `bound` moves up by the parts of every glyph before it, so a span keeps its own
    // glyphs and their parts.
    let moved = |bound: usize| bound + origins.partition_point(|origin| *origin < bound);
    for row in rows {
        row.glyphs = moved(row.glyphs.start)..moved(row.glyphs.end);
    }
}

/// Snap a selection edge to the pixel boundary where its first covered row or column begins
/// (`ceil(edge - 0.5)`, the pixel-centre rule both presenters fill by), so a cut and the highlight
/// agree on every pixel; a whole-pixel edge is unchanged.
fn snap_selection_edge(edge: f32) -> f32 {
    (edge - 0.5).ceil()
}

/// Recolor the part of `glyph` inside `target` to `color` and return the parts outside it, each
/// with the glyph's own color (at most four bands); nothing changes for a color glyph or one that
/// misses `target`.
fn recolor_glyph_part(
    glyph: &mut GlyphInstance,
    target: (f32, f32, f32, f32),
    (sw, sh): (f32, f32),
    color: [f32; 4],
) -> [Option<GlyphInstance>; 4] {
    let original = *glyph;
    let (left, top, width, height) = glyph_rect_px(&original, sw, sh);
    let mut bands = [None; 4];
    if original.flags[0] >= 0.5
        || !finite_rect((left, top, width, height))
        || !aabb_intersects(target, (left, top, width, height))
    {
        // When: a color glyph ignores `color`, and a glyph missing `target` has nothing inside it.
        return bands;
    }
    let (right, bottom) = (left + width, top + height);
    let (target_left, target_top, target_width, target_height) = target;
    // Each cut is the target's snapped edge when it lies inside the glyph, else the glyph's own
    // edge.
    let cut = |edge: f32, low: f32, high: f32| {
        let edge = snap_selection_edge(edge);
        if edge <= low + CUT_EPSILON_PX {
            // The target edge is at or before the glyph's near edge, which bounds the part.
            low
        } else if edge >= high - CUT_EPSILON_PX {
            // When: `edge` is at or past `high`, the glyph's own far edge bounds the part.
            high
        } else {
            // When: `edge` lies strictly between `low` and `high`, the glyph is cut there.
            edge
        }
    };
    let inner_left = cut(target_left, left, right);
    let inner_right = cut(target_left + target_width, left, right);
    let inner_top = cut(target_top, top, bottom);
    let inner_bottom = cut(target_top + target_height, top, bottom);
    if inner_right <= inner_left || inner_bottom <= inner_top {
        // When: `inner_right <= inner_left` or `inner_bottom <= inner_top`, the glyph only grazes
        // `target`, so no pixel of it is selected.
        return bands;
    }
    let [u_left, v_top, u_right, v_bottom] = original.uv;
    // A part keeps the atlas coordinates of the pixels it covers, linear across the glyph.
    let part = |part_left: f32, part_top: f32, part_right: f32, part_bottom: f32, rgba| {
        let u_at = |pixel_x: f32| u_left + (u_right - u_left) * (pixel_x - left) / width;
        let v_at = |pixel_y: f32| v_top + (v_bottom - v_top) * (pixel_y - top) / height;
        GlyphInstance {
            rect: px_to_ndc(
                part_left,
                part_top,
                part_right - part_left,
                part_bottom - part_top,
                sw,
                sh,
            ),
            uv: [u_at(part_left), v_at(part_top), u_at(part_right), v_at(part_bottom)],
            color: rgba,
            flags: original.flags,
        }
    };
    if (inner_left, inner_top, inner_right, inner_bottom) == (left, top, right, bottom) {
        // When: `inner_left`..`inner_bottom` equal the glyph's `left`..`bottom`, it lies wholly
        // inside `target`, so it keeps its rectangle and only recolors.
        glyph.color = color;
        return bands;
    }
    *glyph = part(inner_left, inner_top, inner_right, inner_bottom, color);
    // The outside bands: full-width above and below, then left and right of the inside part.
    let edges = [
        (left, top, right, inner_top),
        (left, inner_bottom, right, bottom),
        (left, inner_top, inner_left, inner_bottom),
        (inner_right, inner_top, right, inner_bottom),
    ];
    for (slot, (band_left, band_top, band_right, band_bottom)) in edges.into_iter().enumerate() {
        if band_right > band_left && band_bottom > band_top {
            // The band holds pixels outside `target`, which keep the glyph's own color.
            bands[slot] = Some(part(band_left, band_top, band_right, band_bottom, original.color));
        }
    }
    bands
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
