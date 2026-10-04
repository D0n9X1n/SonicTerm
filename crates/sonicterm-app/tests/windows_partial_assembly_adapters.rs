//! Partial assembly through both production redraw adapters on a real wgpu renderer: a one-row
//! edit in the child window, and in the main window as a control, presents one `Partial` frame
//! through the App's redraw, its receipt clears the row at the next collection, and the retained
//! frame equals a full repaint of the same state.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_gpu::core::{unpad_readback_rows, GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
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

/// One window on a real counting wgpu renderer, with the visible pane the case edits.
struct Fixture {
    app: App,
    id: WindowId,
    pane: u64,
}

/// Whether this host enumerates no wgpu adapter at all: the only reason to skip.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// A window of `role` with a counting wgpu renderer whose retained frame can be read back, no
/// scrollbar, no padding and a steady cursor; `Err` names a host with no adapter `HOST_INCAPABLE`.
fn fixture(active: &ActiveEventLoop, role: Role) -> Result<Fixture, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(480, 320))
                    .with_visible(true)
                    .with_title("SonicTerm partial assembly"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Off;
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
                software_render_mode: SoftwareRenderMode::Off,
            },
            role: "partial-assembly-adapters",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    );
    let mut renderer = match created {
        Ok(renderer) => renderer,
        Err(error) if host_has_no_adapter() => {
            // When: host_has_no_adapter holds, no wgpu renderer can exist on this host.
            return Err(format!("HOST_INCAPABLE: no wgpu adapter: {error}"));
        }
        Err(error) => return Err(format!("wgpu renderer construction failed: {error}")),
    };
    renderer.set_cursor_blink(false);
    renderer.set_frame_counting(true);
    renderer.__enable_retained_frame_readback();
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(false);
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
    Ok(Fixture { app, id, pane })
}

/// Dispatch one real `RedrawRequested` with pacing open, whatever the frame does.
fn dispatch(fixture: &mut Fixture, active: &ActiveEventLoop) {
    fixture.app.__test_set_window_last_render(fixture.id, Instant::now() - Duration::from_secs(1));
    ApplicationHandler::window_event(
        &mut fixture.app,
        active,
        fixture.id,
        WindowEvent::RedrawRequested,
    );
}

/// Dispatch `RedrawRequested` until the renderer counts one more presented frame.
fn present(fixture: &mut Fixture, active: &ActiveEventLoop) -> Result<(), String> {
    let started = Instant::now();
    let presented = fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0);
    loop {
        dispatch(fixture, active);
        if fixture.app.__test_window_successful_frames(fixture.id).unwrap_or(0) > presented {
            // When: the successful-presentation count moved, a frame was presented.
            return Ok(());
        }
        check(started.elapsed() < Duration::from_secs(3), "the frame was presented")?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Write `bytes` to the fixture's visible pane through its parser.
fn write(fixture: &mut Fixture, bytes: &[u8]) -> Result<(), String> {
    check(
        fixture.app.__test_advance_child_pane_parser(fixture.id, fixture.pane, bytes),
        "the visible pane accepted the bytes",
    )
}

/// The renderer of the fixture's window.
fn renderer(fixture: &mut Fixture) -> Result<&mut GpuRenderer, String> {
    fixture.app.__test_window_renderer_mut(fixture.id).ok_or_else(|| "no renderer".into())
}

/// The retained GPU frame's tightly packed BGRA pixels. This test maps and polls; the renderer
/// never does.
fn retained_pixels(fixture: &mut Fixture) -> Result<Vec<u8>, String> {
    let readback = renderer(fixture)?
        .__copy_retained_frame()
        .ok_or("readback is enabled and the device works")?;
    let slice = readback.buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    readback
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll the retained-frame readback: {error}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|error| format!("map the retained-frame readback: {error}"))?;
    let pixels = unpad_readback_rows(
        &mapped,
        readback.width_px,
        readback.height_px,
        readback.padded_row_bytes,
    );
    drop(mapped);
    readback.buffer.unmap();
    Ok(pixels)
}

