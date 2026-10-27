//! Warm destination creation and renderer settings for tear-out.

use super::*;

pub(super) fn live_renderer_settings<'a>(
    config: &'a Config,
    theme: &'a Theme,
    tab_bar_visible: bool,
    origin: ChildRendererOrigin,
) -> LiveRendererSettings<'a> {
    let refresh_cached_state = origin == ChildRendererOrigin::WarmPool;
    LiveRendererSettings {
        font: refresh_cached_state.then_some(LiveFontSettings {
            family: &config.font.family,
            size: config.font.size,
            line_height: config.font.line_height,
            weight_scale: config.font.effective_weight_scale(),
        }),
        theme: refresh_cached_state.then_some(theme),
        background: theme.colors.background.0.as_str(),
        subpixel_aa: config.font.subpixel_aa,
        tab_bar_visible,
    }
}

impl App {
    /// Settings for a warm-pool renderer: a hidden window that may never draw, so its glyph
    /// atlas starts at the 256 floor and grows after adoption like any other.
    pub(super) fn warm_renderer_settings(&self) -> sonicterm_gpu::core::RendererSettings<'_> {
        sonicterm_gpu::core::RendererSettings {
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Minimum,
            ..self.tear_out_renderer_settings("warm")
        }
    }

    pub(super) fn tear_out_renderer_settings(
        &self,
        role: &'static str,
    ) -> sonicterm_gpu::core::RendererSettings<'_> {
        sonicterm_gpu::core::RendererSettings {
            font_family: &self.config.font.family,
            font_dirs: &self.font_dirs,
            font_size: self.config.font.size,
            line_height_mult: self.config.font.line_height,
            font_weight_scale: self.config.font.effective_weight_scale(),
            subpixel_aa: self.config.font.subpixel_aa,
            padding: [
                self.config.window.padding_left,
                self.config.window.padding_right,
                self.config.window.padding_top,
                self.config.window.padding_bottom,
            ],
            appearance: sonicterm_gpu::core::SurfaceAppearance {
                backdrop: self.config.appearance.backdrop,
                opacity: self.config.appearance.opacity,
                scrollbar: self.config.appearance.scrollbar,
                panel_padding: self.config.appearance.panel_padding,
                software_render_mode: self.config.appearance.software_render_mode,
            },
            role,
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        }
    }

    pub(in crate::app) fn warm_window_pool_maintain(&mut self, event_loop: &ActiveEventLoop) {
        let Some(software_rendering) = self.main_renderer().map(|renderer| {
            renderer.is_software_rendering() || renderer.is_software_render_degraded()
        }) else {
            // When: `main_renderer` is absent, so the software-rendering state that
            // sizes the pool cannot be read; skip maintenance rather than size it wrong.
            return;
        };
        // A stopped device refuses every renderer, so prewarming would churn hidden windows.
        let device_accepts_gpu_work =
            self.main_renderer().is_some_and(GpuRenderer::device_accepts_gpu_work);
        let configured = self.config.window.warm_window_pool;
        let target = super::super::warm_window_pool_target(configured, software_rendering);
        let count_before = self.warm_window_pool.len();
        if self.warm_window_pool.len() > target {
            self.warm_window_pool.truncate(target);
        }
        if super::super::warm_window_pool_may_spawn(
            device_accepts_gpu_work,
            self.warm_window_pool.len(),
            configured,
            software_rendering,
        ) {
            if let Some(warm) = self.create_warm_window(event_loop) {
                self.warm_window_pool.push(warm);
            }
        }
        if self.warm_window_pool.len() != count_before {
            tracing::debug!(
                target: "memory",
                configured,
                target,
                software_rendering,
                warm_renderer_count = self.warm_window_pool.len(),
                "warm renderer pool maintained"
            );
        }
    }

    pub(super) fn create_warm_window(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Option<super::super::WarmWindow> {
        let attrs = super::super::with_app_icon(super::super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::super::NATIVE_WINDOW_TITLE)
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0))
                    .with_visible(false),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let attrs = self.native_drop_attributes(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                // When: `create_window` failed, so there is no window to pool; the pool
                // only pre-warms, so a short pool costs tear-out latency, not correctness.
                tracing::warn!("warm-window-pool: create_window failed: {error}");
                return None;
            }
        };
        window.set_ime_allowed(true);
        // A dormant adapter must exist before the spare is first shown at tear-out.
        self.install_accessibility(event_loop, &window);
        let settings = self.warm_renderer_settings();
        let shared_gpu = self.shared_gpu_context();
        let mut renderer = match shared_gpu.map_or_else(
            || GpuRenderer::new(window.clone(), event_loop, &self.theme, settings),
            |ctx| {
                GpuRenderer::new_with_shared_context(
                    window.clone(),
                    event_loop,
                    &self.theme,
                    settings,
                    ctx,
                )
            },
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: `GpuRenderer` construction failed, so the created window has no
                // renderer to pool; drop it rather than pool a window that cannot draw.
                tracing::warn!("warm-window-pool: renderer init failed: {error}");
                return None;
            }
        };
        if !self.configure_child_renderer(&mut renderer, &window, ChildRendererOrigin::Fresh) {
            // When: `configure_child_renderer` rejected the initial size, so this window
            // cannot render; drop it rather than leave an unusable entry in the pool.
            tracing::error!("warm-window-pool: renderer rejected unsafe initial size");
            return None;
        }
        Some(super::super::WarmWindow { window, renderer, created_at: Instant::now() })
    }

    pub(super) fn take_warm_window(&mut self) -> Option<super::super::WarmWindow> {
        self.warm_window_pool.pop()
    }

    /// Test hook: build one warm-pool window exactly as pool maintenance does, pool it, and return
    /// its id; `None` when its window or renderer could not be built.
    #[doc(hidden)]
    pub fn __test_prewarm_window(&mut self, event_loop: &ActiveEventLoop) -> Option<WindowId> {
        let warm = self.create_warm_window(event_loop)?;
        let id = warm.window.id();
        self.warm_window_pool.push(warm);
        Some(id)
    }

    /// Test hook: the glyph atlas dimension of pooled warm window `id`, `None` once it left the pool.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_warm_glyph_atlas_dim(&self, id: WindowId) -> Option<u32> {
        self.warm_window_pool
            .iter()
            .find(|warm| warm.window.id() == id)
            .map(|warm| warm.renderer.glyph_atlas_facts().dim)
    }

    /// Test hook: tear the main window's tab at `index` out through the production route, which
    /// adopts a pooled warm window when one is ready.
    #[doc(hidden)]
    pub fn __test_tear_out_tab(&mut self, event_loop: &ActiveEventLoop, index: usize) -> bool {
        self.tear_out_tab(event_loop, index)
    }

    pub(in crate::app) fn is_warm_window_id(&self, win_id: WindowId) -> bool {
        self.warm_window_pool.iter().any(|warm| warm.window.id() == win_id)
    }
}

#[cfg(test)]
#[path = "tear_out_warm_tests.rs"]
mod tear_out_warm_tests;
