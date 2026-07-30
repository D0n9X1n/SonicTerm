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
    event::WindowEvent,
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
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(320, 180))
                    .with_visible(true)
                    .with_title("SonicTerm selection presentation regression"),
            )
            .map_err(|error| format!("window creation failed: {error}"))?,
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

    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &theme,
        RendererSettings {
            font_family: &config.font.family,
            font_size: config.font.size,
            line_height_mult: config.font.line_height,
            font_weight_scale: config.font.effective_weight_scale(),
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: config.appearance.backdrop,
                opacity: 1.0,
                scrollbar: config.appearance.scrollbar,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "selection-present-regression",
        },
    )
    .map_err(|error| format!("renderer creation failed: {error}"))?;
    if !renderer.is_software_render_degraded() {
        return Err(String::from("forced software-render mode did not engage"));
    }
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);

    let mut app = App::new(theme, config, Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    if !app.__test_advance_pane_parser(pane_id, b"\x1b[?1049h") {
        return Err(String::from("failed to enter alternate screen"));
    }
    {
        let state = app.main_mut().ok_or_else(|| String::from("missing synthetic main"))?;
        state.window = Some(window.clone());
        state.renderer = Some(renderer);
    }

    render_main(&mut app, active, window.id());
    let (sample_x, sample_y) = sample_cell(&app)?;
    let reference_cpu = cpu_pixel(&app, sample_x, sample_y)?;
    let hwnd = hwnd_for(&window)?;
    let reference_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;

    let mut selection = Selection::new(1, 1);
    selection.extend(1, 2);
    selection.anchored = true;
    if !app.__test_set_main_selection(Some(selection)) {
        return Err(String::from("failed to install selection"));
    }
    render_main(&mut app, active, window.id());
    let selected_cpu = cpu_pixel(&app, sample_x, sample_y)?;
    let selected_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if selected_cpu == reference_cpu || selected_hwnd == reference_hwnd {
        return Err(String::from("selection fixture did not change both CPU and HWND pixels"));
    }

    if !app.__test_advance_pane_parser(pane_id, b"\x1b[S") {
        return Err(String::from("failed to apply alternate-screen scroll"));
    }
    render_main(&mut app, active, window.id());
    if app.main_selection().and_then(Option::as_ref).is_some() {
        return Err(String::from("production redraw did not clear intersecting selection"));
    }
    let deselected_cpu = cpu_pixel(&app, sample_x, sample_y)?;
    let deselected_hwnd = hwnd_pixel(hwnd, sample_x as i32, sample_y as i32)?;
    if deselected_cpu != reference_cpu || deselected_hwnd != reference_hwnd {
        return Err(format!(
            "first deselected frame retained pixels: cpu reference={reference_cpu:?} \
             selected={selected_cpu:?} deselected={deselected_cpu:?}; hwnd \
             reference={reference_hwnd:#010x} selected={selected_hwnd:#010x} \
             deselected={deselected_hwnd:#010x}"
        ));
    }
    Ok(())
}

fn render_main(app: &mut App, active: &ActiveEventLoop, window_id: WindowId) {
    app.main_mut().expect("main state").last_render = Instant::now() - Duration::from_secs(1);
    ApplicationHandler::window_event(app, active, window_id, WindowEvent::RedrawRequested);
}

fn sample_cell(app: &App) -> Result<(u32, u32), String> {
    let renderer = app.main_renderer().ok_or_else(|| String::from("missing renderer"))?;
    let (cell_w, cell_h) = renderer.cell_size();
    Ok(((cell_w * 1.5).round() as u32, (renderer.top_inset() + cell_h * 1.5).round() as u32))
}

fn cpu_pixel(app: &App, x: u32, y: u32) -> Result<[u8; 4], String> {
    app.main_renderer()
        .and_then(|renderer| renderer.__test_software_frame_pixel_bgra(x, y))
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
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let observed = unsafe { GetPixel(hdc, x, y) }.0;
    let _ = unsafe { ReleaseDC(Some(hwnd), hdc) };
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
