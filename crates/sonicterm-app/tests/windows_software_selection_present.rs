#![cfg(target_os = "windows")]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
    keymap::Keymap,
    theme::{Hex, Theme},
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_ui::selection::Selection;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

const BACKGROUND_HEX: &str = "#123456";
const SELECTION_HEX: &str = "#e04020";

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        self.outcome = Some(run_probe(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn run_probe(active: &ActiveEventLoop) -> Result<(), String> {
    run_window_case(active, true)?;
    run_window_case(active, false)
}

fn run_window_case(active: &ActiveEventLoop, main: bool) -> Result<(), String> {
    let label = if main { "main" } else { "child" };
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(320, 180))
                    .with_visible(true)
                    .with_title(format!("SonicTerm {label} selection presentation regression")),
            )
            .map_err(|error| format!("{label}: window creation failed: {error}"))?,
    );
    let mut theme = Theme::default();
    theme.colors.background = Hex(BACKGROUND_HEX.to_string());
    theme.colors.selection_bg = Hex(SELECTION_HEX.to_string());
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.window.padding_left = 0.0;
    config.window.padding_right = 0.0;
    config.window.padding_top = 0.0;
    config.window.padding_bottom = 0.0;

    let mut renderer = make_renderer(window.clone(), active, &theme, &config, label)?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);

    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let (window_id, pane_id) = if main {
        let pane_id = app.__test_seed_tab("main");
        (app.__test_main_window_id().expect("synthetic main"), pane_id)
    } else {
        let window_id = app.__test_seed_child_window(&["child"]);
        let pane_id = app.__test_child_active_pane(window_id).expect("synthetic child pane");
        (window_id, pane_id)
    };
    advance_pane(&app, main, window_id, pane_id, b"\x1b[?1049h")?;
    if !app.__test_attach_window_renderer(window_id, window.clone(), renderer) {
        return Err(format!("{label}: failed to attach real window and renderer"));
    }

    render_window(&mut app, active, window_id, main);
    let (sample_x, sample_y) = sample_cell(&app, window_id)?;
    let reference_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let hwnd = hwnd_for(&window)?;
    let reference_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;

    let mut selection = Selection::new(1, 1);
    selection.extend(1, 2);
    selection.anchored = true;
    let selected = if main {
        app.__test_set_main_selection(Some(selection))
    } else {
        app.__test_set_child_selection(window_id, Some(selection))
    };
    if !selected {
        return Err(format!("{label}: failed to install selection"));
    }
    render_window(&mut app, active, window_id, main);
    let selected_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let selected_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if selected_cpu == reference_cpu || selected_hwnd == reference_hwnd {
        return Err(format!("{label}: selection fixture did not change both CPU and HWND pixels"));
    }

    ApplicationHandler::window_event(
        &mut app,
        active,
        window_id,
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: TouchPhase::Moved,
        },
    );
    if current_selection(&app, main, window_id).is_none() {
        return Err(format!("{label}: wheel input cleared selection before content changed"));
    }
    // Model the redraw winit may coalesce before the TUI reply. On software
    // presentation this must retain the selection because no row changed.
    ApplicationHandler::window_event(&mut app, active, window_id, WindowEvent::RedrawRequested);
    let deferred = if main {
        app.__test_main_redraw_deferred()
    } else {
        app.__test_child_redraw_deferred(window_id)
    };
    if !deferred {
        return Err(format!("{label}: software wheel redraw was not deferred to the frame cap"));
    }
    let unchanged_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if unchanged_hwnd != selected_hwnd {
        return Err(format!("{label}: unchanged wheel redraw altered selection pixels"));
    }

    advance_pane(&app, main, window_id, pane_id, b"\x1b[S")?;
    let now = Instant::now();
    if !app.__test_set_window_last_render(window_id, now - Duration::from_millis(30)) {
        return Err(format!("{label}: failed to advance the frame clock"));
    }
    ApplicationHandler::new_events(
        &mut app,
        active,
        winit::event::StartCause::ResumeTimeReached {
            start: now - Duration::from_millis(30),
            requested_resume: now,
        },
    );
    ApplicationHandler::window_event(&mut app, active, window_id, WindowEvent::RedrawRequested);
    if current_selection(&app, main, window_id).is_some() {
        return Err(format!("{label}: production redraw did not clear intersecting selection"));
    }
    let deselected_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let deselected_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if deselected_cpu != reference_cpu || deselected_hwnd != reference_hwnd {
        return Err(format!(
            "{label}: first deselected frame retained pixels: cpu reference={reference_cpu:?} \
             selected={selected_cpu:?} deselected={deselected_cpu:?}; hwnd \
             reference={reference_hwnd:#010x} selected={selected_hwnd:#010x} \
             deselected={deselected_hwnd:#010x}"
        ));
    }
    Ok(())
}

