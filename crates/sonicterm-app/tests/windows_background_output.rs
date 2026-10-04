#![cfg(target_os = "windows")]
//! Background-tab output through the app on a real renderer (GDI, on the hosted runner): output in a
//! hidden tab requests no frame, and switching to that tab presents, through the production redraw
//! handler, the latest output and the OSC title the output set.

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

/// The OSC 2 title and screen the background output leaves: a cleared screen with two lines.
const OUTPUT: &[u8] = b"\x1b]2;background title\x07\x1b[2J\x1b[Hlatest line one\r\nlatest line two";

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

/// The software frame's pixels from the active pane's top edge down, below the tab bar.
fn pane_pixels(app: &App, window: &Window, id: WindowId) -> Result<Vec<[u8; 4]>, String> {
    let (_, cell_height, top) = app.__test_window_cell_geometry(id).ok_or("no pane geometry")?;
    let size = window.inner_size();
    let first_row = top.ceil() as u32;
    let last_row = (top + cell_height * 4.0).min(size.height as f32) as u32;
    (first_row..last_row)
        .flat_map(|pixel_y| (0..size.width).map(move |pixel_x| (pixel_x, pixel_y)))
        .map(|(pixel_x, pixel_y)| {
            app.__test_window_software_frame_pixel_bgra(id, pixel_x, pixel_y)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

/// Deliver the output events the hook queued, through the app's real user-event handler.
fn deliver(app: &mut App, active: &ActiveEventLoop, queued: Vec<WindowId>, pane_id: u64) {
    for window_id in queued {
        ApplicationHandler::user_event(app, active, UserEvent::PaneOutput { window_id, pane_id });
    }
}

/// A child window with a GDI renderer and three tabs: 0 is shown, 1 receives output in the
/// background, 2 receives the same output while shown and is the reference frame.
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
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    app.__test_seed_tab("main");
    let id = app.__test_seed_child_window(&["shown", "background", "reference"]);
    let panes = app.__test_window_tab_panes(id).ok_or("no child tabs")?;
    let [shown, background, reference] = panes[..] else {
        return Err(format!("three tab panes, found {panes:?}"));
    };
    check(app.__test_attach_window_renderer(id, window.clone(), renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    for pane in [shown, background, reference] {
        // A hidden cursor keeps the two panes' frames comparable.
        check(app.__test_advance_child_pane_parser(id, pane, b"\x1b[?25l"), "cursor hidden")?;
    }

    // The reference: the same output while its tab is shown.
    check(app.__test_invoke_activate_tab_in_child(id, 2), "reference tab shown")?;
    settle(&mut app, active, id)?;
    let blank = pane_pixels(&app, &window, id)?;
    let queued = app.__test_publish_pane_output(id, reference, OUTPUT);
    check(queued == [id], "the shown pane's output queues an event")?;
    deliver(&mut app, active, queued, reference);
    settle(&mut app, active, id)?;
    let expected = pane_pixels(&app, &window, id)?;
    check(expected != blank, "the reference frame draws the output")?;

    // The subject: tab 0 shown while tab 1 receives the output.
    check(app.__test_invoke_activate_tab_in_child(id, 0), "first tab shown")?;
    settle(&mut app, active, id)?;
    let queued = app.__test_publish_pane_output(id, background, OUTPUT);
    check(queued == [id], "the background pane's output queues an event")?;
    deliver(&mut app, active, queued, background);
    check(
        idle(&app, id),
        &format!("background output requests no frame: {}", app.__test_window_release_blockers(id)),
    )?;

    check(app.__test_invoke_activate_tab_in_child(id, 1), "background tab shown")?;
    settle(&mut app, active, id)?;
    check(
        pane_pixels(&app, &window, id)? == expected,
        "the switched-to frame draws the latest output, as the reference frame does",
    )?;
    let title = app.__test_window_active_tab_title(id).ok_or("no active tab")?;
    check(
        title.contains("background title"),
        &format!("the redraw applied the OSC title to the tab bar: {title:?}"),
    )
}

/// Background-tab output requests no frame, and the frame presented after switching to that tab
/// shows the latest output and title.
#[test]
fn windows_background_tab_output_is_shown_on_switch() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("background output event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
