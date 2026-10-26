//! Row geometry responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) fn software_block_glyph_target_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> (f32, f32, f32, f32) {
    let left = (left - 0.5).ceil();
    let top = (top - 0.5).ceil();
    let right = (right - 0.5).ceil().max(left + 1.0);
    let bottom = (bottom - 0.5).ceil().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

/// Apply HarfBuzz positioning while preserving the raster tile's size.
pub(super) fn positioned_shaped_glyph_rect(
    natural: (f32, f32, f32, f32),
    x_offset: f32,
    y_offset: f32,
) -> (f32, f32, f32, f32) {
    (natural.0 + x_offset, natural.1 + y_offset, natural.2, natural.3)
}

/// Resolve a shaped glyph's horizontal offset from its cluster-local running pen.
pub(super) fn shaped_cluster_x_offset(
    prior_col: &mut Option<u16>,
    pen_x: &mut f32,
    glyph: &sonicterm_text::shape::ShapedGlyph,
) -> f32 {
    if *prior_col != Some(glyph.lead_col) {
        // A new terminal cluster anchors its pen to the lead cell instead of carrying the
        // preceding cluster's accumulated advance.
        *prior_col = Some(glyph.lead_col);
        *pen_x = 0.0;
    }
    let offset = *pen_x + glyph.x_offset;
    *pen_x += glyph.x_advance;
    offset
}

/// Reserve vertical retained-damage margin for native glyph bearings and GPOS offsets.
pub(super) fn terminal_vertical_ink_pad(
    cell_h: f32,
    metrics: Option<sonicterm_engine::CellMetricsPx>,
) -> f32 {
    metrics.map(|metrics| metrics.cell_h as f32).unwrap_or(cell_h).max(0.0).ceil()
}

/// Whether a glyph is a standalone Claude Code circle marker that projection fits inside its
/// cell: an approved codepoint in a one-cell, narrow cluster with no combining extras.
pub(crate) fn status_marker_fit_eligible(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
) -> bool {
    matches!(ch, '\u{23fa}' | '\u{25ef}' | '\u{25cf}')
        && cluster_cells == 1
        && !is_wide
        && !has_extras
}

/// Scale `natural` uniformly to fit `cell` and centre it; a non-positive size on either side has
/// no meaningful ratio, so the natural rectangle is kept.
pub(crate) fn fit_status_marker_rect(
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    let (_, _, glyph_w, glyph_h) = natural;
    let (cell_x, cell_y, cell_w, cell_h) = cell;
    // When: glyph_w, glyph_h, cell_w, or cell_h is non-positive, no meaningful
    // fit ratio exists, so preserve the natural rectangle.
    if glyph_w <= 0.0 || glyph_h <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return natural;
    }
    let scale = (cell_w / glyph_w).min(cell_h / glyph_h);
    let fitted_w = glyph_w * scale;
    let fitted_h = glyph_h * scale;
    (cell_x + (cell_w - fitted_w) * 0.5, cell_y + (cell_h - fitted_h) * 0.5, fitted_w, fitted_h)
}

/// Normalizes standalone Claude Code circle markers inside one cell without distortion: the
/// eligibility predicate and the fit composed, as shaping decides and projection applies them.
#[cfg(test)]
pub(crate) fn fit_single_cell_status_marker(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    if status_marker_fit_eligible(ch, cluster_cells, is_wide, has_extras) {
        fit_status_marker_rect(natural, cell)
    } else {
        // When: `status_marker_fit_eligible` is false, the rasterizer's geometry is kept exactly.
        natural
    }
}

/// shared device-pixel-snapped column-edge cache. Returns
/// `cols + 1` entries where slot `c` is the snapped left edge of cell
/// `c`, and slot `c + span` is its right edge. Every overlay/glyph
/// path that derives a horizontal rect from a column index must read
/// from this cache so adjacent overlays share an exact device-pixel
/// edge with the glyph cells they cover.
///
/// Raster-pixel inputs use scale 1.0 for integer device-pixel rounding, including at fractional display DPI.
#[doc(hidden)]
#[must_use]
pub fn build_snapped_cell_x(origin_x: f32, cell_w: f32, cols: u16) -> Vec<f32> {
    let mut edges = Vec::with_capacity(usize::from(cols) + 1);
    fill_snapped_cell_x(&mut edges, origin_x, cell_w, cols);
    edges
}

/// The displayed row of a quick-select hint at scrollback-absolute `hint_row` in a viewport whose
/// top is `view_top_abs` and which shows `visible_rows` rows; `None` when the row is not displayed.
/// The hint background and the hint letter both place a hint through this one mapping.
#[doc(hidden)]
#[must_use]
pub fn quick_select_viewport_row(
    hint_row: usize,
    view_top_abs: u64,
    visible_rows: usize,
) -> Option<usize> {
    let offset = (hint_row as u64).checked_sub(view_top_abs)?;
    usize::try_from(offset).ok().filter(|row| *row < visible_rows)
}

