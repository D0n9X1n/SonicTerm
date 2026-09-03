#![cfg(target_os = "windows")]

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
    keymap::{Action, Keymap},
    theme::{Hex, Theme},
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

const BACKGROUND_HEX: &str = "#123456";
const FLASH_EXPIRY_WAIT: Duration = Duration::from_millis(400);

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
                    .with_title(format!("SonicTerm {label} focus flash presentation regression")),
            )
            .map_err(|error| format!("{label}: window creation failed: {error}"))?,
    );
    let mut theme = Theme::default();
    theme.colors.background = Hex(BACKGROUND_HEX.to_string());
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.window.padding_left = 0.0;
    config.window.padding_right = 0.0;
    config.window.padding_top = 0.0;
    config.window.padding_bottom = 0.0;

    let mut renderer = make_renderer(window.clone(), active, &theme, &config, label)?;
    renderer.set_cursor_blink(false);

    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let (window_id, target_pane) = if main {
        let pane_id = app.__test_seed_tab("main");
        (app.__test_main_window_id().expect("synthetic main"), pane_id)
    } else {
        let window_id = app.__test_seed_child_window(&["child"]);
        let pane_id = app.__test_child_active_pane(window_id).expect("synthetic child pane");
        (window_id, pane_id)
    };
    if !app.__test_attach_window_renderer(window_id, window.clone(), renderer) {
        return Err(format!("{label}: failed to attach real window and renderer"));
    }
    if !app.run_action(&Action::ToggleTabBar) || app.tab_bar_visible() {
        return Err(format!("{label}: failed to hide tab-bar chrome through production action"));
    }
    if main {
        app.__test_split_active_right();
    } else if !app.__test_child_split_active_right(window_id) {
        return Err(format!("{label}: failed to split child pane"));
    }
    let active_after_split = active_pane(&app, main, window_id)?;
    if active_after_split == target_pane {
        return Err(format!("{label}: split did not focus the new right pane"));
    }

    thread::sleep(FLASH_EXPIRY_WAIT);
    render_window(&mut app, active, window_id);
    if app.__test_window_pane_focus_flash_target(window_id).is_some() {
        return Err(format!("{label}: split focus flash did not expire before baseline"));
    }

    let (sample_x, sample_y) = inactive_left_sample(&app, &window, window_id)?;
    let reference_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let hwnd = hwnd_for(&window)?;
    let reference_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;

    pointer_move(&mut app, active, window_id, sample_x, sample_y);
    pointer_button(&mut app, active, window_id, ElementState::Pressed);
    if active_pane(&app, main, window_id)? != target_pane {
        return Err(format!("{label}: click did not focus the inactive left pane"));
    }
    if app.__test_window_pane_focus_flash_target(window_id) != Some(target_pane) {
        return Err(format!("{label}: click did not arm focus feedback for the target pane"));
    }
    let selection = app
        .__test_window_selection(window_id)
        .flatten()
        .ok_or_else(|| format!("{label}: click did not create destination selection state"))?;
    if selection.pane_id != Some(target_pane) {
        return Err(format!(
            "{label}: click selection belongs to {:?}, expected target pane {target_pane}",
            selection.pane_id
        ));
    }

    render_window(&mut app, active, window_id);
    let flashed_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let flashed_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if flashed_cpu == reference_cpu || flashed_hwnd == reference_hwnd {
        return Err(format!(
            "{label}: focus flash did not change both presented pixels: cpu baseline={reference_cpu:?} \
             flashed={flashed_cpu:?}; hwnd baseline={reference_hwnd:#010x} \
             flashed={flashed_hwnd:#010x}"
        ));
    }

    pointer_button(&mut app, active, window_id, ElementState::Released);
    thread::sleep(FLASH_EXPIRY_WAIT);
    render_window(&mut app, active, window_id);
    if app.__test_window_pane_focus_flash_target(window_id).is_some() {
        return Err(format!("{label}: click focus flash remained armed after its bound"));
    }
    let expired_cpu = cpu_pixel(&app, window_id, sample_x, sample_y)?;
    let expired_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if expired_cpu != reference_cpu || expired_hwnd != reference_hwnd {
        return Err(format!(
            "{label}: expired focus frame did not return to baseline: cpu baseline={reference_cpu:?} \
             flashed={flashed_cpu:?} expired={expired_cpu:?}; hwnd baseline={reference_hwnd:#010x} \
             flashed={flashed_hwnd:#010x} expired={expired_hwnd:#010x}"
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

fn active_pane(app: &App, main: bool, window_id: WindowId) -> Result<u64, String> {
    if main { app.__test_active_pane_in_tab(0) } else { app.__test_child_active_pane(window_id) }
        .ok_or_else(|| String::from("active pane unavailable"))
}

fn inactive_left_sample(
    app: &App,
    window: &Window,
    window_id: WindowId,
) -> Result<(u32, u32), String> {
    let (_, cell_h, top_inset) = app
        .__test_window_cell_geometry(window_id)
        .ok_or_else(|| String::from("missing renderer geometry"))?;
    let size = window.inner_size();
    let x = size.width / 4;
    let pane_height = (size.height as f32 - top_inset).max(cell_h);
    let y = (top_inset + pane_height * 0.5).round() as u32;
    Ok((x, y.min(size.height.saturating_sub(1))))
}

fn pointer_move(app: &mut App, active: &ActiveEventLoop, window_id: WindowId, x: u32, y: u32) {
    ApplicationHandler::window_event(
        app,
        active,
        window_id,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(f64::from(x), f64::from(y)),
        },
    );
}

fn pointer_button(
    app: &mut App,
    active: &ActiveEventLoop,
    window_id: WindowId,
    state: ElementState,
) {
    ApplicationHandler::window_event(
        app,
        active,
        window_id,
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
    );
}

fn render_window(app: &mut App, active: &ActiveEventLoop, window_id: WindowId) {
    assert!(app.__test_set_window_last_render(window_id, Instant::now() - Duration::from_secs(1)));
    ApplicationHandler::window_event(app, active, window_id, WindowEvent::RedrawRequested);
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
        // SAFETY: `hdc` is live and `x`/`y` are sample coordinates inside the test surface.
        unsafe { GetPixel(hdc, x, y) }.0;
    let _ =
        // SAFETY: `hdc` came from `GetDC(Some(hwnd))` and is released exactly once to the same window.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    if observed == CLR_INVALID {
        return Err(String::from("GetPixel returned CLR_INVALID"));
    }
    Ok(observed)
}

/// Mouse focus feedback reaches both the CPU frame and HWND for main and child splits.
#[test]
fn click_focus_flash_presents_in_windows_software_mode() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop available");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("event loop runs");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
