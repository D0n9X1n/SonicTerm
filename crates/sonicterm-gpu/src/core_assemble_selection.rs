//! Assemble selection in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_selection(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        active_snapped_cell_x: &[f32],
        frame_recolor: &mut crate::cursor::RecolorRecord,
    ) {
        let AssemblyGeometry {
            grid,
            plan,
            active_origin_x,
            active_origin_y,
            active_pane_x,
            active_pane_y,
            active_pane_width_px: active_pane_w,
            active_pane_height_px: active_pane_h,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let AssemblyUi {
            theme,
            cursor_visible,
            selection,
            copy_mode,
            search,
            ime,
            read_only_mode,
            ..
        } = inputs;
        let frame_scratch::FrameScratch {
            glyphs: glyph_instances,
            quads,
            overlay_quads: quads_overlay,
            row_spans,
            ..
        } = scratch;
        if let Some(sel) = selection {
            if !sel.is_empty() {
                // Selection highlights are anchored to the active pane's
                // origin. They MUST be clipped to that pane's rect — otherwise
                // a selection that extends past the pane's last visible column
                // (e.g. the user drags across the split into the neighbouring
                // pane) would emit a quad that visually bleeds into the
                // neighbouring pane's grid area. Regression-guard for the
                // bug where dragging in a split-right layout painted the
                // selection across both panes.
                let pane_x = active_origin_x;
                let pane_y = active_origin_y;
                // Pane rect_px is the source of truth — see note above.
                let pane_w = active_pane_w;
                let pane_h = active_pane_h;
                // Selection rows are scrollback-ABSOLUTE; resolve the active
                // pane's view top so `selection_quad_rects` can map them back
                // to viewport rows (so the highlight follows the TEXT when
                // scrolled).
                let sel_view_top_abs = plan.active_view_top_abs;
                push_selection_quads(
                    &mut *quads,
                    sel,
                    &SelectionGeometry {
                        view_top_abs: sel_view_top_abs,
                        grid_size: (grid.rows, grid.cols),
                        origin: (active_origin_x, active_origin_y),
                        cell_size: (self.cell_w, self.cell_h),
                        clip: (pane_x, pane_y, pane_w, pane_h),
                        surface: (sw, sh),
                    },
                    active_snapped_cell_x,
                    self.selection_color,
                );
            }
        }

        if let Some(copy_mode) = copy_mode {
            if let Some(quick_select) = copy_mode.quick_select.as_ref() {
                self.prepare_quick_select_overlay(
                    quick_select,
                    active_origin_x,
                    active_origin_y,
                    plan.active_view_top_abs,
                    grid.rows as usize,
                    grid.scrollback_evicted(),
                    theme,
                    sw,
                    sh,
                    &mut *quads_overlay,
                    active_snapped_cell_x,
                );
            }
            let view_top_abs = plan.active_view_top_abs;
            if let Some((cx, cy)) = Self::emit_copy_mode_quads(
                copy_mode,
                grid,
                view_top_abs,
                active_origin_x,
                active_origin_y,
                self.cell_w,
                self.cell_h,
                sw,
                sh,
                self.selection_color,
                self.cursor_color,
                &mut *quads,
                active_snapped_cell_x,
            ) {
                let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                    &mut *glyph_instances,
                    row_spans,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    self.cursor_text_color,
                );
                crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                *frame_recolor = frame_recolor.merge(record);
            }
        }
        if cursor_visible && self.window_focused && !read_only_mode {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = plan.active_view_top_abs;
            if terminal_cursor_drawn_at_view(view_top, live_top) {
                // read both cursor cell left edge AND width from the
                // shared snapped-edge cache so the cursor (block / bar /
                // underline) lines up with its glyph cell at fractional DPI.
                // The same columns `drawn_cursor_cell` records in the frame identity.
                let (cur_col, cursor_span) = terminal_cursor_columns(grid);
                let cur_col_clamped = cur_col.min(active_snapped_cell_x.len().saturating_sub(2));
                let end_col = (cur_col_clamped + cursor_span)
                    .min(active_snapped_cell_x.len().saturating_sub(1));
                let mut cx = active_snapped_cell_x
                    .get(cur_col_clamped)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let cw = active_snapped_cell_x
                    .get(end_col)
                    .map(|r| r - cx)
                    .unwrap_or(self.cell_w * cursor_span as f32);
                let cy = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                // Visible preedit moves the terminal cursor to its caret only when search does not own composition.
                if search.is_none() {
                    if let Some(i) = ime {
                        let text = i.preedit();
                        // gate on visible ink (NOT just non-empty) and
                        // reuse the shared pure helper, so a whitespace-only
                        // macOS marked string never shoves the cursor block
                        // into empty space with no glyph under it.
                        let caret_byte = i.cursor().map(|(_, e)| e).unwrap_or(text.len());
                        let font_size = self.raster_px(self.font_size);
                        cx += preedit_caret_advance(text, caret_byte, font_size);
                    }
                }
                // Keep every cursor shape at its exact theme color. Alpha
                // fading over the terminal background makes Gruvbox yellow
                // read as olive/green during real redraws.
                let color = active_cursor_color(self.cursor_color);
                // Wezterm cursor shapes:
                //   Block     → full-cell quad, glyph re-rendered in bg
                //   Bar       → 2px vertical bar pinned to the left edge
                //   Underline → 2px horizontal bar pinned to the bottom
                // We pick a ~2px sub-cell thickness rather than something
                // proportional to cell_h so the bar stays crisp on both
                // small and large font sizes (no half-pixel sub-stem).
                // 2 logical px scaled to physical px so the bar/underline
                // keep a constant physical thickness across DPIs (min 1px).
                let subshape_px: f32 = (2.0 * self.scale_factor).round().max(1.0);
                match self.cursor_shape {
                    CursorShape::Block => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, cw, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                        let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                            &mut *glyph_instances,
                            row_spans,
                            cx,
                            cy,
                            cw,
                            self.cell_h,
                            sw,
                            sh,
                            self.cursor_text_color,
                        );
                        crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                        *frame_recolor = frame_recolor.merge(record);
                    }
                    CursorShape::Bar => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, subshape_px, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                    CursorShape::Underline => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy + self.cell_h - subshape_px, cw, subshape_px),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }
}
