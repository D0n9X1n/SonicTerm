#![cfg(target_os = "windows")]
//! Visible parser guards released before presentation, through both production redraw adapters on
//! a real renderer: a hook at the start of presentation can lock and change the visible pane, the
//! change is drawn by the next frame and acknowledged at the collection after it, a stop during or
//! before presentation clears nothing, and the IME area is the cursor the frame drew.

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
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
        // winit allows one event loop per process, so every case runs inside this one.
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

/// Which production redraw adapter a case drives.
#[derive(Clone, Copy, Debug)]
enum Role {
    Main,
    Child,
}

/// One window on a real renderer, with the visible pane the cases change.
struct Fixture {
    app: App,
    id: WindowId,
    pane: u64,
    fallback: FallbackRequests,
}

/// Fallback requests the renderer has queued and its worker has ended.
#[derive(Clone, Default)]
struct FallbackRequests {
    scheduled: Arc<AtomicU64>,
    ended: Arc<AtomicU64>,
}

impl FallbackRequests {
    /// `(scheduled, ended)` now.
    fn counts(&self) -> (u64, u64) {
        (self.scheduled.load(Ordering::SeqCst), self.ended.load(Ordering::SeqCst))
    }
}

/// Whether this host enumerates no wgpu adapter at all, established apart from renderer
/// construction. That is the only limitation that turns a failed wgpu renderer into a skip;
/// surface configuration, device and resource errors on a host with an adapter fail the test.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// Classify a wgpu renderer construction failure: `HOST_INCAPABLE` only when no adapter exists.
fn wgpu_construction_failure(error: impl std::fmt::Display) -> String {
    if host_has_no_adapter() {
        format!("HOST_INCAPABLE: no wgpu adapter: {error}")
    } else {
        // When: host_has_no_adapter is false an adapter exists, so the construction error is a defect.
        format!("wgpu renderer construction failed on a host with an adapter: {error}")
    }
}

/// A window of `role` with a real renderer, presenting through GDI (`software`) or wgpu.
/// A wgpu renderer is skipped as `HOST_INCAPABLE: ...` only when the host enumerates no adapter;
/// any other construction error fails. The presenter each fixture resolved is checked.
fn fixture(active: &ActiveEventLoop, role: Role, software: bool) -> Result<Fixture, String> {
    fixture_with_fallback_delay(active, role, software, None)
}

