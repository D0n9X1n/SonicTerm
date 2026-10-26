//! Live font changes for the session: size and family, rebuilt in every window, and the request
//! every window's renderer receives.

use super::{resize_panes_to_rects, App};

impl App {
    pub(super) fn set_font_size(&mut self, size: f32) {
        self.config.font.size = size;
        self.apply_live_font();
        tracing::info!("font size -> {size}pt");
    }

    /// Switch the live font family in every window for this session, without saving it.
    pub(super) fn set_font_family(&mut self, family: &str) {
        self.config.font.family = family.to_owned();
        self.font_family_generation += 1;
        self.apply_live_font();
        tracing::info!("font family -> {family}");
    }

    /// The font `apply_live_font` hands every window's renderer: the live `[font]` values.
    pub(super) fn live_font_request(&self) -> LiveFontRequest {
        LiveFontRequest {
            family: self.config.font.family.clone(),
            size: self.config.font.size,
            line_height: self.config.font.line_height,
            weight_scale: self.config.font.effective_weight_scale(),
        }
    }

    /// Rebuild every renderer's font from the live `[font]` values and refit each window's panes
    /// to the new cell size.
    fn apply_live_font(&mut self) {
        let request = self.live_font_request();
        let LiveFontRequest { family, size, line_height: line_h, weight_scale } = request.clone();
        if let Some(renderer) = self.main_renderer_mut() {
            renderer.set_font(&family, size, line_h, weight_scale);
        }
        // the loop below covers main + every child.
        for (window_id, child) in self.windows.iter_mut() {
            // Every window is handed the same request; the test recorder sees each one.
            #[cfg(test)]
            self.live_font_log.push((*window_id, request.clone()));
            #[cfg(not(test))]
            let _ = window_id;
            {
                let Some(renderer) = child.renderer.as_mut() else {
                    // When: child.renderer is None for test-seeded entries; skip so the
                    // new size reaches only windows that can actually rasterize it.
                    continue;
                };
                renderer.set_font(&family, size, line_h, weight_scale);
            }
            let Some(renderer) = child.renderer.as_ref() else {
                // When: child.renderer is None for test-seeded entries, so there is no
                // cell_size or padding inset to fit new pane rects against.
                continue;
            };
            let rects = App::compute_pane_rects_for(child);
            let (cell_width, cell_height) = renderer.cell_size();
            let inset = [
                renderer.padding_left_px(),
                renderer.padding_right_px(),
                renderer.padding_top_px(),
                renderer.padding_bottom_px(),
            ];
            resize_panes_to_rects(&child.panes, &rects, cell_width, cell_height, inset);
        }
        self.refresh_all_window_minimums();
        if let Some(window) = self.main_window() {
            crate::app::frame_counters::request_native_redraw(window);
        }
        for child in self.windows.values() {
            // When: child.renderer is None only for test-seeded entries, which carry
            // no window either, so child.request_redraw would already do nothing.
            if child.renderer.is_none() {
                continue;
            }
            child.request_window_redraw();
        }
    }
}

/// The font every window's renderer is asked for after a live font change.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LiveFontRequest {
    /// The live family.
    pub(super) family: String,
    /// The live size in points, including zoom.
    pub(super) size: f32,
    /// The line-height multiplier.
    pub(super) line_height: f32,
    /// The effective monochrome weight scale.
    pub(super) weight_scale: f32,
}
