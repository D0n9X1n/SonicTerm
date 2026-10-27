//! Assemble transients in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_transients(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
    ) {
        let AssemblyGeometry {
            grid,
            plan,
            active_origin_x,
            active_origin_y,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let AssemblyUi { theme, copy_mode, quick_select_hint_count, .. } = inputs;
        let frame_scratch::FrameScratch {
            overlay_glyphs: overlay_glyph_instances,
            overlay_quads: quads_overlay,
            ..
        } = scratch;
        // Drag-chip overlay: translucent ~120×24 quad that follows the
        // cursor while a tab is held. Drawn AFTER ime/search so it
        // sits on top of everything.
        if let Some(chip) = self.drag_chip.clone() {
            const CHIP_W: f32 = 120.0;
            const CHIP_H: f32 = 24.0;
            // Two independent multipliers compose here.
            // `chip.scale` is the tear-out ANIMATION ease (1.0 in-bar, 1.02
            // on tear); `dpi` is the display scale factor. The chip's logical
            // size + decorations must scale by DPI so it keeps a constant
            // physical size across displays. `top_left` is already in physical
            // px (cursor-relative, from the app layer) so it is NOT scaled —
            // only the size and the size-derived centering offset are.
            let dpi = self.scale_factor;
            let scale = chip.scale.clamp(0.5, 2.0) * dpi;
            let w = CHIP_W * scale;
            let h = CHIP_H * scale;
            // Re-center the scaled chip so growth is centered around
            // the original anchor point (cursor-relative offset is
            // preserved by the caller in `top_left`).
            let cx = chip.top_left.0 + CHIP_W * 0.5 * dpi;
            let cy = chip.top_left.1 + CHIP_H * 0.5 * dpi;
            let x0 = cx - w * 0.5;
            let y0 = cy - h * 0.5;

            // Soft drop shadow: stack two dimmer quads with growing
            // offset to fake an 8px blur without a fragment shader.
            // Offsets are logical px → scale by DPI.
            for (off, alpha) in [(2.0_f32, 0.18_f32), (4.0_f32, 0.10_f32), (8.0_f32, 0.05_f32)] {
                let off = off * dpi;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(x0 + off, y0 + off, w, h, sw, sh),
                    color: [0.0, 0.0, 0.0, alpha],
                    ..Default::default()
                });
            }

            // Drop-line indicator (in-bar reorder cue). Drawn BEFORE
            // the chip so the chip floats on top if they overlap.
            if let Some(lx) = chip.drop_line_x {
                let (ly0, ly1) = chip.drop_line_y;
                let lh = (ly1 - ly0).max(2.0 * dpi);
                // Drop-line accent — theme-driven (was hardcoded ACCENT_BLUE).
                let line_color = with_premultiplied_alpha(
                    self.chrome_caches.palette.palette_for(theme).accent,
                    0.95,
                );
                // 3px line centered on lx; both the half-width offset and the
                // width are logical px scaled by DPI.
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(lx - 1.5 * dpi, ly0, 3.0 * dpi, lh, sw, sh),
                    color: line_color,
                    ..Default::default()
                });
            }

            // Ghost body: alpha controlled by
            // `chip.ghost_alpha` (spec 0.5). The historical chip
            // rendered at 0.7; the spec ghost is more
            // translucent so the bar underneath stays legible.
            let chip_color = with_premultiplied_alpha(self.tab_active_bg, chip.ghost_alpha);
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x0, y0, w, h, sw, sh),
                color: chip_color,
                ..Default::default()
            });

            // Drag-chip title text → chrome_text.
            //
            // Scale the
            // text color alpha by `chip.ghost_alpha` (spec 0.5) so
            // the GHOST TITLE matches the ghost body translucency.
            if !chip.title.is_empty() {
                let ghost_fg =
                    scale_chrome_text_alpha(self.tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    // Match the legacy TextArea geometry: left = x0 + 6,
                    // top = y0 + (h - font_size*0.85*1.2) * 0.5, clip to
                    // chip body inset 4px. Font size goes through raster_px and
                    // the insets through dpi so the ghost title scales with the
                    // chip on HiDPI displays.
                    let chip_font_size = self.raster_px(self.font_size * 0.85);
                    let top = y0 + ((h - chip_font_size * 1.2).max(0.0)) * 0.5;
                    let baseline_y = top + chip_font_size * 0.8;
                    let layout = chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        &chip.title,
                        ghost_fg,
                        ChromeAttrs::default(),
                        chip_font_size,
                        native_em,
                        (x0 + 6.0 * dpi, baseline_y),
                        (sw, sh),
                        Some(ChromeClip { x: x0 + 4.0 * dpi, y: y0, w: w - 8.0 * dpi, h }),
                    );
                    overlay_glyph_instances.extend(layout.glyphs);
                    quads_overlay.extend(layout.missing_boxes);
                }
            }
            self.drag_chip_visual = Some(DragChipVisual { top_left: (x0, y0), size: (w, h) });
        } else {
            // When: `self.drag_chip` is None — no tab is being dragged, so the
            // recorded visual is cleared rather than left stale for tests.
            self.drag_chip_visual = None;
        }

        // Glyphon `Resolution` / `TextArea` / `TextBounds` /
        // `text_renderer.prepare` are gone. Every chrome string already
        // landed in `glyph_instances` (pre-overlay: search status bar,
        // tab titles) or `overlay_glyph_instances` (modal chrome:
        // palette, IME preedit, drag-chip title, quick-select hints) via `chrome_text::layout`
        // earlier in this function. The atlas upload + per-pass draw
        // calls below carry those instances to the GPU.

        // Quick-select hint overlay → chrome_text into the overlay
        // glyph instance vec. Each hint is anchored at its (row, col)
        // cell origin so the hint character sits exactly inside the
        // chosen cell. The hint set is sparse, so emitting each hint through chrome_text avoids
        // materializing a full padded hint string.
        if quick_select_hint_count > 0 {
            // When: quick_select_hint_count is positive, each hint's letter is laid out on its cell.
            if let Some(qs) = copy_mode.and_then(|cm| cm.quick_select.as_ref()) {
                // When: copy_mode carries quick_select, its hints supply the letters to draw.
                let bg_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                if let Some(stack) = self.font_stack.as_ref() {
                    // When: font_stack exists, chrome_text can shape each hint letter.
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    for hint in &qs.hints {
                        let Some(visible_row) = quick_select_hint_display_row(
                            qs,
                            hint,
                            grid.scrollback_evicted(),
                            plan.active_view_top_abs,
                            grid.rows as usize,
                        ) else {
                            // When: quick_select_hint_display_row finds the hint evicted or off the displayed rows, its letter is not drawn.
                            continue;
                        };
                        let x = active_origin_x + hint.col_start as f32 * self.cell_w;
                        let y = active_origin_y + visible_row as f32 * self.cell_h;
                        let s = hint.hint.to_string();
                        let l = chrome_text::layout(
                            stack,
                            &mut wt,
                            &mut self.glyph_atlas,
                            &s,
                            bg_color,
                            ChromeAttrs::default(),
                            self.font_size,
                            native_em,
                            (x, y + self.font_size * 0.8),
                            (sw, sh),
                            None,
                        );
                        overlay_glyph_instances.extend(l.glyphs);
                        quads_overlay.extend(l.missing_boxes);
                    }
                }
            }
        }
    }
}