/// Test 10 (child) and its main-window control: dense rows and a hidden cursor are drawn and
/// acknowledged; a one-row edit then presents exactly one `Partial` frame through the adapter,
/// hashing fewer cells than a full frame did; its receipt clears the edited row at the next
/// collection; the retained frame equals a full repaint of the same state; and the partial frame
/// uploads fewer vertex and index bytes than that full repaint does.
fn partial_edit_through_the_adapter(active: &ActiveEventLoop, role: Role) -> Result<(), String> {
    let mut fixture = fixture(active, role)?;
    write(&mut fixture, b"\x1b[?25l")?;
    for row in 1..=8 {
        write(&mut fixture, format!("\x1b[{row};1HMWMWMWMWMWMWMWMW").as_bytes())?;
    }
    write(&mut fixture, b"\x1b[5;1H")?;
    present(&mut fixture, active)?;
    let full = renderer(&mut fixture)?.frame_stats();
    // The next collection applies the first frame's receipts; the unchanged key presents nothing.
    dispatch(&mut fixture, active);
    let rows = fixture.app.__test_window_pane_dirty_rows(fixture.id, fixture.pane);
    check(rows.as_deref() == Some(&[][..]), &format!("the drawn dirt is cleared: {rows:?}"))?;

    write(&mut fixture, b"edit")?;
    let before = renderer(&mut fixture)?.frame_stats();
    present(&mut fixture, active)?;
    let after = renderer(&mut fixture)?.frame_stats();
    let hashed = after.row_cells_hashed - before.row_cells_hashed;
    // The stats are cumulative, so one frame's upload is the difference across it.
    let uploaded =
        |stats: &sonicterm_gpu::frame_stats::FrameStats| stats.vertex_bytes + stats.index_bytes;
    let partial_upload = uploaded(&after) - uploaded(&before);
    check(
        after.partial_frames - before.partial_frames == 1
            && after.full_frames == before.full_frames
            && hashed > 0
            && hashed < full.row_cells_hashed,
        &format!(
            "one partial frame hashing {hashed} of a full frame's {} cells: {before:?} -> {after:?}",
            full.row_cells_hashed
        ),
    )?;
    check(
        fixture.app.__test_window_pending_receipts(fixture.id) == Some(1),
        "the partial frame's receipt waits for the next collection",
    )?;
    let narrow = retained_pixels(&mut fixture)?;
    dispatch(&mut fixture, active);
    let rows = fixture.app.__test_window_pane_dirty_rows(fixture.id, fixture.pane);
    check(rows.as_deref() == Some(&[][..]), &format!("the edited row is acknowledged: {rows:?}"))?;

    renderer(&mut fixture)?.invalidate_retained_frame();
    let before = renderer(&mut fixture)?.frame_stats();
    present(&mut fixture, active)?;
    let after = renderer(&mut fixture)?.frame_stats();
    check(
        after.full_frames - before.full_frames == 1,
        &format!("the comparison frame is Full: {before:?} -> {after:?}"),
    )?;
    let full_upload = uploaded(&after) - uploaded(&before);
    check(
        partial_upload > 0 && partial_upload < full_upload,
        &format!(
            "the partial frame uploads {partial_upload} bytes, less than the full repaint's \
             {full_upload}"
        ),
    )?;
    let full_pixels = retained_pixels(&mut fixture)?;
    check(narrow == full_pixels, "the partial frame equals a full repaint of the same state")
}

fn run(active: &ActiveEventLoop) -> Result<(), String> {
    for role in [Role::Child, Role::Main] {
        match partial_edit_through_the_adapter(active, role) {
            Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
                // When: the host cannot create a wgpu renderer, report the capability, not a pass.
                println!("capability=HOST_INCAPABLE case={role:?} reason={reason}");
            }
            result => result.map_err(|error| format!("{role:?}: {error}"))?,
        }
    }
    Ok(())
}

/// A one-row edit presents a partial frame through both production redraw adapters, child first,
/// and equals a full repaint, on a real wgpu renderer.
#[cfg(target_os = "windows")]
#[test]
fn windows_both_adapters_present_a_partial_edit() {
    use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("partial assembly adapters event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
