//! Frame identity responsibilities of the renderer, preserving its single owner.

use super::*;

/// Drop the background-quad cache entries of every live row `planned` carries as dirty, stored at
/// absolute row `scrollback_len + row`; this cache is uncounted. The glyph cache drops nothing for
/// dirt: its rows are keyed by content, so changed content misses by itself.
pub(super) fn invalidate_planned_quad_rows(
    cache: &mut crate::row_quad_cache::LineQuadCache,
    planned: &PlannedPane,
) {
    for &row in &planned.dirty_live_rows {
        cache.invalidate_row_abs(planned.id, planned.scrollback_len + row as u64);
    }
}

/// Whether the terminal cursor is drawn for a view whose top is absolute row `view_top_abs`.
/// It is drawn only when the view sits at `live_top_abs`, the live top; any scrolled-back view
/// hides it, even one whose rows still include the cursor's live row.
pub(super) fn terminal_cursor_drawn_at_view(view_top_abs: u64, live_top_abs: u64) -> bool {
    view_top_abs == live_top_abs
}

/// The first column and width in columns of the terminal cursor's block on `grid`: a cursor on
/// either half of a wide character covers both halves.
pub(super) fn terminal_cursor_columns(grid: &Grid) -> (usize, usize) {
    let row = grid.row(grid.cursor.row);
    let mut first_col = grid.cursor.col as usize;
    let mut span = 1usize;
    if let Some(cell) = row.get(first_col) {
        if cell.flags.contains(CellFlags::WIDE_CONT) && first_col > 0 {
            first_col -= 1;
            span = 2;
        } else if cell.flags.contains(CellFlags::WIDE) {
            // When: `WIDE` — the cursor is on the lead half, so the
            // block spans two columns from where it already is.
            span = 2;
        }
    }
    (first_col, span)
}

/// The cell the terminal cursor is drawn in on `grid` (pane `pane_id`, view top `view_top_abs`),
/// by the draw's own rule: only when `cursor_visible`, `window_focused` and not `read_only`, and
/// only while the view is at the live top. The frame identity records it so the planner can
/// damage the old and new cursor rows.
pub(super) fn drawn_cursor_cell(
    grid: &Grid,
    pane_id: u64,
    view_top_abs: u64,
    cursor_visible: bool,
    window_focused: bool,
    read_only: bool,
) -> Option<CursorCell> {
    let live_top = grid.scrollback_len() as u64;
    let drawn = cursor_visible
        && window_focused
        && !read_only
        && terminal_cursor_drawn_at_view(view_top_abs, live_top);
    if !drawn {
        // When: `drawn` is false (hidden, unfocused, read-only or scrolled back), no cursor pixels exist.
        return None;
    }
    let (first_col, span) = terminal_cursor_columns(grid);
    Some(CursorCell {
        pane_id,
        slot: grid.cursor.row,
        col: u16::try_from(first_col).unwrap_or(u16::MAX),
        span: u16::try_from(span).unwrap_or(u16::MAX),
    })
}

#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(super) fn dirty_rows_damage_rect<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w, surface_h,
    )
}

/// Decide a pane's per-frame damage rectangle, given whether the pane is
/// showing the alternate screen.
///
/// A hardware surface keeps the previous frame's pixels, so damage-limited
/// repainting only redraws the rows the grid marked dirty. That is correct
/// for a normal shell pane: a changed prompt line is a narrow edit and
/// leaving the surrounding rows untouched is exactly what we want. It is
/// WRONG for an alternate-screen app (vim/nvim/less/tmux). Those apps
/// scroll, split, and repaint regions such that a row which was NOT
/// re-emitted this frame can still be visually stale — the app moved
/// content out from under it. For an alt-screen pane we therefore repaint
/// the pane's whole clipped rectangle whenever ANY row is dirty, and
/// nothing when the pane is clean.
///
/// Returns:
/// * `None` for an alt-screen pane with no dirty rows (clean -> no repaint).
/// * the full pane rectangle clipped to the surface for a dirty alt-screen
///   pane — a complete pane repaint, never an unconditional full-window one.
/// * the narrow, glyph-padded dirty-row union ([`dirty_rows_damage_rect_with_ink_pad`])
///   for a normal-screen pane.
///
/// The alt-screen decision is independent of cell metrics, so sparse /
/// scattered dirty rows and fractional cell heights all resolve to the same
/// complete-pane repaint; the surface clip both bounds the rect to on-screen
/// pixels and rejects a fully off-surface pane.
#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(super) fn pane_damage_rect<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    pane_damage_rect_with_ink_pad(
        is_alt, dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w,
        surface_h,
    )
}

#[cfg(test)]
#[path = "core_frame_identity_tests.rs"]
mod core_frame_identity_tests;
