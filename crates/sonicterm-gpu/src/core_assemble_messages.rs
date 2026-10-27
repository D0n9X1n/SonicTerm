//! Assemble messages in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_messages(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        read_only_badge: Option<(f32, f32, f32, f32)>,
        search_bar_layout: Option<SearchBarLayout>,
    ) {
        let AssemblyGeometry { surface_width_px: sw, surface_height_px: sh, .. } = geometry;
        let AssemblyUi { theme, notification, link_preview, .. } = inputs;
        let frame_scratch::FrameScratch {
            overlay_glyphs: overlay_glyph_instances,
            overlay_quads: quads_overlay,
            ..
        } = scratch;
        if let Some(bubble) = notification {
            let notification_font_size = self.raster_px(self.font_size.max(1.0));
            let row = u8::from(read_only_badge.is_some()) + u8::from(search_bar_layout.is_some());
            let text_layout = self.notification_layout(&bubble.message, (sw, sh), row);
            let layout = text_layout.geometry;
            let bg_hex = match bubble.level {
                NotificationLevel::Info => theme.colors.bright.green.0.as_str(),
                NotificationLevel::Warning => theme.colors.ansi.yellow.0.as_str(),
                NotificationLevel::Error => theme.colors.bright.red.0.as_str(),
            };
            let bubble_bg = hex_to_premultiplied_rgba(bg_hex, 1.0);
            let bubble_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                bubble_bg,
                [layout.border.w, layout.border.h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                let mut wt = stack.clone();
                let text_x = layout.border.x + text_layout.padding;
                // Glyph ink may extend beyond its shaped advance into the trailing padding.
                let text_clip_w = (layout.close.x - text_x).max(0.0);
                let baseline = layout.border.y + text_layout.padding + notification_font_size;
                for (index, line) in text_layout.lines.iter().enumerate() {
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        notification_font_size,
                        notification_font_size,
                        &mut wt,
                        line,
                        bubble_fg,
                        ChromeAttrs::default(),
                        text_x,
                        baseline + index as f32 * text_layout.line_height,
                        [text_x, layout.border.y, text_clip_w, layout.border.h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                }
                let close_w =
                    estimate_badge_text_width(NOTIFICATION_CLOSE_ICON, notification_font_size);
                let close_x = layout.close.x + (layout.close.w - close_w) * 0.5;
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    notification_font_size,
                    notification_font_size,
                    &mut wt,
                    NOTIFICATION_CLOSE_ICON,
                    bubble_fg,
                    ChromeAttrs::default(),
                    close_x,
                    baseline,
                    [layout.close.x, layout.close.y, layout.close.w, layout.close.h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
            }
        }

        if let (Some(preview), Some(stack)) = (link_preview, self.font_stack.as_ref()) {
            use sonicterm_render_model::boundary::ui::overlays::{
                link_preview_text, LinkPreviewLayout,
            };
            let text = link_preview_text(&preview.uri);
            let font_size = self.raster_px(self.font_size.max(1.0));
            let layout = LinkPreviewLayout::compute(
                &text,
                preview.pointer,
                (sw, sh),
                font_size * 1.4,
                self.scale_factor,
                |value| {
                    conservative_badge_text_width(
                        estimate_badge_text_width(value, font_size),
                        crate::frame_stats::shape_request(|| {
                            stack.measure_text_width_for_frame(value)
                        })
                        .ok(),
                    )
                },
            );
            if let Some(layout) = layout {
                let chrome = self.chrome_caches.palette.palette_for(theme);
                let rect = layout.border;
                quads_overlay.push(QuadInstance::rounded(
                    px_to_ndc(rect.x, rect.y, rect.w, rect.h, sw, sh),
                    chrome.bg_surface,
                    [rect.w, rect.h],
                    self.chrome_px(4.0),
                ));
                let color = hex_to_chrome_color(theme.colors.foreground.0.as_str());
                let mut raster = stack.clone();
                for (index, line) in layout.lines.iter().enumerate() {
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        font_size,
                        font_size,
                        &mut raster,
                        line,
                        color,
                        ChromeAttrs::default(),
                        rect.x + layout.padding,
                        rect.y + layout.padding + font_size + index as f32 * layout.line_height,
                        [rect.x, rect.y, rect.w, rect.h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                }
            }
        }
    }
}
