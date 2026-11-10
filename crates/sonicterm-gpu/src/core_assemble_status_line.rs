//! Assemble the status line, the integrated titlebar band and its caption buttons in the existing
//! painter order, with no device work.

use super::*;

/// The small side text when its stack, rasterizer and size are all present; otherwise small
/// segments are drawn at the strip's own size.
fn small_text<'stack, Raster>(
    font_stack: Option<&'stack sonicterm_engine::FontStack>,
    rasterizer: Option<&'stack mut Raster>,
    text_px: Option<f32>,
) -> Option<crate::status_strip::SmallText<'stack, Raster>> {
    Some(crate::status_strip::SmallText {
        font_stack: font_stack?,
        rasterizer: rasterizer?,
        text_px: text_px?,
    })
}

/// Which top-level chrome surface a pass draws.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The status strip, merged into the titlebar band when it is at the top of an integrated
    /// window.
    Strip,
    /// The name-only band of an integrated window whose strip is off or at the bottom.
    Band,
}

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
        let layout = self.band_layout();
        if layout.separate_px > 0.0 {
            // An integrated window without a top strip: its name band sits above the panes.
            self.assemble_surface(Surface::Band, theme, (sw, sh), scratch);
        }
        if self.status_line_visible {
            // The strip is on: draw its band and fitted segments on its edge.
            self.assemble_surface(Surface::Strip, theme, (sw, sh), scratch);
        }
        let band_px = if layout.merged { layout.strip_px } else { layout.separate_px };
        if band_px > 0.0 {
            // The caption buttons draw over whichever surface is the band.
            let palette = self.chrome_caches.palette.palette_for(theme);
            let band_name = crate::status_strip::StatusColors::resolve(
                theme,
                self.tab_active_fg,
                self.tab_inactive_fg,
                &palette,
            )
            .band_name;
            // Neutral like Windows 11's: text-colored glyphs, and a press as faint as a hover.
            let colors = crate::caption_buttons::CaptionColors {
                glyph: palette.text_primary,
                hover: palette.bg_hover,
                pressed: palette.bg_hover,
                quiet: crate::color::chrome_color_to_linear_rgba(band_name),
            };
            let buttons = sonicterm_render_model::boundary::ui::window_band::caption_buttons(
                &self.window_band,
                sw,
                band_px,
                self.scale_factor,
            );
            for (button, rect) in buttons {
                let fills = crate::caption_buttons::caption_fills(
                    button,
                    rect,
                    &self.caption_state,
                    &colors,
                    self.scale_factor,
                );
                crate::caption_buttons::push_fills(&fills, (sw, sh), &mut scratch.quads);
            }
        }
    }

    /// Draw one surface: its band, hairline, fitted segments and focus ring, and stage its hit
    /// boxes for the presenter's outcome.
    fn assemble_surface(
        &mut self,
        surface: Surface,
        theme: &sonicterm_render_model::boundary::cfg::theme::Theme,
        (sw, sh): (f32, f32),
        scratch: &mut frame_scratch::FrameScratch,
    ) {
        use std::hash::{Hash, Hasher};
        let frame_scratch::FrameScratch { glyphs: glyph_instances, quads, .. } = scratch;
        let layout = self.band_layout();
        let strip_palette = self.chrome_caches.palette.palette_for(theme);
        let colors = crate::status_strip::StatusColors::resolve(
            theme,
            self.tab_active_fg,
            self.tab_inactive_fg,
            &strip_palette,
        );
        let in_band = match surface {
            Surface::Strip => layout.merged,
            Surface::Band => true,
        };
        let colors =
            colors.for_band_focus(self.window_band.buttons, in_band, self.caption_state.focused);
        let band = if in_band {
            let scale = self.scale_factor;
            crate::status_strip::StripBand {
                in_band: true,
                left_inset: (self.window_band.left_inset * scale).round(),
                right_inset: (self.window_band.right_inset * scale).round(),
            }
        } else {
            // When: in_band is false, the surface is an ordinary strip with no insets.
            crate::status_strip::StripBand::default()
        };
        let (top, height, position) = match surface {
            Surface::Strip => {
                (self.status_line_top(), self.status_line_height(), self.status_line_position)
            }
            Surface::Band => (
                self.titlebar_inset * self.scale_factor,
                layout.separate_px,
                sonicterm_render_model::boundary::cfg::status_line::StatusPosition::Top,
            ),
        };
        let mut geometry = crate::status_strip::StripGeometry {
            top,
            height,
            position,
            screen: (sw, sh),
            scale_factor: self.scale_factor,
            font_raster_px: self.font_size * self.scale_factor,
            font_scale: self.status_line_font_scale,
            cap_height: 0.0,
            band,
        };
        let view = match surface {
            Surface::Strip => &self.status_line_view,
            Surface::Band => &self.band_view,
        };
        // Small side segments use the palette footer's stack and size, only while that is smaller
        // than the strip's own text; otherwise they keep the strip's size.
        let small_px = Some(self.raster_px(palette_footer_font_size(self.font_size)))
            .filter(|small_px| *small_px < geometry.text_px());
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        small_px.map(f32::to_bits).hash(&mut hasher);
        (
            view.pixel_hash,
            sw.to_bits(),
            geometry.text_px().to_bits(),
            band.left_inset.to_bits(),
            band.right_inset.to_bits(),
            self.tab_title_font.key(),
            self.tab_title_font.fallback_epoch(),
        )
            .hash(&mut hasher);
        let fit_key = hasher.finish();
        // Text in a band is the terminal font's size, so it shapes with the body stack.
        let text_stack = if in_band {
            self.font_stack.clone()
        } else {
            // When: in_band is false, an ordinary strip shapes with its own font_scale stack.
            self.status_line_font_stack.clone()
        };
        let mut strip_rasterizer = text_stack.clone();
        let mut small_rasterizer = small_px.and(self.palette_footer_font_stack.clone());
        let text_px = geometry.text_px();
        let variant = geometry.raster_variant();
        // The cap height is measured once per text size and kept, the band's apart from the strip's.
        let kept_cap = if in_band { self.band_cap_height } else { self.status_line_cap_height };
        let kept = crate::status_strip::kept_cap_height(kept_cap, text_px, || {
            match (text_stack.as_ref(), strip_rasterizer.as_mut()) {
                (Some(stack), Some(rasterizer)) => crate::status_strip::strip_cap_height(
                    stack,
                    rasterizer,
                    &mut self.glyph_atlas,
                    text_px,
                    variant,
                ),
                _ => text_px * 0.7,
            }
        });
        // The band keeps its own cap height so a strip below it never remeasures it.
        *(if in_band { &mut self.band_cap_height } else { &mut self.status_line_cap_height }) =
            Some(kept);
        geometry.cap_height = kept.1;
        let kept_fit = match surface {
            Surface::Strip => &self.status_line_fit,
            Surface::Band => &self.band_fit,
        };
        let stale = kept_fit.as_ref().is_none_or(|(key, _)| *key != fit_key);
        if let (true, Some(stack), Some(rasterizer)) =
            (stale, text_stack.as_ref(), strip_rasterizer.as_mut())
        {
            // The view, width, insets or font changed: refit with the chrome font.
            let view = match surface {
                Surface::Strip => &self.status_line_view,
                Surface::Band => &self.band_view,
            };
            let fitted = crate::status_strip::fit_strip(
                view,
                &geometry,
                stack,
                rasterizer,
                &mut self.glyph_atlas,
                small_text(
                    self.palette_footer_font_stack.as_ref(),
                    small_rasterizer.as_mut(),
                    small_px,
                ),
            );
            match surface {
                Surface::Strip => self.status_line_fit = Some((fit_key, fitted)),
                Surface::Band => self.band_fit = Some((fit_key, fitted)),
            }
        }
        let (fit, view) = match surface {
            Surface::Strip => (&self.status_line_fit, &self.status_line_view),
            Surface::Band => (&self.band_fit, &self.band_view),
        };
        let (placed, separators): (&[PlacedSegment], &[PlacedPart]) =
            fit.as_ref().map_or((&[], &[]), |(_, fitted)| {
                (fitted.segments.as_slice(), fitted.separators.as_slice())
            });
        crate::status_strip::emit_strip(
            placed,
            separators,
            &geometry,
            &colors,
            text_stack.as_ref(),
            strip_rasterizer.as_mut(),
            small_text(
                self.palette_footer_font_stack.as_ref(),
                small_rasterizer.as_mut(),
                small_px,
            ),
            &mut self.glyph_atlas,
            &mut self.chrome_caches.runs,
            self.chrome_reuse,
            &mut *quads,
            &mut *glyph_instances,
        );
        let focused_box = self.status_line_focus.and_then(|(id, strong)| {
            placed.iter().find_map(|segment| {
                let view = view.segments.get(segment.index)?;
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
                let view = view.segments.get(segment.index)?;
                Some(crate::status_strip::PresentedStatusSegment {
                    id: view.id,
                    x: segment.left,
                    width: segment.width,
                    click: view.click.clone(),
                })
            })
            .collect();
        let hit = crate::status_strip::StripHit {
            top: geometry.top,
            height: geometry.height,
            segments: staged,
        };
        match surface {
            Surface::Strip => self.status_line_layouts.stage(hit),
            Surface::Band => self.band_layouts.stage(hit),
        }
    }
}
