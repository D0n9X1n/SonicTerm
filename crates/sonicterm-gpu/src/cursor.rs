//! Cursor-related rendering helpers.
//!
//! These helpers consume `QuadInstance` / `GlyphInstance` and emit
//! pixel-to-NDC quads, keeping cursor geometry on the GPU side of the
//! renderer-model boundary.

use crate::quad::{px_to_ndc, QuadInstance};
use sonicterm_text::GlyphInstance;

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
) {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, NDC inversion cannot produce a meaningful cursor overlap.
        return;
    }
    recolor_span(glyphs, (cell_x, cell_y, cell_w, cell_h), sw, sh, bg_rgba);
}

/// A glyph's `[x, y, w, h]` rectangle in surface pixels, inverted from its NDC rect.
///
/// The one reconstruction shared by recoloring and row ink bounds, so a row's ink
/// contains every rectangle the recolor test sees.
#[inline]
fn glyph_rect_px(glyph: &GlyphInstance, sw: f32, sh: f32) -> (f32, f32, f32, f32) {
    let [gx, gy, gw, gh] = glyph.rect;
    // Invert px_to_ndc: nx = (x/sw)*2 - 1 → x = (nx + 1) * sw / 2.
    // ny encodes the BOTTOM of the rect (after the +nh shift), so
    // y_top_px = (1 - gy - gh) * sh / 2.
    let px = (gx + 1.0) * sw * 0.5;
    let pw = gw * sw * 0.5;
    let py = (1.0 - gy - gh) * sh * 0.5;
    let ph = gh * sh * 0.5;
    (px, py, pw, ph)
}

/// Recolor every glyph in `glyphs` that lies at least 20% inside `target`.
fn recolor_span(
    glyphs: &mut [GlyphInstance],
    target: (f32, f32, f32, f32),
    sw: f32,
    sh: f32,
    bg_rgba: [f32; 4],
) {
    for glyph in glyphs.iter_mut() {
        let glyph_rect = glyph_rect_px(glyph, sw, sh);
        let (_, _, pw, ph) = glyph_rect;
        let overlap = aabb_overlap_area(target, glyph_rect);
        let glyph_area = (pw * ph).max(1.0);
        // Recolor glyphs that are actually under the cursor. A tiny right-edge
        // overhang from the previous glyph (common with italic/script fonts)
        // should not recolor the whole glyph to background and make it vanish.
        if aabb_intersects(target, glyph_rect) && overlap >= glyph_area * 0.20 {
            glyph.color = bg_rgba;
        }
    }
}

/// One terminal row's glyphs in the main glyph list and the union of their ink.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowGlyphSpan {
    /// The row's glyph indices in the main glyph list.
    pub glyphs: std::ops::Range<usize>,
    /// `[left, top, right, bottom]` in surface pixels over every glyph rectangle the
    /// recolor test reconstructs; `None` for a row with no glyphs or no surface.
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
                    let (px, py, pw, ph) = glyph_rect_px(glyph, sw, sh);
                    let edges = [px, py, px + pw, py + ph];
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
/// Recolors exactly what [`recolor_cursor_glyphs`] over the whole list would. Returns the
/// number of glyphs examined.
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
) -> usize {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is nonpositive, the full scan recolors nothing, so neither does this.
        return 0;
    }
    let target = (cell_x, cell_y, cell_w, cell_h);
    let total = glyphs.len();
    let mut visited = 0;
    let mut next = 0;
    for row in rows {
        // Rows are recorded in emission order and never overlap; clamping keeps a stale span
        // from indexing past the list.
        let start = row.glyphs.start.clamp(next, total);
        let end = row.glyphs.end.clamp(start, total);
        recolor_span(&mut glyphs[next..start], target, sw, sh, bg_rgba);
        visited += start - next;
        if row.ink_px.is_some_and(|ink| ink_meets(ink, target)) {
            // The row's ink union meets the target, so one of its glyphs may; scan them all.
            recolor_span(&mut glyphs[start..end], target, sw, sh, bg_rgba);
            visited += end - start;
        }
        next = end;
    }
    recolor_span(&mut glyphs[next..], target, sw, sh, bg_rgba);
    visited + (total - next)
}

/// Whether `ink` (`[left, top, right, bottom]`) meets `target` under the strict test
/// [`aabb_intersects`] applies to each glyph, so a row whose ink misses holds no glyph that hits.
#[inline]
fn ink_meets(ink: [f32; 4], target: (f32, f32, f32, f32)) -> bool {
    let (tx, ty, tw, th) = target;
    tx < ink[2] && tx + tw > ink[0] && ty < ink[3] && ty + th > ink[1]
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
