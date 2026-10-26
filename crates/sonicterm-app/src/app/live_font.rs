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
        // `self.windows` holds the main window and every child, so each renderer is rebuilt once.
        for (window_id, child) in self.windows.iter_mut() {
            #[cfg(not(test))]
            let _ = window_id;
            let target: &mut dyn LiveFontTarget = match child.renderer.as_mut() {
                Some(renderer) => renderer,
                #[cfg(test)]
                None => {
                    // When: child.renderer is None in a test-seeded window; its recorder takes the delivery.
                    &mut LiveFontRecorder { window_id: *window_id, log: &mut self.live_font_log }
                }
                #[cfg(not(test))]
                None => {
                    // When: child.renderer is None, the window cannot rasterize, so no font is rebuilt.
                    continue;
                }
            };
            // Every window's new font is delivered here, and only here.
            target.deliver(&request);
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

/// Where `apply_live_font` delivers a window's new font: its renderer, or in tests a recorder for a
/// window that has none.
trait LiveFontTarget {
    /// Rebuild this window's font from `request`.
    fn deliver(&mut self, request: &LiveFontRequest);
}

impl LiveFontTarget for sonicterm_gpu::core::GpuRenderer {
    fn deliver(&mut self, request: &LiveFontRequest) {
        self.set_font(&request.family, request.size, request.line_height, request.weight_scale);
    }
}

/// The test target for a window with no renderer: it records the request the window was handed.
#[cfg(test)]
struct LiveFontRecorder<'log> {
    /// The window being delivered to.
    window_id: winit::window::WindowId,
    /// The app's log of deliveries.
    log: &'log mut Vec<(winit::window::WindowId, LiveFontRequest)>,
}

#[cfg(test)]
impl LiveFontTarget for LiveFontRecorder<'_> {
    fn deliver(&mut self, request: &LiveFontRequest) {
        self.log.push((self.window_id, request.clone()));
    }
}
