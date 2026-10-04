#![cfg(target_os = "windows")]
//! A real `Resized` through the main and child handlers on a real renderer (GDI, on the hosted
//! runner): a resize at the configured size arms no obligation, so an unrelated redraw during a
//! synchronized update is held; a changed size forces a frame through the hold, and that
//! obligation survives a following same-size `Resized`.

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::{Arc, OnceLock},
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

/// The dispatch clock this test controls; it stays below every hold deadline and window cap.
static FIXED_NOW: OnceLock<Instant> = OnceLock::new();

/// The controlled dispatch clock.
fn fixed_now() -> Instant {
    *FIXED_NOW.get().expect("the clock is fixed before it is installed")
}

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

/// A count from window `id`'s frame-counter record.
fn window_count(app: &App, id: WindowId, name: &str) -> Result<u64, String> {
    let snapshot = app.frame_counters_snapshot().ok_or("counters are on")?;
    let (_, record) =
        snapshot.windows.iter().find(|(window, _)| *window == id).ok_or("window record")?;
    record.count(name).ok_or_else(|| format!("{name} in the window record"))
}

/// A visible native window and a GDI renderer for it.
fn window_and_renderer(
    active: &ActiveEventLoop,
    config: &Config,
    theme: &Theme,
    title: &str,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(800, 600))
                    .with_visible(true)
                    .with_title(title),
            )
            .map_err(|error| error.to_string())?,
    );
    let renderer = GpuRenderer::new(
        window.clone(),
        active,
        theme,
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
            role: "synchronized-resize-test",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok((window, renderer))
}

/// Dispatch `RedrawRequested` once with the pacing clock moved back, so only the synchronized
/// hold or a forcing cause decides; returns how `presented` and `defer_sync` moved.
fn redraw_once(
    app: &mut App,
    active: &ActiveEventLoop,
    id: WindowId,
) -> Result<(u64, u64), String> {
    let (presented, held) =
        (window_count(app, id, "presented")?, window_count(app, id, "defer_sync")?);
    app.__test_set_window_last_render(id, fixed_now() - Duration::from_secs(1));
    ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
    Ok((
        window_count(app, id, "presented")? - presented,
        window_count(app, id, "defer_sync")? - held,
    ))
}

/// Present window `id`'s first frame, before the clock is fixed.
fn first_present(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    for _ in 0..300 {
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if window_count(app, id, "presented")? > 0 {
            // When: the presented count moved, the window has shown its first frame.
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("the first frame was presented".to_owned())
}

/// One role: a same-size `Resized` leaves an unrelated redraw held; a changed size, then the same
/// size again, forces one frame through the hold, after which the hold applies again.
fn exercise(
    app: &mut App,
    active: &ActiveEventLoop,
    id: WindowId,
    pane: u64,
    role: &str,
) -> Result<(), String> {
    let start = fixed_now();
    let configured = app
        .__test_window_renderer_mut(id)
        .map(|renderer| renderer.surface_size())
        .ok_or_else(|| format!("{role}: no renderer"))?;
    let configured = PhysicalSize::new(configured.0, configured.1);
    ApplicationHandler::window_event(app, active, id, WindowEvent::Resized(configured));

    let mut worker = app.__test_pane_worker(id, pane).ok_or_else(|| format!("{role}: worker"))?;
    check(
        worker.batch(b"\x1b[?2026hpainted row", start).is_empty(),
        &format!("{role}: held update"),
    )?;
    let (presented, held) = redraw_once(app, active, id)?;
    check(
        (presented, held) == (0, 1),
        &format!(
            "{role}: a same-size Resized arms nothing, so the redraw is held: {presented} {held}"
        ),
    )?;

    let changed = PhysicalSize::new(configured.width - 16, configured.height - 16);
    ApplicationHandler::window_event(app, active, id, WindowEvent::Resized(changed));
    ApplicationHandler::window_event(app, active, id, WindowEvent::Resized(changed));
    let (presented, held) = redraw_once(app, active, id)?;
    check(
        (presented, held) == (1, 0),
        &format!("{role}: the changed size survives a same-size Resized and forces a frame: {presented} {held}"),
    )?;
    let (presented, held) = redraw_once(app, active, id)?;
    check(
        (presented, held) == (0, 1),
        &format!("{role}: once presented, the hold applies again: {presented} {held}"),
    )?;
    let _ = worker.batch(b"\x1b[?2026l", start);
    Ok(())
}

/// The main window and a child window, each with a GDI renderer and one pane.
fn run(active: &ActiveEventLoop) -> Result<(), String> {
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.terminal.cursor_blink = false;
    let (main_window, main_renderer) = window_and_renderer(active, &config, &theme, "main")?;
    let (child_window, child_renderer) = window_and_renderer(active, &config, &theme, "child")?;
    let mut app = App::new(theme, config, Keymap::default());
    app.force_frame_counters_on().map_err(|_| "counters forced before any window")?;
    app.__test_set_software_render_degrade(true);
    app.__test_seed_tab("main");
    let main = app.__test_main_window_id().ok_or("main window")?;
    let child = app.__test_seed_child_window(&["child"]);
    check(app.__test_attach_window_renderer(main, main_window, main_renderer), "main renderer")?;
    check(
        app.__test_attach_window_renderer(child, child_window, child_renderer),
        "child renderer",
    )?;
    let main_pane = app.__test_window_tab_panes(main).and_then(|panes| panes.first().copied());
    let child_pane = app.__test_window_tab_panes(child).and_then(|panes| panes.first().copied());
    let (main_pane, child_pane) = (main_pane.ok_or("main pane")?, child_pane.ok_or("child pane")?);
    first_present(&mut app, active, main)?;
    first_present(&mut app, active, child)?;
    FIXED_NOW.get_or_init(Instant::now);
    app.__test_set_dispatch_clock(fixed_now);
    exercise(&mut app, active, main, main_pane, "main")?;
    exercise(&mut app, active, child, child_pane, "child")
}

/// A same-size `Resized` through either handler does not force a frame through a synchronized
/// hold; a changed size does, and keeps that obligation through a following same-size event.
#[test]
fn windows_resized_forces_a_held_frame_only_for_a_changed_size() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("synchronized resize event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
