//! Hidden tear-out destination preparation and committed window installation.

use super::*;

impl App {
    /// Build and configure a hidden destination without mutating pane state.
    pub(super) fn prepare_tear_out_destination(
        &mut self,
        event_loop: &ActiveEventLoop,
        screen_pos: Option<(i32, i32)>,
        source: &'static str,
        request: super::super::WindowRequest,
    ) -> Result<PreparedDestination, DestinationFailure> {
        let tear_start = Instant::now();
        let spare_accepts_gpu_work =
            self.warm_window_pool.last().map(|warm| warm.renderer.device_accepts_gpu_work());
        if let Some(stage) = warm_destination_refusal(spare_accepts_gpu_work) {
            // When: `warm_destination_refusal` returns a stage, the stopped spare stays pooled.
            return Err(DestinationFailure::new(
                ChildRendererOrigin::WarmPool,
                stage,
                "warm destination's GPU device stopped accepting work".to_owned(),
                DestinationUnwind::nothing(),
            ));
        }
        let (window, renderer, create_window_ms, renderer_init_ms, resize_ms) = match self
            .take_warm_window()
        {
            Some(mut warm) => {
                // When: `take_warm_window` returns `Some`, adopt that hidden
                // renderer rather than constructing another destination.
                if let Some((screen_x, screen_y)) = screen_pos {
                    warm.window
                        .set_outer_position(winit::dpi::PhysicalPosition::new(screen_x, screen_y));
                }
                let resize_start = Instant::now();
                if !self.configure_child_renderer(
                    &mut warm.renderer,
                    &warm.window,
                    ChildRendererOrigin::WarmPool,
                ) {
                    // When: pooled adoption mutated the renderer but rejected its
                    // size, retire that hidden entry rather than returning it poisoned.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::WarmPool,
                        TearOutStage::RendererConfigure,
                        "renderer rejected unsafe child size".to_owned(),
                        DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm),
                    ));
                }
                if !super::super::apply_window_request(&warm.window, &mut warm.renderer, request) {
                    // When: inherited sizing exceeds renderer bounds, retire the mutated hidden window and restore its source.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::WarmPool,
                        TearOutStage::RendererConfigure,
                        "renderer rejected inherited child size".to_owned(),
                        DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm),
                    ));
                }
                if !warm.renderer.device_accepts_gpu_work() {
                    // When: `device_accepts_gpu_work` fails, retire the adopted spare.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::WarmPool,
                        TearOutStage::RendererConfigure,
                        "warm destination's GPU device stopped during adoption".to_owned(),
                        DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm),
                    ));
                }
                let resize_ms = resize_start.elapsed().as_secs_f32() * 1000.0;
                (warm.window, warm.renderer, 0.0, 0.0, resize_ms)
            }
            None => {
                // When: `take_warm_window` returns `None`, construct a fresh
                // destination hidden so every failure remains invisible.
                let mut attrs =
                    super::super::with_app_icon(super::super::with_backdrop_transparency(
                        with_integrated_titlebar(
                            Window::default_attributes()
                                .with_title(super::super::NATIVE_WINDOW_TITLE)
                                .with_decorations(true)
                                .with_inner_size(request.inner_size)
                                .with_visible(false),
                        ),
                        self.config.appearance.backdrop,
                        self.config.appearance.software_render_mode,
                    ));
                if let Some((screen_x, screen_y)) = screen_pos {
                    attrs =
                        attrs.with_position(winit::dpi::PhysicalPosition::new(screen_x, screen_y));
                }
                let create_start = Instant::now();
                let attrs = self.native_drop_attributes(attrs);
                let window = event_loop.create_window(attrs).map(Arc::new).map_err(|error| {
                    DestinationFailure::new(
                        ChildRendererOrigin::Fresh,
                        TearOutStage::CreateWindow,
                        error.to_string(),
                        DestinationUnwind::nothing(),
                    )
                })?;
                let create_window_ms = create_start.elapsed().as_secs_f32() * 1000.0;
                window.set_ime_allowed(true);
                // A dormant adapter must exist before the destination is first shown.
                self.install_accessibility(event_loop, &window);
                let shared_gpu = self.shared_gpu_context();
                let renderer_settings = self.tear_out_renderer_settings("child");
                let renderer_start = Instant::now();
                let mut renderer = shared_gpu
                    .map_or_else(
                        || {
                            GpuRenderer::new(
                                window.clone(),
                                event_loop,
                                &self.theme,
                                renderer_settings,
                            )
                        },
                        |ctx| {
                            GpuRenderer::new_with_shared_context(
                                window.clone(),
                                event_loop,
                                &self.theme,
                                renderer_settings,
                                ctx,
                            )
                        },
                    )
                    .map_err(|error| {
                        DestinationFailure::new(
                            ChildRendererOrigin::Fresh,
                            TearOutStage::RendererInit,
                            error.to_string(),
                            DestinationUnwind::drop_fresh(window.clone(), None),
                        )
                    })?;
                let renderer_init_ms = renderer_start.elapsed().as_secs_f32() * 1000.0;
                let resize_start = Instant::now();
                if !self.configure_child_renderer(
                    &mut renderer,
                    &window,
                    ChildRendererOrigin::Fresh,
                ) {
                    // When: fresh configuration rejects the hidden destination,
                    // its renderer and window remain owned by the failure.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::Fresh,
                        TearOutStage::RendererConfigure,
                        "renderer rejected unsafe child size".to_owned(),
                        DestinationUnwind::drop_fresh(window, Some(renderer)),
                    ));
                }
                if !renderer.device_accepts_gpu_work() {
                    // When: `device_accepts_gpu_work` fails, drop the fresh destination.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::Fresh,
                        TearOutStage::RendererConfigure,
                        "fresh destination's GPU device stopped during sizing".to_owned(),
                        DestinationUnwind::drop_fresh(window, Some(renderer)),
                    ));
                }
                let resize_ms = resize_start.elapsed().as_secs_f32() * 1000.0;
                (window, renderer, create_window_ms, renderer_init_ms, resize_ms)
            }
        };
        let mut timing = crate::app::TearOutTiming::new(source, tear_start);
        timing.create_window_ms = create_window_ms;
        timing.renderer_init_ms = renderer_init_ms;
        timing.resize_ms = resize_ms;
        Ok(PreparedDestination { window, renderer, timing })
    }
}

#[cfg(test)]
#[path = "tear_out_destination_tests.rs"]
mod tear_out_destination_tests;
