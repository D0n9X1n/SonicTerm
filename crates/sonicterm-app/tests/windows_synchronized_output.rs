#![cfg(target_os = "windows")]
//! Synchronized output (DEC 2026) on a real renderer (GDI, on the hosted runner): bytes go through
//! the pane worker's own publisher and flush decision, not through ConPTY. No frame presents
//! between `?2026h` and `?2026l`, and the frame after `?2026l` shows the final screen.

use sonicterm_app::app::{App, UserEvent};
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

/// Row writes inside the update; each is followed by a redraw request.
const ROW_WRITES: usize = 60;

/// The dispatch clock this test controls; it stays below every hold deadline.
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

/// Dispatch the real `RedrawRequested` until a frame settles successfully: `Presented`, or a
/// `CachedReblit` of an unchanged frame, which GDI returns once the frame is already on screen.
/// The pacing clock moves back so streaming pacing never holds it.
fn settle(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    let before = window_count(app, id, "presented")? + window_count(app, id, "cached")?;
    for _ in 0..300 {
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if window_count(app, id, "presented")? + window_count(app, id, "cached")? > before {
            // When: a presented or re-blitted frame was counted, the window shows the current grid.
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("the frame settled".to_owned())
}

/// Dispatch until one fresh frame is presented: the subject's single required presentation.
fn present(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    let before = window_count(app, id, "presented")?;
    for _ in 0..300 {
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if window_count(app, id, "presented")? > before {
            // When: the presented count moved, a fresh frame reached the surface.
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("the frame was presented".to_owned())
}

/// The software frame's pixels over the whole drawable terminal region, below the tab bar.
fn pane_pixels(app: &App, window: &Window, id: WindowId) -> Result<Vec<[u8; 4]>, String> {
    let (_, _, top) = app.__test_window_cell_geometry(id).ok_or("no pane geometry")?;
    let size = window.inner_size();
    let first_row = top.ceil() as u32;
    let last_row = size.height;
    (first_row..last_row)
        .flat_map(|pixel_y| (0..size.width).map(move |pixel_x| (pixel_x, pixel_y)))
        .map(|(pixel_x, pixel_y)| {
            app.__test_window_software_frame_pixel_bgra(id, pixel_x, pixel_y)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

/// The issue's update: `?2026h`, 60 row writes, `?2026l`. Write `row` lands on a grid row of the
/// `rows` the pane has, so every write is drawn and the last writes decide the final screen.
fn row_write(row: usize, rows: u16) -> Vec<u8> {
    let line = row % usize::from(rows.max(1)) + 1;
    format!("\x1b[{line};1Hrow {row:02} of the synchronized update").into_bytes()
}

/// A child window with a GDI renderer and two tabs: 0 is the subject, 1 the reference that
/// receives the same bytes without brackets.
fn run(active: &ActiveEventLoop) -> Result<(), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(800, 600))
                    .with_visible(true)
                    .with_title("SonicTerm synchronized output"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.terminal.cursor_blink = false;
    let renderer = GpuRenderer::new(
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
            role: "synchronized-output-test",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    let mut app = App::new(theme, config, Keymap::default());
    app.force_frame_counters_on().map_err(|_| "counters forced before any window")?;
    app.__test_set_software_render_degrade(true);
    app.__test_seed_tab("main");
    let id = app.__test_seed_child_window(&["subject", "reference"]);
    let panes = app.__test_window_tab_panes(id).ok_or("no child tabs")?;
    let [subject, reference] = panes[..] else {
        return Err(format!("two tab panes, found {panes:?}"));
    };
    check(app.__test_attach_window_renderer(id, window.clone(), renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    for pane in [subject, reference] {
        // A hidden cursor keeps the two panes' frames comparable.
        check(app.__test_advance_child_pane_parser(id, pane, b"\x1b[?25l"), "cursor hidden")?;
    }

    let (_, rows) = app.__test_child_pane_grid_size(id, subject).ok_or("subject grid")?;
    check(rows >= 20, &format!("the grid holds the written rows: {rows}"))?;

    // The reference frame: the same bytes, unbracketed, in tab 1.
    check(app.__test_invoke_activate_tab_in_child(id, 1), "reference tab shown")?;
    settle(&mut app, active, id)?;
    let blank = pane_pixels(&app, &window, id)?;
    let mut reference_worker = app.__test_pane_worker(id, reference).ok_or("reference worker")?;
    for row in 0..ROW_WRITES {
        let _ = reference_worker.batch(&row_write(row, rows), Instant::now());
    }
    present(&mut app, active, id)?;
    let expected = pane_pixels(&app, &window, id)?;
    check(!expected.is_empty() && expected != blank, "the reference draws the update")?;

    // The subject starts from one fresh presentation, then settles: GDI re-blits an unchanged
    // frame (`CachedReblit`), so no second fresh presentation is required.
    check(app.__test_invoke_activate_tab_in_child(id, 0), "subject tab shown")?;
    present(&mut app, active, id)?;
    settle(&mut app, active, id)?;
    // The non-forced baseline: settled, presented once, nothing pending or in flight.
    let blockers = app.__test_window_release_blockers(id);
    check(
        blockers.contains("in_flight=false") && blockers.contains("pending=false"),
        &format!("the subject settled with no forcing cause pending: {blockers}"),
    )?;
    let start = *FIXED_NOW.get_or_init(Instant::now);
    app.__test_set_dispatch_clock(fixed_now);
    let presented_before = window_count(&app, id, "presented")?;
    let held_before = window_count(&app, id, "defer_sync")?;

    let mut worker = app.__test_pane_worker(id, subject).ok_or("subject worker")?;
    let queued = worker.batch(b"\x1b[?2026h", start);
    check(queued.is_empty(), "opening the update sends no output event")?;
    for row in 0..ROW_WRITES {
        let queued = worker.batch(&row_write(row, rows), start);
        check(queued.is_empty(), "a held row write sends no output event")?;
        // An unrelated redraw lands inside the update after every row write.
        app.__test_set_window_last_render(id, start - Duration::from_secs(1));
        ApplicationHandler::window_event(&mut app, active, id, WindowEvent::RedrawRequested);
    }
    check(
        window_count(&app, id, "presented")? == presented_before,
        "no frame presents between ?2026h and ?2026l",
    )?;
    let held = window_count(&app, id, "defer_sync")? - held_before;
    check(held >= ROW_WRITES as u64, &format!("every redraw inside the update is held: {held}"))?;

    let queued = worker.batch(b"\x1b[?2026l", start);
    check(queued == [id], "the reset sends one output event")?;
    for window_id in queued {
        ApplicationHandler::user_event(
            &mut app,
            active,
            UserEvent::PaneOutput { window_id, pane_id: subject },
        );
    }
    app.__test_set_window_last_render(id, start - Duration::from_secs(1));
    ApplicationHandler::window_event(&mut app, active, id, WindowEvent::RedrawRequested);
    check(
        window_count(&app, id, "presented")? == presented_before + 1,
        "exactly one frame presents after ?2026l",
    )?;
    check(pane_pixels(&app, &window, id)? == expected, "that frame shows the final screen")
}

/// No torn frame through the production publisher: the update presents once, complete.
#[test]
fn windows_synchronized_update_presents_once_and_complete() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("synchronized output event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
