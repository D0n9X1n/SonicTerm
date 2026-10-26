//! Assemble glyph rows in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_glyph_rows(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        missing_chars_this_frame: &mut Vec<char>,
    ) {
        let AssemblyGeometry {
            fg_default,
            pane_views,
            plan,
            cell_width_px: cell_w,
            cell_height_px: cell_h,
            baseline_y_in_cell,
            raster_px,
            software_presenter,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let AssemblyUi { theme, hovered_url_cells, .. } = inputs;
        let frame_scratch::FrameScratch {
            glyphs: glyph_instances,
            row_spans,
            underlines,
            underline_owners,
            staged_ranges,
            missing_tofu,
            snapped,
            snapped_peak,
            row_keys,
            ..
        } = scratch;
        {
            // Post-glyphon the grid path is wezterm-only.
            // FontStack is the sole rasterizer; on test fixtures
            // without bundled fonts (FontStack returns None) the grid
            // walk skips per-glyph emission and only paints quads.
            let mut wt_raster = self.font_stack.clone();
            // Theme accent for the Cmd-hovered URL recolor. `UiPalette::accent`
            // is a linear-sRGB `[f32;4]` (alpha 1.0), the same space the
            // per-glyph `color` field carries, so it drops in with no
            // conversion. PERF: `UiPalette::from_theme` does ~20 hex parses +
            // sRGB→linear `powf` conversions; computing it unconditionally
            // every frame added measurable render latency to plain output
            // repaints (e.g. `ls -al`). It's only consumed when a URL is
            // ACTIVE-hovered (modifier held) and glyphs recolor to accent;
            // plain hover draws only a yellow underline, so compute lazily —
            // `[0.0;4]` otherwise. #perf
            let hovered_url_accent: [f32; 4] = if hovered_url_needs_accent(hovered_url_cells) {
                self.chrome_caches.palette.palette_for(theme).accent
            } else {
                // When: `!hovered_url_needs_accent` — plain hover draws only
                // the underline, so the accent is never sampled.
                [0.0, 0.0, 0.0, 0.0]
            };
            // One cache pass and one fresh ink stage per assembly pass, before any pane pins or
            // admits a row; the test fixture starts its passes through the same helper.
            begin_glyph_pass(
                &mut self.row_glyph_cache,
                &mut self.row_ink,
                pane_views.iter().map(|pane| (pane.planned, pane.grid.cols)),
            );
            if let Some(probe) = self.emitted_rows_probe.as_mut() {
                // A test enabled the inspector: this pass records only what it emits itself.
                probe.clear();
            }
            for (pane_index, pv) in
                pane_views.iter().enumerate().filter(|(_, pane)| pane.planned.full_clip.is_some())
            {
                let pane_staged_start = self.row_ink.staged_len();
                // The inspector's record of this pane's emitted slots; `None` in production.
                let mut pane_emitted = self.emitted_rows_probe.as_ref().map(|_| Vec::new());
                // Per-cell device-pixel snapping rounds each cell's left edge independently, which
                // at fractional DPI alternates the cell pitch; every glyph path derives its cell
                // edges from these shared snapped edges, so adjacent cells share an edge.
                // Each pane's glyph edges reuse slot `2 * pane_index` of the frame scratch.
                let edge_slot = 2 * pane_index;
                frame_scratch::fill_snapped_slot(
                    snapped,
                    snapped_peak,
                    edge_slot,
                    pv.origin_x,
                    cell_w,
                    pv.grid.cols,
                );
                let snapped_cell_x: &[f32] = &snapped[edge_slot];
                // Hover recolor applies only to the pane named by the hit-test, so a split at the
                // same row and columns never inherits another pane's accent.
                let pane_hovered_url =
                    hovered_url_cells.filter(|hovered| hovered.pane_id == pv.pane_id);
                assemble_pane_glyph_rows(
                    GlyphShaping {
                        atlas: &mut self.glyph_atlas,
                        row_cache: &mut self.row_glyph_cache,
                        font_stack: self.font_stack.as_ref(),
                        wt_raster: wt_raster.as_mut(),
                        style_rev: self.style_rev,
                        theme,
                        fg_default,
                        raster_px,
                        cell_size: (cell_w, cell_h),
                        surface: (sw, sh),
                        baseline_y_in_cell,
                        hovered_url_accent,
                        software_presenter,
                    },
                    PaneGlyphRows {
                        pane_id: pv.pane_id,
                        grid: pv.grid,
                        planned: pv.planned,
                        origin: (pv.origin_x, pv.origin_y),
                        snapped_cell_x,
                        pane_hovered_url,
                    },
                    GlyphFrame {
                        glyph_instances: &mut *glyph_instances,
                        underlines: &mut *underlines,
                        missing_tofu: &mut *missing_tofu,
                        missing_chars_this_frame: &mut *missing_chars_this_frame,
                        row_spans: &mut *row_spans,
                    },
                    PaneGlyphSinks {
                        row_ink: &mut self.row_ink,
                        ink_surface: plan.surface,
                        underline_owners: &mut *underline_owners,
                        injected_row_glyph: self.injected_row_glyph,
                        emitted_slots: pane_emitted.as_mut(),
                        row_keys: &mut *row_keys,
                        missing_rows: Some(&mut self.staged_missing),
                        fonts: self.applied_fonts,
                    },
                );
                staged_ranges.push((pv.pane_id, pane_staged_start..self.row_ink.staged_len()));
                if let (Some(probe), Some(slots)) = (self.emitted_rows_probe.as_mut(), pane_emitted)
                {
                    // The inspector is on: keep the slots this pane's rows were emitted at.
                    probe.push((pv.pane_id, slots));
                }
            } // end per-pane loop
        }
    }
}
