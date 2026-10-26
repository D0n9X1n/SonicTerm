//! Assemble backgrounds in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_backgrounds(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        now: Instant,
    ) {
        let AssemblyGeometry {
            pane_views,
            plan,
            cell_width_px: cell_w,
            cell_height_px: cell_h,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let AssemblyUi { theme, selection, .. } = inputs;
        let frame_scratch::FrameScratch {
            glyphs: glyph_instances,
            quads,
            overlay_quads: quads_overlay,
            staged_ranges,
            snapped,
            snapped_peak,
            ..
        } = scratch;
        // Per-cell ANSI background colors. Must be pushed FIRST so that
        // selection / cursor / overlay quads draw on top — otherwise an
        // ANSI-colored cell would obscure the selection highlight. The
        // helper run-length coalesces adjacent same-bg cells into a single
        // wide quad (an 80-col `\033[41m` fill becomes 1 quad, not 80).
        // Cells whose bg resolves to the theme default are skipped: the
        // attachment clear or partial replacement reset already covers that area.
        // Background quads use each pane's own origin, including inactive panes.
        //
        // P2: per-row LineQuadCache. Background quads are a
        // hot QuadInstance source in dense-cell workloads. Each row's
        // emission is keyed on (pane_id,
        // abs_row, content+geom+style+selection hash); on a hit we
        // `extend_from_slice` the cached slice and skip the per-cell
        // run-length-encode walk in `emit_cell_bg_quads_for_row`.
        let sel_bbox_for_quads: Option<(u64, u16, u64, u16)> = selection.map(|s| {
            let (a, b) = s.normalized();
            (a.0, a.1, b.0, b.1)
        });
        let total_visible_rows: u16 = pane_views.iter().map(|pv| pv.grid.rows).sum();
        self.line_quad_cache.resize(total_visible_rows.max(1));
        for (pane_index, pv) in
            pane_views.iter().enumerate().filter(|(_, pane)| pane.planned.full_clip.is_some())
        {
            let pv_grid: &Grid = pv.grid;
            let pane_id: crate::row_quad_cache::PaneId = pv.pane_id;
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let view_top_abs_bg = pv.planned.view_top_abs;
            let pane_staged = staged_ranges
                .iter()
                .find(|(staged_pane, _)| *staged_pane == pane_id)
                .map(|(_, range)| range.clone());
            // Mirror RowGlyphCache's dirty-row invalidation: drop the absolute
            // rows of every live row the VT thread mutated since the last frame.
            invalidate_planned_quad_rows(&mut self.line_quad_cache, pv.planned);
            let pad_bg = pane_rect.x;
            let top_inset_bg = pane_rect.y;
            let max_cols = pv.planned.background_cols;
            let max_rows = pv.planned.background_rows;
            if max_cols == 0 || max_rows == 0 {
                // When: `max_cols == 0 || max_rows == 0` — the pane rect is
                // thinner than one cell, so no background quad would fit.
                continue;
            }
            // per-pane snapped-edge cache for bg-fill runs. Per
            // diagnosis Recommendation, per-pane bg must NOT reuse the
            // active pane's cache because each split-pane has its own
            // pad and the snapped column edges differ.
            // Each pane's background edges reuse slot `2 * pane_index + 1` of the frame scratch.
            let bg_edge_slot = 2 * pane_index + 1;
            frame_scratch::fill_snapped_slot(
                snapped,
                snapped_peak,
                bg_edge_slot,
                pad_bg,
                cell_w,
                pv_grid.cols,
            );
            let snapped_cell_x_bg: &[f32] = &snapped[bg_edge_slot];
            for (r, row_abs) in pv.planned.rows().take(max_rows as usize) {
                if !pv.planned.emit_rows[usize::from(r)] {
                    // When: the plan does not emit slot `r`, its retained background stays.
                    continue;
                }
                let Some(row_cells) = pv_grid.row_at_abs(row_abs) else {
                    // When: `pv_grid.row_at_abs(row_abs)` is None — the row is
                    // outside the scrollback this pane still retains.
                    continue;
                };
                let quads_before = quads.len();
                let geometry = RowBackgroundGeometry {
                    origin: (pad_bg, top_inset_bg),
                    pane_size: (pane_rect.w, pane_rect.h),
                    cell_size: (cell_w, cell_h),
                    surface: (sw, sh),
                    max_cols,
                };
                let _replayed = emit_row_background(
                    &mut self.line_quad_cache,
                    RowBackgroundRow {
                        pane_id,
                        grid: pv_grid,
                        view_top_abs: view_top_abs_bg,
                        slot: r,
                    },
                    row_cells.iter(),
                    (self.style_rev, theme, sel_bbox_for_quads),
                    &geometry,
                    snapped_cell_x_bg,
                    &mut *quads,
                );
                let mut ink = crate::row_ink::InkEdges::default();
                for quad in &quads[quads_before..] {
                    ink.add_px(crate::cursor::ndc_rect_px(quad.rect, sw, sh));
                }
                let rect = ink.to_rect(plan.surface);
                match pane_staged.clone().and_then(|range| self.row_ink.staged_index(range, r)) {
                    Some(staged) => self.row_ink.merge_staged(staged, rect),
                    None => {
                        // When: `staged_index` finds no glyph-loop record for `r`, stage one here
                        // so the row's background ink is never dropped.
                        let _ = self.row_ink.stage_row(pane_id, r, pv_grid, view_top_abs_bg, rect);
                    }
                }
            }
        }

        if let Some((flash_pane_id, flash_alpha)) = self.pane_focus_flash_alpha(now) {
            if let Some(pv) = pane_views.iter().find(|pv| pv.pane_id == flash_pane_id) {
                let chrome = pv.planned.chrome;
                quads.push(focus_flash_quad(
                    self.bg_rgba,
                    (chrome.x, chrome.y, chrome.w, chrome.h),
                    flash_alpha,
                    (sw, sh),
                ));
            }
        }

        // Per-pane scrollbar emit. Runs after row backgrounds and before
        // selection, cursor, and modal overlays. Auto opacity comes from the
        // app state machine; geometry remains shared with hit-testing.
        for pv in pane_views.iter().filter(|pane| pane.planned.full_clip.is_some()) {
            let pane_rect = pv.planned.chrome;
            let viewport_rows = pv.planned.row_count;
            let total_rows = pv.planned.scrollback_len + u64::from(viewport_rows);
            let view_top = pv.planned.view_top_abs;
            emit_pane_scrollbar(
                &mut *quads_overlay,
                pane_rect,
                viewport_rows,
                total_rows,
                view_top,
                self.scrollbar_mode,
                theme,
                sw,
                sh,
                pv.scrollbar_alpha,
                self.scale_factor,
            );
        }

        if self.injected_test_glyph.is_some() {
            // An injected test glyph joins the terminal glyphs before any recolor reads them.
            self.push_injected_test_glyph(&mut *glyph_instances, sw, sh);
        }
    }
}
