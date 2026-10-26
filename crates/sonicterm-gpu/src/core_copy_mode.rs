//! Copy mode responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Translate a scrollback-absolute row into the row index visible in the
    /// current viewport. Returns `None` when the row lies above or below the
    /// rendered viewport.
    #[doc(hidden)]
    pub fn viewport_relative_row(
        absolute_row: usize,
        view_top_abs: u64,
        visible_rows: u16,
    ) -> Option<u16> {
        let visible_row = absolute_row as i128 - i128::from(view_top_abs);
        (0..i128::from(visible_rows)).contains(&visible_row).then_some(visible_row as u16)
    }
}

impl GpuRenderer {
    /// Resolve the viewport top used by the renderer after clamping explicit
    /// scrollback requests to the live bottom.
    ///
    /// The clamp only bounds the index; it cannot tell which row an index
    /// named before history evicted rows. Callers pass a projection the app
    /// has already rebased for eviction.
    #[doc(hidden)]
    pub fn resolved_view_top_abs(grid: &Grid, viewport_top_abs: Option<u64>) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }
}

impl GpuRenderer {
    /// Compatibility name for the same Grid clamp; callers must already have rebased history eviction.
    #[doc(hidden)]
    pub fn resolved_view_top_abs_legacy(
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> u64 {
        Self::resolved_view_top_abs(grid, viewport_top_abs)
    }
}

impl GpuRenderer {
    /// Adjust a viewport after copy-mode movement so the scrollback-absolute
    /// copy-mode cursor remains visible.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move(
        copy_mode: &CopyModeState,
        grid: &Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already inside the viewport — return the
            // caller's `viewport_top_abs` so an explicit scroll survives.
            viewport_top_abs
        }
    }
}

impl GpuRenderer {
    /// Legacy-Grid variant. See `resolved_view_top_abs_legacy`.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move_legacy(
        copy_mode: &CopyModeState,
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already visible — return the caller's
            // `viewport_top_abs` so an explicit scroll position survives.
            viewport_top_abs
        }
    }
}

impl GpuRenderer {
    /// Emit copy-mode selection and cursor quads using scrollback-absolute
    /// copy-mode coordinates translated into viewport-relative rows.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn emit_copy_mode_quads(
        copy_mode: &CopyModeState,
        grid: &Grid,
        view_top_abs: u64,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        sw: f32,
        sh: f32,
        selection_color: [f32; 4],
        cursor_color: [f32; 4],
        quads: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) -> Option<(f32, f32)> {
        // derive selection-row x/w and copy-cursor cx from the
        // shared snapped-edge cache so copy-mode overlays share
        // device-pixel edges with adjacent glyph cells at fractional
        // DPI. Empty-cache fallback preserves the raw arithmetic for
        // callers (debug/test helpers) that don't carry a real cache;
        // integer scales make the two identical via the identity fast
        // path in `snap_to_device_pixels`.
        let raw_fallback = snapped_cell_x.is_empty();
        if let Some((start, end)) = copy_mode.selected_range() {
            // When: `copy_mode.selected_range()` is Some — copy mode has an
            // anchored selection. The cursor quad below is emitted regardless.
            for row_abs in start.1..=end.1 {
                let Some(visible_row) =
                    Self::viewport_relative_row(row_abs, view_top_abs, grid.rows)
                else {
                    // When: `viewport_relative_row` is None — a selection may
                    // span scrollback, so off-screen rows are skipped.
                    continue;
                };
                let col_a = if row_abs == start.1 {
                    start.0
                } else {
                    // When: `row_abs != start.1` — an interior or final row,
                    // which starts at column 0, not the selection anchor.
                    0
                }
                .min(grid.cols as usize);
                let col_b = if row_abs == end.1 {
                    end.0.min(grid.cols.saturating_sub(1) as usize)
                } else {
                    // When: `row_abs != end.1` — an interior row of a
                    // multi-row selection, which runs to the right edge.
                    grid.cols.saturating_sub(1) as usize
                };
                if col_b < col_a {
                    // When: `col_b < col_a` — an empty span on this row, which
                    // would still push a zero-width quad into the buffer.
                    continue;
                }
                let end_exclusive = col_b + 1;
                let (x, w) = if raw_fallback {
                    (origin_x + col_a as f32 * cell_w, (end_exclusive - col_a) as f32 * cell_w)
                } else {
                    // When: `!raw_fallback` — a real snapped-edge cache, so the
                    // quad takes device-pixel edges shared with glyph cells.
                    let cache_end = end_exclusive.min(snapped_cell_x.len() - 1);
                    let lo = snapped_cell_x[col_a];
                    let hi = snapped_cell_x[cache_end];
                    (lo, hi - lo)
                };
                let y = origin_y + f32::from(visible_row) * cell_h;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, cell_h, sw, sh),
                    color: selection_color,
                    ..Default::default()
                });
            }
        }

        if copy_mode.is_read_only() {
            // When: `copy_mode.is_read_only()` — a read-only view has no
            // editable cursor to draw, so only the selection quads above stand.
            return None;
        }

        let visible_row = Self::viewport_relative_row(copy_mode.cursor.1, view_top_abs, grid.rows)?;
        let copy_col = copy_mode.cursor.0.min(grid.cols.saturating_sub(1) as usize);
        let (cx, cw) = if raw_fallback {
            (origin_x + copy_col as f32 * cell_w, cell_w)
        } else {
            // When: `!raw_fallback` — a real snapped-edge cache, so the cursor
            // takes the same edges as the glyph cell beneath it.
            let lo = snapped_cell_x[copy_col];
            let hi = snapped_cell_x[(copy_col + 1).min(snapped_cell_x.len() - 1)];
            (lo, hi - lo)
        };
        let cy = origin_y + f32::from(visible_row) * cell_h;
        quads.push(QuadInstance {
            rect: px_to_ndc(cx, cy, cw, cell_h, sw, sh),
            color: cursor_color,
            ..Default::default()
        });
        Some((cx, cy))
    }
}

impl GpuRenderer {
    /// Fix 1 test hook: number of panes the most recent
    /// `render()` call received in its slice. The integration test
    /// asserts this equals the active tab's pane count so a regression
    /// to a single-element slice (the original bug) is caught
    /// mechanically. Production code must not depend on this.
    #[doc(hidden)]
    pub fn last_panes_received(&self) -> usize {
        self.last_emit_origins.len()
    }
}

#[cfg(test)]
#[path = "core_copy_mode_tests.rs"]
mod core_copy_mode_tests;
