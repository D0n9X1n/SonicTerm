//! Assemble decorations in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_decorations(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        broadcast_participant_ids: &[u64],
    ) {
        let AssemblyGeometry {
            pane_views, plan, surface_width_px: sw, surface_height_px: sh, ..
        } = geometry;
        let AssemblyUi { theme, hovered_url_cells, .. } = inputs;
        let frame_scratch::FrameScratch {
            quads,
            overlay_quads: quads_overlay,
            underlines,
            underline_owners,
            missing_tofu,
            pane_rects,
            ..
        } = scratch;
        let pane_rects = pane_rects.as_slice();
        // Underline quads — drawn last so they appear on top of the text.
        // SGR 4:n style and SGR 58 colour are stored per-cell and coalesced
        // above, matching WezTerm/xterm underline semantics instead of the
        // old single-colour single-line approximation.
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        // underlines are collected from every pane (each entry
        // carries its own `origin_x` == pane pad), so memoize a snapped
        // cache per distinct pane pad. Most frames have ≤ 2 panes, so
        // the linear-scan map is cheaper than a HashMap.
        // Step-4 revise (option (a)): each entry also carries the
        // ORIGINATING pane's column count. Previously this loop sized
        // the cache from `grid.cols` (== ACTIVE pane), which clamped
        // and truncated underlines on wider INACTIVE panes. Key the
        // cache by (pad_bits, pane_cols) and size it accordingly.
        let mut underline_caches: Vec<(u32, u16, Vec<f32>)> = Vec::new();
        for (entry, (origin_x, origin_y, pane_cols, row, run)) in underlines.iter().enumerate() {
            let pad_bits = origin_x.to_bits();
            let cache = if let Some((_, _, c)) =
                underline_caches.iter().find(|(b, pc, _)| *b == pad_bits && *pc == *pane_cols)
            {
                c
            } else {
                // When: the `find` returned None — no cache exists yet for this
                // origin and column count, so one is built and memoized.
                let c = build_snapped_cell_x(*origin_x, self.cell_w, *pane_cols);
                underline_caches.push((pad_bits, *pane_cols, c));
                &underline_caches.last().unwrap().2
            };
            let end_exclusive = (run.end_col as usize).saturating_add(1);
            let cache_end = end_exclusive.min(cache.len().saturating_sub(1));
            let col_a_usize = (run.start_col as usize).min(cache_end);
            let x = cache
                .get(col_a_usize)
                .copied()
                .unwrap_or(*origin_x + f32::from(run.start_col) * self.cell_w);
            let w = cache
                .get(cache_end)
                .map(|r| r - x)
                .unwrap_or_else(|| f32::from(run.end_col - run.start_col + 1) * self.cell_w);
            let y = *origin_y + f32::from(*row) * self.cell_h;
            let underline_color =
                chrome_color_to_linear_rgba(color_to_chrome(run.color, theme, self.fg_default));
            let quads_before = quads.len();
            push_underline_quads(
                &mut *quads,
                run.style,
                x,
                y,
                w,
                self.cell_h,
                underline_thickness,
                sw,
                sh,
                underline_color,
            );
            // Dotted and curly underlines reach below the cell, so the drawn quads join the record.
            if let Some(staged) = underline_owners.get(entry) {
                let mut ink = crate::row_ink::InkEdges::default();
                for quad in &quads[quads_before..] {
                    ink.add_px(crate::cursor::ndc_rect_px(quad.rect, sw, sh));
                }
                self.row_ink.merge_staged(*staged, ink.to_rect(plan.surface));
            }
        }

        // Hover target underline. The pane id travels with the hit so an
        // inactive split can reveal its own path without painting another
        // pane's fragments at the same rows and columns.
        if let Some(h) = hovered_url_cells {
            // When: `hovered_url_cells` contains `h`, render every canonical fragment for its owning pane.
            if let Some(hovered_view) = pane_views.iter().find(|view| view.pane_id == h.pane_id) {
                // When: `pane_views` finds `h.pane_id`, project all fragments through that pane's geometry.
                let hov_accent = if h.active {
                    self.chrome_caches.palette.palette_for(theme).accent
                } else {
                    // When: `h.active` is false, render the non-clickable hover hint in the theme's yellow rather than the action accent.
                    hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 0.9)
                };
                let hovered_grid_cols = hovered_view.grid.cols;
                let hcache =
                    build_snapped_cell_x(hovered_view.origin_x, self.cell_w, hovered_grid_cols);
                for span in h.spans() {
                    let Some((x, y, width, height)) = hovered_url_span_rect(
                        *span,
                        hovered_grid_cols,
                        hovered_view.grid.rows,
                        hovered_view.origin_x,
                        hovered_view.origin_y,
                        self.cell_w,
                        self.cell_h,
                        &hcache,
                    ) else {
                        // When: `span` has no visible coverage in `hovered_view`, emit no stale geometry.
                        continue;
                    };
                    push_underline_quads(
                        &mut *quads,
                        UnderlineStyle::Single,
                        x,
                        y,
                        width,
                        height,
                        underline_thickness,
                        sw,
                        sh,
                        hov_accent,
                    );
                }
            }
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in missing_tofu.iter() {
            let rgba = with_premultiplied_alpha(chrome_color_to_linear_rgba(*col), 0.55);
            let t = 1.0_f32; // border thickness
                             // Top
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Bottom
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y + *h - t, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Left
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Right
            quads.push(QuadInstance {
                rect: px_to_ndc(*x + *w - t, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
        }

        // -------- Pane splitters + broadcast safety chrome ------------------
        // Splitters are 1px interior seams at the shared OUTER pane boundary.
        // They are not pane borders: no window perimeter is drawn, and the
        // seam sits outside the per-pane cell padding that is applied inside
        // each pane rect by the layout caller.
        if pane_rects.len() > 1 {
            for splitter in splitter_rects_from_panes(pane_rects, 1.0) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(
                        splitter.rect.x,
                        splitter.rect.y,
                        splitter.rect.w,
                        splitter.rect.h,
                        sw,
                        sh,
                    ),
                    color: self.splitter_color,
                    ..Default::default()
                });
            }
        }

        // Safety edges sit above terminal ink, images, and the scrollbar, but below modal chrome.
        emit_broadcast_borders(
            &mut *quads_overlay,
            pane_rects,
            broadcast_participant_ids,
            hex_to_premultiplied_rgba(theme.colors.bright.red.0.as_str(), 1.0),
            sw,
            sh,
        );
    }
}