fn make_renderer(
    window: Arc<Window>,
    active: &ActiveEventLoop,
    theme: &Theme,
    config: &Config,
    role: &'static str,
) -> Result<GpuRenderer, String> {
    let renderer = GpuRenderer::new(
        window,
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
            role,
        },
    )
    .map_err(|error| format!("{role}: renderer creation failed: {error}"))?;
    if !renderer.is_software_render_degraded() {
        return Err(format!("{role}: forced software-render mode did not engage"));
    }
    Ok(renderer)
}

fn advance_pane(
    app: &App,
    main: bool,
    window_id: WindowId,
    pane_id: u64,
    bytes: &[u8],
) -> Result<(), String> {
    let advanced = if main {
        app.__test_advance_pane_parser(pane_id, bytes)
    } else {
        app.__test_advance_child_pane_parser(window_id, pane_id, bytes)
    };
    advanced.then_some(()).ok_or_else(|| String::from("failed to advance pane parser"))
}

fn render_window(app: &mut App, active: &ActiveEventLoop, window_id: WindowId, _: bool) {
    assert!(app.__test_set_window_last_render(window_id, Instant::now() - Duration::from_secs(1)));
    ApplicationHandler::window_event(app, active, window_id, WindowEvent::RedrawRequested);
}

fn current_selection(app: &App, main: bool, window_id: WindowId) -> Option<Selection> {
    if main {
        app.main_selection().copied().flatten()
    } else {
        app.__test_window_selection(window_id).flatten()
    }
}

fn sample_cell(app: &App, window_id: WindowId) -> Result<(u32, u32), String> {
    let (cell_w, cell_h, top_inset) = app
        .__test_window_cell_geometry(window_id)
        .ok_or_else(|| String::from("missing renderer geometry"))?;
    Ok(((cell_w * 1.5).round() as u32, (top_inset + cell_h * 1.5).round() as u32))
}

fn cpu_pixel(app: &App, window_id: WindowId, x: u32, y: u32) -> Result<[u8; 4], String> {
    app.__test_window_software_frame_pixel_bgra(window_id, x, y)
        .ok_or_else(|| String::from("software frame pixel unavailable"))
}

fn hwnd_for(window: &Window) -> Result<HWND, String> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("window did not expose a Win32 handle"));
    };
    Ok(HWND(handle.hwnd.get() as *mut _))
}

fn hwnd_pixel(hwnd: HWND, x: i32, y: i32) -> Result<u32, String> {
    let hdc =
        // SAFETY: `hwnd` is a live test window; `GetDC` returns a borrowed DC paired with `ReleaseDC` below.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let observed =
        // SAFETY: `hdc` is live and `x`/`y` are scalar sample coordinates inside the test surface.
        unsafe { GetPixel(hdc, x, y) }.0;
    let _ =
        // SAFETY: `hdc` came from `GetDC(Some(hwnd))` above and is released exactly once to the same window.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    if observed == CLR_INVALID {
        return Err(String::from("GetPixel returned CLR_INVALID"));
    }
    Ok(observed)
}

#[test]
fn first_windows_software_frame_removes_invalidated_alt_selection() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop available");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("event loop runs");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
