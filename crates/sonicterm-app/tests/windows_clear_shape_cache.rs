#![cfg(target_os = "windows")]
//! `UserEvent::ClearShapeCache` through the app on a real renderer (GDI, on the hosted runner): it
//! clears every live renderer's shape caches and requests a frame for each window, as before
//! frame shaping stopped waiting for fallback.

use sonicterm_app::app::{App, UserEvent};
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so the whole case runs inside this one.
        self.outcome = Some(run(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// Dispatch the real `RedrawRequested` until a frame is presented, retrying briefly as the
/// link-preview fixture does, since a frame can be skipped while the swapchain settles.
fn present(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    let started = Instant::now();
    loop {
        app.__test_set_window_last_render(id, started - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if app.__test_window_last_render(id).is_some_and(|time| time >= started) {
            // When: the window's last render moved past `started`, a frame was presented.
            return Ok(());
        }
        check(started.elapsed() < Duration::from_secs(3), "the frame was presented")?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Present until nothing is pending or in flight; a frame that changes focus may ask for one more.
fn settle(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    for _ in 0..5 {
        present(app, active, id)?;
        if idle(app, id) {
            // When: no cause is pending and no request is in flight, the frame sequence settled.
            return Ok(());
        }
    }
    Err(format!("the window never settled: {}", app.__test_window_release_blockers(id)))
}

/// Whether window `id` has no redraw request in flight and no cause pending.
fn idle(app: &App, id: WindowId) -> bool {
    let blockers = app.__test_window_release_blockers(id);
    blockers.contains("in_flight=false") && blockers.contains("pending=false")
}

fn run(active: &ActiveEventLoop) -> Result<(), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 360))
                    .with_visible(true)
                    .with_title("SonicTerm background output"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.terminal.cursor_blink = false;
    config.window.padding_left = 0.0;
    config.window.padding_right = 0.0;
    config.window.padding_top = 0.0;
    config.window.padding_bottom = 0.0;
    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &theme,
        RendererSettings {
            font_family: &config.font.family,
            font_dirs: &[],
            font_size: config.font.size,
            line_height_mult: config.font.line_height,
            font_weight_scale: config.font.effective_weight_scale(),
            subpixel_aa: config.font.subpixel_aa,
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: config.appearance.backdrop,
                opacity: 1.0,
                scrollbar: config.appearance.scrollbar,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "background-output-test",
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    app.__test_seed_tab("main");
    let id = app.__test_seed_child_window(&["shown"]);
    check(app.__test_attach_window_renderer(id, window.clone(), renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    settle(&mut app, active, id)?;
    check(idle(&app, id), "the window settled")?;
    let before = app.__test_window_style_rev(id).ok_or("no renderer style revision")?;
    ApplicationHandler::user_event(&mut app, active, UserEvent::ClearShapeCache);
    let after = app.__test_window_style_rev(id).ok_or("no renderer style revision")?;
    check(
        after > before,
        &format!("the shape caches were cleared: style_rev {before} -> {after}"),
    )?;
    check(
        !idle(&app, id),
        &format!("the clear requested a frame: {}", app.__test_window_release_blockers(id)),
    )?;
    settle(&mut app, active, id)
}

/// A legacy shape-cache clear still clears every live renderer's caches and asks each window for a
/// frame, unchanged by the non-blocking fallback path.
#[test]
fn windows_clear_shape_cache_clears_and_redraws_every_window() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("clear shape cache event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