/// [`fixture`], with each fallback request held on the worker until `delay` after construction
/// when given, so a request can end after the first frames as it does on a slow font scan.
fn fixture_with_fallback_delay(
    active: &ActiveEventLoop,
    role: Role,
    software: bool,
    delay: Option<Duration>,
) -> Result<Fixture, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(480, 240))
                    .with_visible(true)
                    .with_title("SonicTerm release before present"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    // `Off` never degrades, so the wgpu cases present through wgpu even on a software adapter.
    let mode = if software { SoftwareRenderMode::Force } else { SoftwareRenderMode::Off };
    config.appearance.software_render_mode = mode;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.terminal.cursor_blink = false;
    config.window.padding_left = 0.0;
    config.window.padding_right = 0.0;
    config.window.padding_top = 0.0;
    config.window.padding_bottom = 0.0;
    let created = GpuRenderer::new(
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
                software_render_mode: mode,
            },
            role: "release-before-present-test",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    );
    let mut renderer = match created {
        Ok(renderer) => renderer,
        Err(error) if !software => return Err(wgpu_construction_failure(error)),
        Err(error) => return Err(error.to_string()),
    };
    renderer.set_cursor_blink(false);
    let fallback = FallbackRequests::default();
    let (scheduled, ended) = (Arc::clone(&fallback.scheduled), Arc::clone(&fallback.ended));
    let release_at = delay.map(|delay| Instant::now() + delay);
    renderer.__test_set_fallback_request_hooks(
        Arc::new(move || {
            scheduled.fetch_add(1, Ordering::SeqCst);
        }),
        Arc::new(move || {
            while release_at.is_some_and(|release_at| Instant::now() < release_at) {
                std::thread::sleep(Duration::from_millis(10));
            }
            ended.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(software);
    let main_pane = app.__test_seed_tab("main");
    let (id, pane) = match role {
        Role::Main => (app.__test_main_window_id().ok_or("no main window")?, main_pane),
        Role::Child => {
            let id = app.__test_seed_child_window(&["child"]);
            let panes = app.__test_window_tab_panes(id).ok_or("no child tabs")?;
            (id, *panes.first().ok_or("no child pane")?)
        }
    };
    check(app.__test_attach_window_renderer(id, window, renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    let degraded =
        app.__test_window_renderer_mut(id).map(|renderer| renderer.is_software_render_degraded());
    check(degraded == Some(software), &format!("the renderer presents through GDI={software}"))?;
    let mut fixture = Fixture { app, id, pane, fallback };
    settle_fallback(&mut fixture, active)?;
    // Settling consumed the first frame; every row is dirty again, as on a fresh window.
    let parser =
        fixture.app.__test_child_pane_parser(fixture.id, fixture.pane).ok_or("no pane parser")?;
    parser.lock().grid_mut().mark_all_dirty();
    Ok(fixture)
}

/// Draw until every fallback request the frames queued has ended and no frame has presented for a
/// second, within twenty seconds. A request is counted when it is queued, before any font scan,
/// so a lookup still running keeps the counts apart. A request that ends later applies a newer
/// font generation, which presents a frame on its own and would add an unrequested receipt inside
/// a case.
fn settle_fallback(fixture: &mut Fixture, active: &ActiveEventLoop) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut observed = None;
    let mut quiet_since = Instant::now();
    loop {
        dispatch(fixture, active, Instant::now());
        let (scheduled, ended) = fixture.fallback.counts();
        let frames = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
        let state = (scheduled, ended, frames);
        if observed != Some(state) {
            // When: a request was queued or ended, or a frame presented, the quiet second restarts.
            observed = Some(state);
            quiet_since = Instant::now();
        }
        if scheduled == ended && quiet_since.elapsed() >= Duration::from_secs(1) {
            return Ok(());
        }
        check(
            Instant::now() < deadline,
            &format!("the font fallback settles: {scheduled} queued, {ended} ended"),
        )?;
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Dispatch the real `RedrawRequested` until the renderer counts one more presented frame.
fn present(fixture: &mut Fixture, active: &ActiveEventLoop) -> Result<(), String> {
    let started = Instant::now();
    let presented = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
    loop {
        dispatch(fixture, active, Instant::now());
        if fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0) > presented {
            // When: the successful-presentation count moved, a frame was presented.
            return Ok(());
        }
        check(started.elapsed() < Duration::from_secs(3), "the frame was presented")?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Dispatch one real `RedrawRequested` with pacing open, whatever the frame does.
fn dispatch(fixture: &mut Fixture, active: &ActiveEventLoop, now: Instant) {
    fixture.app.__test_set_window_last_render(fixture.id, now - Duration::from_secs(1));
    ApplicationHandler::window_event(
        &mut fixture.app,
        active,
        fixture.id,
        WindowEvent::RedrawRequested,
    );
}

/// Install a present hook that locks the visible pane's parser with `try_lock` under a 1 s
/// watchdog, writes `bytes`, records whether the lock was free, and stops the device if `stop`.
fn lock_and_write(
    fixture: &mut Fixture,
    bytes: &'static [u8],
    stop: bool,
) -> Result<Arc<Mutex<Option<bool>>>, String> {
    let parser =
        fixture.app.__test_child_pane_parser(fixture.id, fixture.pane).ok_or("no pane parser")?;
    let locked = Arc::new(Mutex::new(None));
    let record = Arc::clone(&locked);
    let hook: Box<dyn FnMut() -> bool + Send> = Box::new(move || {
        let watchdog = Instant::now();
        let free = loop {
            if let Some(mut guard) = parser.try_lock() {
                drop(guard.advance(bytes));
                break true;
            }
            if watchdog.elapsed() >= Duration::from_secs(1) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        *record.lock().unwrap() = Some(free);
        stop
    });
    check(fixture.app.__test_set_window_present_hook(fixture.id, Some(hook)), "hook installed")?;
    Ok(locked)
}

/// Remove the window's present hook.
fn unhook(fixture: &mut Fixture) -> Result<(), String> {
    check(fixture.app.__test_set_window_present_hook(fixture.id, None), "hook removed")
}

/// The window's dirty rows of its visible pane.
fn dirty(fixture: &Fixture) -> Result<Vec<usize>, String> {
    fixture
        .app
        .__test_window_pane_dirty_rows(fixture.id, fixture.pane)
        .ok_or_else(|| "no pane".into())
}

/// The software frame's pixels of terminal row `row`, over its first 16 columns: the band the
/// hook's text lands in, clear of the tab bar above and of any chrome at the right edge.
fn row_band(fixture: &Fixture, row: u16) -> Result<Vec<[u8; 4]>, String> {
    let (cell_width, cell_height, top) =
        fixture.app.__test_window_cell_geometry(fixture.id).ok_or("no cell geometry")?;
    let first_y = (top + f32::from(row) * cell_height).ceil() as u32;
    let last_y = (top + f32::from(row + 1) * cell_height).floor() as u32;
    let last_x = (16.0 * cell_width) as u32;
    (first_y..last_y)
        .flat_map(|pixel_y| (0..last_x).map(move |pixel_x| (pixel_x, pixel_y)))
        .map(|(pixel_x, pixel_y)| {
            fixture
                .app
                .__test_window_software_frame_pixel_bgra(fixture.id, pixel_x, pixel_y)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

/// The hook's row as a frame draws it when the same bytes are written directly, with no hook.
fn reference_row(active: &ActiveEventLoop, role: Role) -> Result<Vec<[u8; 4]>, String> {
    let mut reference = fixture(active, role, true)?;
    for bytes in [b"\x1b[?25l".as_slice(), b"base", b"\x1b[2;1Hhooked"] {
        let written =
            reference.app.__test_advance_child_pane_parser(reference.id, reference.pane, bytes);
        check(written, "reference written")?;
        present(&mut reference, active)?;
    }
    row_band(&reference, 1)
}

/// Tests 1, 2, 8a and 8b: during presentation the hook locks the visible pane's parser and writes
/// a row, and the frame presents. The next frame draws that row: on GDI its pixels equal a frame
/// that wrote the same bytes directly, and differ before that frame. Its receipt waits; with nothing
/// changed after it, the following collection clears the dirt and no frame is presented.
fn parse_during_present_is_drawn_then_acknowledged(
    active: &ActiveEventLoop,
    role: Role,
    software: bool,
) -> Result<(), String> {
    let mut fixture = fixture(active, role, software)?;
    // A hidden cursor keeps the hook's row comparable with the reference's.
    check(
        fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, b"\x1b[?25l"),
        "hide",
    )?;
    present(&mut fixture, active)?;
    let locked = lock_and_write(&mut fixture, b"\x1b[2;1Hhooked", false)?;
    check(fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, b"base"), "base")?;
    let presented = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
    present(&mut fixture, active)?;
    check(*locked.lock().unwrap() == Some(true), "the hook locked the visible parser in 1 s")?;
    check(
        fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0) > presented,
        "the hooked frame is Presented",
    )?;
    unhook(&mut fixture)?;
    check(dirty(&fixture)?.contains(&1), "the hook's row is dirty after the frame")?;
    let before = if software { Some(row_band(&fixture, 1)?) } else { None };
    present(&mut fixture, active)?;
    if let Some(before) = before {
        let reference = reference_row(active, role)?;
        check(before != reference, "before that frame the hook's row is not drawn")?;
        check(row_band(&fixture, 1)? == reference, "the next frame draws the hook's row")?;
    }
    check(
        fixture.app.__test_window_pending_receipts(fixture.id) == Some(1),
        "the drawing frame's receipt waits for the next collection",
    )?;
    let presented = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
    dispatch(&mut fixture, active, Instant::now());
    check(dirty(&fixture)?.is_empty(), "the next collection clears the drawn dirt")?;
    check(fixture.app.__test_window_pending_receipts(fixture.id) == Some(0), "the set is applied")?;
    check(
        fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0) == presented,
        "the unchanged key presents no frame",
    )
}

/// Tests 8c and 4f: a stop during presentation yields no receipts; the hook's dirt is kept and
/// no frame is counted as presented.
fn stop_during_present_clears_nothing(active: &ActiveEventLoop, role: Role) -> Result<(), String> {
    let mut fixture = fixture(active, role, true)?;
    present(&mut fixture, active)?;
    let locked = lock_and_write(&mut fixture, b"\x1b[3;1Hstopped", true)?;
    check(fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, b"x"), "change")?;
    let presented = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
    dispatch(&mut fixture, active, Instant::now());
    check(*locked.lock().unwrap() == Some(true), "the hook ran with the parser free")?;
    check(
        fixture.app.__test_window_pending_receipts(fixture.id) == Some(0),
        "the stopped frame stored no receipts",
    )?;
    check(dirty(&fixture)?.contains(&2), "the hook's dirt is kept")?;
    check(
        fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0) == presented,
        "the stopped frame is not Presented",
    )
}

/// Test 7a's device stop: after frame N presents, a device stop means the next redraw runs no
/// collection, so N's receipts and the dirt both wait.
fn stop_before_collection_keeps_the_set(
    active: &ActiveEventLoop,
    role: Role,
) -> Result<(), String> {
    let mut fixture = fixture(active, role, true)?;
    present(&mut fixture, active)?;
    check(fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, b"n"), "change")?;
    present(&mut fixture, active)?;
    let pending = fixture.app.__test_window_pending_receipts(fixture.id);
    check(pending.is_some_and(|count| count > 0), "frame N left receipts")?;
    let dirt = dirty(&fixture)?;
    check(fixture.app.__test_stop_window_device(fixture.id), "device stopped")?;
    dispatch(&mut fixture, active, Instant::now());
    check(fixture.app.__test_window_pending_receipts(fixture.id) == pending, "the set is kept")?;
    check(dirty(&fixture)? == dirt, "nothing is cleared")
}

/// Test 12: the hook rewrites the marker row and moves the cursor during presentation; the IME
/// area published for that frame is the drawn cursor's. Once drawn, the moved cursor is published.
fn ime_area_is_the_drawn_cursor(active: &ActiveEventLoop, role: Role) -> Result<(), String> {
    let mut fixture = fixture(active, role, true)?;
    present(&mut fixture, active)?;
    let drawn = fixture.app.__test_window_ime_area(fixture.id).ok_or("no IME area published")?;
    let locked = lock_and_write(&mut fixture, b"\x1b[4;1Hmarker\x1b[4;9H", false)?;
    check(
        fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, b"a\x08"),
        "edit",
    )?;
    present(&mut fixture, active)?;
    check(*locked.lock().unwrap() == Some(true), "the hook moved the cursor during present")?;
    check(
        fixture.app.__test_window_ime_area(fixture.id) == Some(drawn),
        "the IME area is the cursor the frame drew, not the moved one",
    )?;
    unhook(&mut fixture)?;
    present(&mut fixture, active)?;
    let moved = fixture.app.__test_window_ime_area(fixture.id).ok_or("no IME area")?;
    let (cell_width, cell_height, _) =
        fixture.app.__test_window_cell_geometry(fixture.id).ok_or("no cell geometry")?;
    let expected =
        (drawn.0 .0 + (8.0 * cell_width) as i32, drawn.0 .1 + (3.0 * cell_height) as i32);
    check(
        (moved.0 .0 - expected.0).abs() <= 1 && (moved.0 .1 - expected.1).abs() <= 1,
        &format!("the drawn moved cursor is published: {moved:?}, expected about {expected:?}"),
    )
}

/// Test 9: a fixture whose fallback request ends late is settled before any case starts, so the
/// request cannot present an extra frame inside a case. Each request is held six seconds on the
/// worker, longer than any quiet window, so only waiting for the request's end can pass.
fn late_fallback_lands_before_cases(active: &ActiveEventLoop, role: Role) -> Result<(), String> {
    let mut fixture =
        fixture_with_fallback_delay(active, role, true, Some(Duration::from_secs(6)))?;
    dispatch(&mut fixture, active, Instant::now());
    let (scheduled, ended) = fixture.fallback.counts();
    if scheduled == 0 {
        // When: no frame queued a fallback request, this host cannot exercise the late-request path.
        println!(
            "capability=HOST_INCAPABLE case={role:?}-late-fallback reason=no fallback request"
        );
        return Ok(());
    }
    check(
        scheduled == ended,
        &format!("every fallback request ended: {scheduled} queued, {ended} ended"),
    )
}

fn run(active: &ActiveEventLoop) -> Result<(), String> {
    for role in [Role::Main, Role::Child] {
        let named = |error: String| format!("{role:?}: {error}");
        late_fallback_lands_before_cases(active, role).map_err(named)?;
        for software in [true, false] {
            match parse_during_present_is_drawn_then_acknowledged(active, role, software) {
                Err(reason) if !software && reason.starts_with("HOST_INCAPABLE") => {
                    // When: the host cannot create a wgpu renderer, report the capability, not a pass.
                    println!("capability=HOST_INCAPABLE case={role:?}-wgpu reason={reason}");
                }
                result => result.map_err(|error| named(format!("software={software}: {error}")))?,
            }
        }
        stop_during_present_clears_nothing(active, role).map_err(named)?;
        stop_before_collection_keeps_the_set(active, role).map_err(named)?;
        ime_area_is_the_drawn_cursor(active, role).map_err(named)?;
    }
    Ok(())
}

/// Both redraw adapters release the visible parser guards before presentation and acknowledge
/// what a frame drew at the collection after it, on a real renderer.
#[test]
fn windows_both_adapters_release_parser_guards_before_present() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("release-before-present event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
