//! Fields responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Stamp of everything outside the field text that moves field pixels:
    /// surface size, scale, font, cell metrics, panel padding, device, and device state.
    pub(super) fn field_environment(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&self.config.width, &mut hasher);
        std::hash::Hash::hash(&self.config.height, &mut hasher);
        std::hash::Hash::hash(&self.scale_factor.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.font_size.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.font_family, &mut hasher);
        std::hash::Hash::hash(&self.font_weight_scale.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.cell_w.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.panel_padding.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.device_generation(), &mut hasher);
        // A stopped device keeps its generation until recovery, but nothing it presented is current.
        std::hash::Hash::hash(&self.device_errors.state(), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }
}

impl GpuRenderer {
    /// Map a pointer (physical px) onto the presented command-palette query.
    ///
    /// `preedit` must be the IME preedit the palette was drawn with. Returns
    /// [`FieldHit::Stale`] when the presented frame shows other text, font,
    /// scale, or surface — request a redraw and retry — and never estimates.
    #[must_use]
    pub fn palette_field_hit(
        &self,
        palette: &CommandPalette,
        preedit: &str,
        point: (f32, f32),
        mode: FieldHitMode,
    ) -> FieldHit {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let Some(text) = FieldText::palette(palette, preedit) else {
            // When: the palette is closed or shows the colour picker, there is no query to hit.
            return FieldHit::Outside;
        };
        field_hit(
            self.presented_fields.palette.as_ref(),
            &text,
            self.field_environment(),
            self.font_stack.as_ref(),
            point,
            mode,
        )
    }
}

impl GpuRenderer {
    /// Map a pointer (physical px) onto the presented search query.
    ///
    /// Offsets are relative to `search.query`; the prompt and counter clamp to its edges.
    #[must_use]
    pub fn search_field_hit(
        &self,
        search: &SearchState,
        preedit: &str,
        point: (f32, f32),
        mode: FieldHitMode,
    ) -> FieldHit {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        field_hit(
            self.presented_fields.search.as_ref(),
            &FieldText::search(search, preedit),
            self.field_environment(),
            self.font_stack.as_ref(),
            point,
            mode,
        )
    }
}

impl GpuRenderer {
    /// Presented caret of the palette query (physical px) when the screen shows
    /// exactly this query, caret, selection, and preedit; `None` means redraw first.
    #[must_use]
    pub fn palette_field_caret_rect(
        &self,
        palette: &CommandPalette,
        preedit: &str,
    ) -> Option<FieldRect> {
        let text = FieldText::palette(palette, preedit)?;
        field_caret_rect(self.presented_fields.palette.as_ref(), &text, self.field_environment())
    }
}

impl GpuRenderer {
    /// Presented caret of the search query (physical px), or `None` until the
    /// screen shows exactly this query, caret, selection, and preedit.
    #[must_use]
    pub fn search_field_caret_rect(
        &self,
        search: &SearchState,
        preedit: &str,
    ) -> Option<FieldRect> {
        field_caret_rect(
            self.presented_fields.search.as_ref(),
            &FieldText::search(search, preedit),
            self.field_environment(),
        )
    }
}

impl GpuRenderer {
    /// Current font family in effect. Test-only inspector for the
    /// live-reload path; production code reads font fields directly.
    #[doc(hidden)]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }
}

impl GpuRenderer {
    /// Current font size in px.
    #[doc(hidden)]
    pub fn font_size(&self) -> f32 {
        self.font_size
    }
}

impl GpuRenderer {
    /// Measure overlay text in raster pixels with the renderer's active font
    /// stack, falling back conservatively when shaping is unavailable.
    pub fn measure_overlay_text_width(&self, text: &str, font_size: f32) -> f32 {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let estimate = estimate_badge_text_width(text, font_size);
        conservative_badge_text_width(
            estimate,
            self.font_stack.as_ref().and_then(|stack| {
                crate::frame_stats::shape_request(|| stack.measure_text_width_for_frame(text)).ok()
            }),
        )
    }
}

impl GpuRenderer {
    /// Lay out notification text with the same native shaping used to paint its glyphs.
    pub fn notification_layout(
        &self,
        message: &str,
        window: (f32, f32),
        row: u8,
    ) -> sonicterm_render_model::boundary::ui::overlays::NotificationTextLayout {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let font_size = self.raster_px(self.font_size.max(1.0));
        NotificationBubbleLayout::compute_text(
            window.0,
            window.1,
            message,
            font_size,
            row,
            self.scale_factor,
            |text| {
                self.font_stack
                    .as_ref()
                    .and_then(|stack| {
                        crate::frame_stats::shape_request(|| {
                            stack.measure_text_width_for_frame(text)
                        })
                        .ok()
                    })
                    .unwrap_or_else(|| estimate_badge_text_width(text, font_size))
            },
        )
    }
}

#[cfg(test)]
#[path = "core_fields_tests.rs"]
mod core_fields_tests;
