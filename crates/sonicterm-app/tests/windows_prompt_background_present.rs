#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::{Action, Keymap},
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
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

fn render(app: &mut App, active: &ActiveEventLoop, id: WindowId) {
    assert!(app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1)));
    ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
}

fn sample(
    app: &App,
    id: WindowId,
    window: &Window,
    x: u32,
    y: u32,
) -> Result<([u8; 4], u32), String> {
    let cpu = app
        .__test_window_software_frame_pixel_bgra(id, x, y)
        .ok_or_else(|| String::from("software pixel unavailable"))?;
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("test window is not Win32"));
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let dc =
        // SAFETY: `hwnd` belongs to the live test window; this DC is released below.
        unsafe { GetDC(Some(hwnd)) };
    if dc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let pixel =
        // SAFETY: `dc` is live and these scalar coordinates lie inside the test client area.
        unsafe { GetPixel(dc, x as i32, y as i32) }.0;
    let _ =
        // SAFETY: `dc` is released once to the same live window that supplied it.
        unsafe { ReleaseDC(Some(hwnd), dc) };
    if pixel == CLR_INVALID {
        return Err(String::from("GetPixel returned CLR_INVALID"));
    }
    Ok((cpu, pixel))
}

fn run_probe(active: &ActiveEventLoop) -> Result<(), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(640, 300))
                    .with_title("SonicTerm prompt projection regression"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
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
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "prompt-projection-test",
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let pane = app.__test_seed_tab("history");
    let id = app.__test_main_window_id().unwrap();
    let mut input = Vec::from(b"\x1b[?25l".as_slice());
    for row in 0..48 {
        input.extend_from_slice(
            format!(
                "\x1b]133;A\x07\x1b[48;2;{};40;80mrow {row:02} XXXXXXXXXXXXXXXX        \x1b[0m\r\n",
                32 + row * 3,
            )
            .as_bytes(),
        );
    }
    assert!(app.__test_advance_pane_parser(pane, &input));
    assert!(app.__test_attach_window_renderer(id, window.clone(), renderer));
    render(&mut app, active, id);
    let (cw, ch, top) = app.__test_window_cell_geometry(id).unwrap();
    let x = (cw * 24.5).round() as u32;
    let row0 = (top + ch * 0.5).round() as u32;
    let row1 = (top + ch * 1.5).round() as u32;
    let baseline = sample(&app, id, &window, x, row0)?;
    let second = sample(&app, id, &window, x, row1)?;
    if baseline == second {
        return Err(String::from("colored history fixture did not distinguish adjacent rows"));
    }

    assert!(app.run_action(&Action::ScrollToPrevPrompt));
    let previous = app.__test_pane_viewport_top_abs(pane).flatten().unwrap();
    render(&mut app, active, id);
    let moved = sample(&app, id, &window, x, row1)?;
    if moved != baseline {
        return Err(format!(
            "prompt navigation left stale background: baseline={baseline:?}, moved={moved:?}"
        ));
    }
    render(&mut app, active, id);
    if sample(&app, id, &window, x, row1)? != moved {
        return Err(String::from("idle presentation changed the moved row"));
    }
    assert!(app.run_action(&Action::ScrollToNextPrompt));
    assert_eq!(app.__test_pane_viewport_top_abs(pane).flatten(), Some(previous + 1));
    render(&mut app, active, id);
    if sample(&app, id, &window, x, row0)? != baseline {
        return Err(String::from("next-prompt navigation did not restore background placement"));
    }
    Ok(())
}

/// A stopped-output prompt move must present the same colored row at its new viewport slot through real GDI.
#[test]
fn windows_prompt_navigation_keeps_projected_background_with_its_row() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("prompt projection event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
