#![cfg(target_os = "windows")]

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
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
    dpi::{PhysicalPosition, PhysicalSize},
    event::{DeviceId, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::ModifiersState,
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

struct Probe;

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        for main in [true, false] {
            exercise(active, main);
        }
        active.exit();
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn render(app: &mut App, active: &ActiveEventLoop, id: WindowId) {
    let started = Instant::now();
    loop {
        app.__test_set_window_last_render(id, started - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if app.__test_window_last_render(id).is_some_and(|time| time >= started) {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(3), "preview frame was not presented");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pixels(app: &App, id: WindowId) -> Vec<[u8; 4]> {
    (60..115)
        .flat_map(|y| (25..250).map(move |x| (x, y)))
        .map(|(x, y)| app.__test_window_software_frame_pixel_bgra(id, x, y).unwrap())
        .collect()
}

fn exercise(active: &ActiveEventLoop, main: bool) {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 360))
                    .with_visible(true)
                    .with_title("SonicTerm link preview regression"),
            )
            .unwrap(),
    );
    let theme = Theme::default();
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
            role: "link-preview-test",
        },
    )
    .unwrap();
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    let (cw, ch) = renderer.cell_size();
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let (id, pane) = if main {
        let pane = app.__test_seed_tab("preview");
        (app.__test_main_window_id().unwrap(), pane)
    } else {
        let id = app.__test_seed_child_window(&["preview"]);
        (id, app.__test_child_active_pane(id).unwrap())
    };
    assert!(app.__test_attach_window_renderer(id, window, renderer));
    app.__test_set_frontmost_window(Some(id));
    let put = |app: &mut App, uri: &str, label: &str| {
        let bytes = format!("\x1b[H\x1b[2K\x1b]8;;{uri}\x1b\\{label}\x1b]8;;\x1b\\");
        assert!(app.__test_advance_child_pane_parser(id, pane, bytes.as_bytes()));
    };
    put(&mut app, "https://example.com/docs", "Docs");
    render(&mut app, active, id);
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new((cw * 1.5) as f64, (ch * 0.5) as f64),
        },
    );
    render(&mut app, active, id);
    let baseline = pixels(&app, id);
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
    );
    render(&mut app, active, id);
    let shown = pixels(&app, id);
    assert_ne!(shown, baseline, "stationary Ctrl must paint the labeled destination");
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
    );
    render(&mut app, active, id);
    assert_eq!(pixels(&app, id), baseline, "modifier release restores covered terminal pixels");
    put(&mut app, "https://example.com/", "https://example.com/");
    render(&mut app, active, id);
    let matching = pixels(&app, id);
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
    );
    render(&mut app, active, id);
    assert_ne!(pixels(&app, id), matching, "an exact URL label still shows the destination");
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
    );
    assert!(app.__test_advance_child_pane_parser(
        id,
        pane,
        b"\x1b[H\x1b[2Khttps://example.com/?a=1&b=2"
    ));
    render(&mut app, active, id);
    let plain = pixels(&app, id);
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
    );
    render(&mut app, active, id);
    assert_ne!(pixels(&app, id), plain, "plain URL queries show a destination too");
    put(&mut app, "https://example.com/docs", "Docs");
    render(&mut app, active, id);
    assert_ne!(pixels(&app, id), baseline, "PTY replacement refreshes a stationary preview");
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::CursorLeft { device_id: DeviceId::dummy() },
    );
    render(&mut app, active, id);
    assert_eq!(pixels(&app, id), baseline, "departure must not resurrect preview on redraw");

    // Only the space's underline attribute changes between these three presentations.
    let space_pixels = |app: &App| {
        let x = (cw * 1.5) as u32;
        (0..ch.ceil() as u32)
            .map(|y| app.__test_window_software_frame_pixel_bgra(id, x, y).unwrap())
            .collect::<Vec<_>>()
    };
    let broken = b"\x1b[H\x1b[2K\x1b[4mA\x1b[24m \x1b[4mB\x1b[0m";
    assert!(app.__test_advance_child_pane_parser(id, pane, broken));
    render(&mut app, active, id);
    let gap = space_pixels(&app);
    assert!(app.__test_advance_child_pane_parser(id, pane, b"\x1b[H\x1b[2K\x1b[4mA B\x1b[0m"));
    render(&mut app, active, id);
    assert_ne!(space_pixels(&app), gap, "styled space must contain underline pixels");
    assert!(app.__test_advance_child_pane_parser(id, pane, broken));
    render(&mut app, active, id);
    assert_eq!(space_pixels(&app), gap, "unstyled space restores the exact gap");
    // Move the block cursor off the measured space after erasure.
    assert!(app.__test_advance_child_pane_parser(
        id,
        pane,
        b"\x1b[H\x1b[4mA\x1b[0K\x1b[0m\x1b[1;4H"
    ));
    render(&mut app, active, id);
    assert_eq!(space_pixels(&app), gap, "erase under active SGR must not leave underline ink");
}

/// Real main and child App event paths present and erase the tooltip through the Windows CPU renderer.
#[test]
fn labeled_link_preview_presents_and_clears() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    event_loop.run_app(&mut Probe).unwrap();
}
