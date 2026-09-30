//! Device-free tab-title font state.
//!
//! The renderer draws tab titles with a stack one point larger than the grid
//! font, and measures every stored tab width with the same stack, raster size
//! and key. `GpuRenderer::set_font`, the scale-factor rebuild and
//! `GpuRenderer::measure_tab_widths` all go through this type, so a font or
//! scale change reaches the next measurement, and a test can apply both
//! transitions without a window or a GPU device.

use sonicterm_engine::FontStack;

use super::{
    tab_content_width_px, tab_font_key, tab_title_font_size, ContentWidthRefresh, Instant, TabBar,
};

/// The tab-title font: its shaping stack, its raster size and the key a stored
/// tab width is measured under. It holds no GPU state.
pub(super) struct TabTitleFont {
    /// Native-size stack for tab titles, or `None` when no font loaded.
    stack: Option<FontStack>,
    /// Grid font family, which the key includes.
    family: String,
    /// Grid font size in logical points; titles draw one point larger.
    body_size: f32,
    /// Effective font weight scale, which the key includes.
    weight_scale: f32,
    /// Display scale factor, which sets the raster size and the privilege-marker reserve.
    scale_factor: f32,
}

impl TabTitleFont {
    /// Tab-title font state for the grid font `family` at `body_size` points and
    /// `weight_scale`, at display `scale_factor`, shaping with `stack`.
    pub(super) fn new(
        family: &str,
        body_size: f32,
        weight_scale: f32,
        scale_factor: f32,
        stack: Option<FontStack>,
    ) -> Self {
        Self { stack, family: family.to_string(), body_size, weight_scale, scale_factor }
    }

    /// Adopt a new grid font and its tab-title `stack`, as `GpuRenderer::set_font`
    /// does, so the next measurement shapes every title again under a new key.
    pub(super) fn set_font(
        &mut self,
        family: &str,
        body_size: f32,
        weight_scale: f32,
        stack: Option<FontStack>,
    ) {
        self.family = family.to_string();
        self.body_size = body_size;
        self.weight_scale = weight_scale;
        self.stack = stack;
    }

    /// Adopt display scale `scale_factor` and rasterize the stack at `dpi`, as the
    /// renderer's scale rebuild does, so the next measurement shapes every title
    /// again at the new raster size.
    pub(super) fn set_scale_factor(&mut self, scale_factor: f32, dpi: usize) {
        self.scale_factor = scale_factor;
        if let Some(stack) = self.stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), dpi);
        }
    }

    /// The tab-title stack, which drawing and measuring share.
    pub(super) fn stack(&self) -> Option<&FontStack> {
        self.stack.as_ref()
    }

    /// Raster-px em size of tab titles. Measuring and drawing both read it, so a
    /// stored tab width is always measured at the size its title is drawn at.
    pub(super) fn raster_px(&self) -> f32 {
        tab_title_font_size(self.body_size) * self.scale_factor
    }

    /// The key a stored tab width is measured under. A change to the family, size,
    /// weight, scale or stack presence shapes every title again.
    pub(super) fn key(&self) -> u64 {
        tab_font_key(
            &self.family,
            self.body_size,
            self.weight_scale,
            self.scale_factor,
            self.stack.is_some(),
        )
    }

    /// Measure changed tab titles on the CPU and store their widths on `tabs`; while
    /// `hold` is set a changed title is measured but laid out later. See
    /// `GpuRenderer::measure_tab_widths`.
    pub(super) fn measure(
        &self,
        tabs: &mut TabBar,
        process_privileged: bool,
        hold: bool,
        now: Instant,
    ) -> ContentWidthRefresh {
        let raster_px = self.raster_px();
        let stack = self.stack.as_ref();
        let scale_factor = self.scale_factor;
        tabs.refresh_content_widths(now, process_privileged, self.key(), hold, |content| {
            tab_content_width_px(stack, content, raster_px, scale_factor)
        })
    }
}

#[cfg(test)]
#[path = "tab_title_font_tests.rs"]
mod tab_title_font_tests;
