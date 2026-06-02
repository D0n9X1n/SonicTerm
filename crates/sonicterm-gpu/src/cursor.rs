//! Cursor-related rendering helpers extracted from `render.rs` (issue #143).
//!
//! Moved from `sonicterm-shared::render::cursor` in M7e of the workspace
//! refactor: all helpers consume `QuadInstance` / `GlyphInstance` and
//! emit pixel-to-NDC quads, so they belong on the GPU side of the layer
//! split.

use crate::quad::{px_to_ndc, QuadInstance};
use crate::text_pipeline::GlyphInstance;

/// One inactive pane's cursor: the cell coordinates inside that pane
/// plus the pane's rectangle in window pixels. Carried as a flat
/// struct (rather than a tuple) so the renderer can extend the
/// payload (e.g. with the pane's bg color) without ripple changes.
///
/// The rectangle is stored as raw `f32` fields (rather than a
/// `sonicterm_ui::pane::Rect`) so this struct stays free of any
/// dependency on `sonicterm-ui` — `sonicterm-ui` already depends on
/// `sonicterm-gpu`, and a back-edge would create a cycle.
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

/// Local mirror of `sonicterm_gpu::core::clip_rect_to_pane`,
/// kept private so this module has no upward dep on `sonicterm-shared`.
/// Tiny enough that duplication beats wiring a back-edge crate just for
/// this helper.
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
/// **AABB intersection** (issue #568) rather than a center-point test:
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
        return;
    }
    let cursor_rect = (cell_x, cell_y, cell_w, cell_h);
    for g in glyphs.iter_mut() {
        let [gx, gy, gw, gh] = g.rect;
        // Invert px_to_ndc: nx = (x/sw)*2 - 1 → x = (nx + 1) * sw / 2.
        // ny encodes the BOTTOM of the rect (after the +nh shift), so
        // y_top_px = (1 - gy - gh) * sh / 2.
        let px = (gx + 1.0) * sw * 0.5;
        let pw = gw * sw * 0.5;
        let py = (1.0 - gy - gh) * sh * 0.5;
        let ph = gh * sh * 0.5;
        if aabb_intersects(cursor_rect, (px, py, pw, ph)) {
            g.color = bg_rgba;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aabb_intersects_basic() {
        // Overlapping rects intersect.
        assert!(aabb_intersects((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 10.0, 10.0)));
        // Disjoint rects don't intersect.
        assert!(!aabb_intersects(
            (0.0, 0.0, 10.0, 10.0),
            (20.0, 0.0, 10.0, 10.0)
        ));
        // Touching edges (zero-area overlap) don't count.
        assert!(!aabb_intersects(
            (0.0, 0.0, 10.0, 10.0),
            (10.0, 0.0, 10.0, 10.0)
        ));
        // Contained rect intersects.
        assert!(aabb_intersects((0.0, 0.0, 10.0, 10.0), (2.0, 2.0, 4.0, 4.0)));
    }
}
