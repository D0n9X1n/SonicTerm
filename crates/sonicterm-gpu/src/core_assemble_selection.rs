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
        active_rows: std::ops::Range<usize>,
        frame_recolor: &mut crate::cursor::RecolorRecord,
    ) {
        let AssemblyGeometry {
            grid,
            plan,
            pane_views,
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
        // The active pane's colors: the theme under its program's colors, read once for the
        // selection, copy mode and cursor; a missing view draws with the theme alone.
        let pane_colors = PaneColors::new(
            theme,
            pane_views.get(plan.active_index).and_then(|view| view.overrides),
        );
        // Selection highlights are anchored to the active pane's
        // origin. They MUST be clipped to that pane's rect — otherwise
        // a selection that extends past the pane's last visible column
        // (e.g. the user drags across the split into the neighbouring
        // pane) would emit a quad that visually bleeds into the
        // neighbouring pane's grid area. Regression-guard for the
        // bug where dragging in a split-right layout painted the
        // selection across both panes.
        // Selection rows are scrollback-ABSOLUTE; the active pane's view top maps them back
        // to viewport rows, so the highlight follows the TEXT when scrolled.
        let geometry = SelectionGeometry {
            view_top_abs: plan.active_view_top_abs,
            grid_size: (grid.rows, grid.cols),
            origin: (active_origin_x, active_origin_y),
            cell_size: (self.cell_w, self.cell_h),
            // Pane rect_px is the source of truth — see note above.
            clip: (active_pane_x, active_pane_y, active_pane_w, active_pane_h),
            surface: (sw, sh),
        };
        // A selection recolors only the active pane's glyph rows (`active_rows` of `row_spans`),
        // the pane it belongs to; a glyph it cuts moves every row span by its parts.
        if let Some(sel) = selection {
            if !sel.is_empty() {
                push_selection_quads(
                    &mut *quads,
                    sel,
                    &geometry,
                    active_snapped_cell_x,
                    pane_colors.selection_rgba(self.selection_color),
                );
                if let Some(selected) = pane_colors.selection_foreground_rgba() {
                    // Selected glyphs take the program's selection foreground; the cursor recolors
                    // below run later, so cursor text wins on the cursor cell.
                    recolor_selected_glyphs(
                        &mut *glyph_instances,
                        &mut *row_spans,
                        active_rows.clone(),
                        sel,
                        &geometry,
                        active_snapped_cell_x,
                        selected,
                    );
                }
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
            let copy_cursor = Self::emit_copy_mode_quads(
                copy_mode,
                grid,
                view_top_abs,
                active_origin_x,
                active_origin_y,
                self.cell_w,
                self.cell_h,
                sw,
                sh,
                pane_colors.selection_rgba(self.selection_color),
                self.cursor_color,
                &mut *quads,
                active_snapped_cell_x,
            );
            if let Some(selected) = pane_colors.selection_foreground_rgba() {
                // Copy mode's selected range takes the program's selection foreground by the
                // mouse selection's rule, before its cursor recolors so cursor text wins there.
                recolor_copy_mode_glyphs(
                    &mut *glyph_instances,
                    &mut *row_spans,
                    active_rows.clone(),
                    copy_mode,
                    grid,
                    &geometry,
                    active_snapped_cell_x,
                    selected,
                );
            }
            if let Some((cx, cy)) = copy_cursor {
                let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                    &mut *glyph_instances,
                    row_spans,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    pane_colors.cursor_text_rgba(self.cursor_text_color),
                    true,
                );
                crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                *frame_recolor = frame_recolor.merge(record);
            }
        }
        // An off blink phase hides only the terminal cursor's pixels; copy mode's cursor above
        // and the frame's cursor cell (the IME anchor) are unaffected.
        if cursor_visible && self.window_focused && !read_only_mode && !self.cursor_blink_off {
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
                let mut cy = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                let mut cw = cw;
                // A gliding cursor draws at its sampled position, in the pane's cell units; at the
                // glide's end the sample is `None` and the snapped cell edges above apply exactly.
                let gliding = self.cursor_glide_frame;
                if let Some(at) = gliding {
                    cx = active_origin_x + at.col_cells() * self.cell_w;
                    cy = active_origin_y + at.row_cells() * self.cell_h;
                    cw = at.span_cells() * self.cell_w;
                }
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
                // The terminal cursor takes the active pane's OSC 12 color when its program set one.
                let pane_cursor = pane_colors.cursor_rgba(self.cursor_color);
                let color = active_cursor_color(pane_cursor);
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
                match self.frame_cursor_shape {
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
                        // Mid-glide the block passes over text without recolouring it; the frame
                        // that lands on the cell recolours its glyph as a resting block does. Only
                        // the cell's columns are recoloured, at the glyph's full height, so a
                        // ligature's other half keeps its color and ink above the row repaints.
                        if gliding.is_none() {
                            let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                                &mut *glyph_instances,
                                row_spans,
                                cx,
                                cy,
                                cw,
                                self.cell_h,
                                sw,
                                sh,
                                pane_colors.cursor_text_rgba(self.cursor_text_color),
                                true,
                            );
                            crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                            *frame_recolor = frame_recolor.merge(record);
                        }
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
