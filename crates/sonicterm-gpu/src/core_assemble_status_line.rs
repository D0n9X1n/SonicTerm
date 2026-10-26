//! Assemble status line in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_status_line(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
    ) {
        let AssemblyGeometry { surface_width_px: sw, surface_height_px: sh, .. } = geometry;
        let AssemblyUi { theme, .. } = inputs;
        let frame_scratch::FrameScratch { glyphs: glyph_instances, quads, .. } = scratch;
        // -------- Tab bar ---------------------------------------------------
        // The insertion gap below opens 8 px at the current drop slot when a
        // drag is active over this bar.
        // -------- Status line strip ----------------------------------------
        if self.status_line_visible {
            // The strip is on: draw its band and fitted segments on its edge.
            use std::hash::{Hash, Hasher};
            let strip_palette = self.chrome_caches.palette.palette_for(theme);
            let colors = crate::status_strip::StatusColors::resolve(
                theme,
                self.tab_active_fg,
                self.tab_inactive_fg,
                &strip_palette,
            );
            let mut geometry = crate::status_strip::StripGeometry {
                top: self.status_line_top(),
                height: self.status_line_height(),
                position: self.status_line_position,
                screen: (sw, sh),
                scale_factor: self.scale_factor,
                font_raster_px: self.font_size * self.scale_factor,
                font_scale: self.status_line_font_scale,
                cap_height: 0.0,
            };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (
                self.status_line_view.pixel_hash,
                sw.to_bits(),
                geometry.text_px().to_bits(),
                self.tab_title_font.key(),
                self.tab_title_font.fallback_epoch(),
            )
                .hash(&mut hasher);
            let fit_key = hasher.finish();
            let mut strip_rasterizer = self.status_line_font_stack.clone();
            let text_px = geometry.text_px();
            // The cap height is measured once per text size and kept.
            let kept = crate::status_strip::kept_cap_height(
                self.status_line_cap_height,
                text_px,
                || match (self.status_line_font_stack.as_ref(), strip_rasterizer.as_mut()) {
                    (Some(stack), Some(rasterizer)) => crate::status_strip::strip_cap_height(
                        stack,
                        rasterizer,
                        &mut self.glyph_atlas,
                        text_px,
                    ),
                    _ => text_px * 0.7,
                },
            );
            self.status_line_cap_height = Some(kept);
            geometry.cap_height = kept.1;
            let stale = self.status_line_fit.as_ref().is_none_or(|(key, _)| *key != fit_key);
            if let (true, Some(stack), Some(rasterizer)) =
                (stale, self.status_line_font_stack.as_ref(), strip_rasterizer.as_mut())
            {
                // The view, width or font changed: refit with the chrome font.
                let fitted = crate::status_strip::fit_strip(
                    &self.status_line_view,
                    &geometry,
                    stack,
                    rasterizer,
                    &mut self.glyph_atlas,
                );
                self.status_line_fit = Some((fit_key, fitted));
            }
            let (placed, separators): (&[PlacedSegment], &[PlacedPart]) =
                self.status_line_fit.as_ref().map_or((&[], &[]), |(_, fitted)| {
                    (fitted.segments.as_slice(), fitted.separators.as_slice())
                });
            crate::status_strip::emit_strip(
                placed,
                separators,
                &geometry,
                &colors,
                self.status_line_font_stack.as_ref(),
                strip_rasterizer.as_mut(),
                &mut self.glyph_atlas,
                &mut self.chrome_caches.runs,
                self.chrome_reuse,
                &mut *quads,
                &mut *glyph_instances,
            );
            let focused_box = self.status_line_focus.and_then(|(id, strong)| {
                placed.iter().find_map(|segment| {
                    let view = self.status_line_view.segments.get(segment.index)?;
                    (view.id == id).then_some((segment.left, segment.width, strong))
                })
            });
            if let Some((left, width, strong)) = focused_box {
                // The keyboard focus ring frames the focused segment inside the band.
                let thickness =
                    crate::status_strip::focus_ring_thickness(geometry.scale_factor, strong);
                let color = crate::color::chrome_color_to_linear_rgba(colors.value);
                for [ring_x, ring_y, ring_w, ring_h] in crate::status_strip::focus_ring_rects(
                    left,
                    width,
                    geometry.top,
                    geometry.height,
                    thickness,
                ) {
                    quads.push(crate::quad::QuadInstance {
                        rect: crate::quad::px_to_ndc(
                            ring_x,
                            ring_y,
                            ring_w,
                            ring_h,
                            sw,
                            geometry.screen.1,
                        ),
                        color,
                        ..Default::default()
                    });
                }
            }
            // Staged only: the presenter's outcome decides whether this layout is ever on screen.
            let staged = placed
                .iter()
                .filter_map(|segment| {
                    let view = self.status_line_view.segments.get(segment.index)?;
                    Some(crate::status_strip::PresentedStatusSegment {
                        id: view.id,
                        x: segment.left,
                        width: segment.width,
                        click: view.click.clone(),
                    })
                })
                .collect();
            self.status_line_layouts.stage(crate::status_strip::StripHit {
                top: geometry.top,
                height: geometry.height,
                segments: staged,
            });
        }
    }
}