/// The displayed row the quick-select `hint` of `quick_select` draws on, once the grid has evicted
/// `scrollback_evicted` history rows in its lifetime; `None` when its own row was evicted or the row
/// is outside the viewport. The hint background and the hint letter both place a hint through it.
#[doc(hidden)]
#[must_use]
pub fn quick_select_hint_display_row(
    quick_select: &QuickSelectState,
    hint: &QuickSelectHint,
    scrollback_evicted: u64,
    view_top_abs: u64,
    visible_rows: usize,
) -> Option<usize> {
    quick_select
        .hint_row_now(hint, scrollback_evicted)
        .and_then(|row| quick_select_viewport_row(row, view_top_abs, visible_rows))
}

/// The snapped left edge of column `col`, as slot `col` of [`build_snapped_cell_x`] holds it.
pub(super) fn snapped_cell_edge(origin_x: f32, cell_w: f32, col: u16) -> f32 {
    sonicterm_render_model::geometry::snap_to_device_pixels(
        (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
        1.0,
    )
    .0
}

/// [`pixel_to_local_col`] without building the edge vector: a binary search over the same snapped
/// edges, computed only where the search looks, so a hit test allocates nothing.
#[doc(hidden)]
#[must_use]
pub fn snapped_col_at(px: f32, origin_x: f32, cell_w: f32, cols: u16) -> Option<u16> {
    if cols == 0 {
        // When: cols is zero, the pane has no addressable cell.
        return None;
    }
    let edge = |col: u16| snapped_cell_edge(origin_x, cell_w, col);
    if px < edge(0) || px >= edge(cols) {
        // When: px is left of the first edge or at or past the last, it lies in padding.
        return None;
    }
    // Edges are non-decreasing, so the cell is the last column whose left edge is at or below px;
    // a boundary px resolves to the right-hand cell, as the linear scan does.
    let (mut low, mut high) = (0_u16, cols);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if edge(middle) <= px {
            low = middle;
        } else {
            // When: middle's left edge is past px, the cell lies before middle.
            high = middle;
        }
    }
    Some(low)
}

/// [`build_snapped_cell_x`] into a reused buffer: `edges` is cleared and refilled with the same
/// `cols + 1` snapped edges, keeping its allocation across frames.
pub(crate) fn fill_snapped_cell_x(edges: &mut Vec<f32>, origin_x: f32, cell_w: f32, cols: u16) {
    edges.clear();
    edges.extend((0..=cols).map(|col| snapped_cell_edge(origin_x, cell_w, col)));
}

/// Pure column-from-pixel lookup that mirrors the renderer's
/// device-pixel-snapped edge cache. `edges` is the output of
/// `build_snapped_cell_x` for the pane in question (length `cols + 1`).
/// Returns `Some(col)` for any `px` in `[edges[0], edges[cols])` using
/// half-open buckets `edges[col] <= px < edges[col+1]` — boundary px
/// resolve to the RHS cell, matching the renderer's draw bias.
///
/// Returns `None` if `px` is left of `edges[0]` or `>= edges[cols]`
/// (caller already gated negatives via the pane resolution step, but
/// this is defensive). Returns `None` if `edges` is malformed
/// (`len < 2`) — that only happens for a 0-col pane, which has no
/// addressable cell to begin with.
#[doc(hidden)]
#[must_use]
pub fn pixel_to_local_col(px: f32, edges: &[f32], cols: u16) -> Option<u16> {
    if cols == 0 || edges.len() < 2 {
        // When: cols is zero or edges is malformed (len < 2), the pane has no addressable cell, so no pixel can resolve to a column.
        return None;
    }
    if px < edges[0] {
        // When: px sits left of edges[0], it lands in the window padding rather than the grid, so no column owns it.
        return None;
    }
    if px >= edges[cols as usize] {
        // When: px reaches edges[cols], it is past the grid's right edge in trailing padding; the half-open buckets end at that exact value.
        return None;
    }
    // Linear scan: half-open buckets edges[i] <= px < edges[i+1].
    // Cell counts are bounded (<= a few hundred) so a scan beats the
    // branch overhead of binary search at typical widths. For very wide
    // grids (cols >> 200) this could switch to `partition_point` — the
    // input is monotone non-decreasing by construction.
    for i in 0..cols as usize {
        if px < edges[i + 1] {
            // When: px falls under edges[i + 1], bucket i contains it; a boundary px resolves to the right-hand cell, matching the renderer's draw bias.
            return Some(i as u16);
        }
    }
    // Unreachable given the `>= edges[cols]` guard above, but keep the
    // total function obvious.
    None
}

#[cfg(test)]
#[path = "core_row_geometry_tests.rs"]
mod core_row_geometry_tests;
