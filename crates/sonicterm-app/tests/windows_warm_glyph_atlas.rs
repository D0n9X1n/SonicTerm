#![cfg(target_os = "windows")]
//! A warm-pool renderer on a real window starts its glyph atlas at the 256 floor, keeps it there
//! through adoption by a tear-out, and grows it on demand once the adopted window draws.

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_text::glyph_atlas::MIN_ATLAS_DIM;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::WindowId,
};

/// Body size large enough that the visible cells of four style faces outgrow a 256 atlas.
const FONT_PX: f32 = 48.0;

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so the case runs inside this one.
        self.outcome = Some(warm_atlas_grows_after_adoption(active));
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

/// Printable ASCII in each of the four style faces, one SGR run per face, as a pty delivers it.
fn four_face_ascii() -> Vec<u8> {
    let printable: String = ('!'..='~').collect();
    ["0", "1", "3", "1;3"]
        .iter()
        .map(|sgr| format!("\x1b[{sgr}m{printable}\x1b[0m\r\n"))
        .collect::<String>()
        .into_bytes()
}

/// The glyph atlas dimension of window `id`'s live renderer.
fn atlas_dim(app: &mut App, id: WindowId) -> Option<u32> {
    app.__test_window_renderer_mut(id).map(|renderer| renderer.glyph_atlas_facts().dim)
}

/// Test 9, end to end: the warm renderer reports the 256 floor; a tear-out adopts that window,
/// which keeps the floor until it draws; drawing the torn tab's text grows the atlas.
fn warm_atlas_grows_after_adoption(active: &ActiveEventLoop) -> Result<(), String> {
    let mut config = Config::default();
    config.font.size = FONT_PX;
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.terminal.cursor_blink = false;
    config.locale = "en".into();
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_seed_tab("main");
    let torn = app.__test_seed_tab("torn");
    check(app.__test_advance_pane_parser(torn, &four_face_ascii()), "the torn tab holds text")?;

    let warm = app.__test_prewarm_window(active).ok_or("the warm window was built")?;
    check(
        app.__test_warm_glyph_atlas_dim(warm) == Some(MIN_ATLAS_DIM),
        "the warm renderer starts its atlas at the 256 floor",
    )?;

    check(app.__test_tear_out_tab(active, 1), "the tear-out ran")?;
    check(app.__test_warm_glyph_atlas_dim(warm).is_none(), "the warm window left the pool")?;
    check(
        atlas_dim(&mut app, warm) == Some(MIN_ATLAS_DIM),
        "the adopted window keeps the floor until it draws",
    )?;

    let started = Instant::now();
    loop {
        app.__test_set_window_last_render(warm, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(&mut app, active, warm, WindowEvent::RedrawRequested);
        let presented = app.__test_window_successful_frames(warm).unwrap_or(0) > 0;
        if presented && atlas_dim(&mut app, warm).is_some_and(|dim| dim > MIN_ATLAS_DIM) {
            // When: a frame presented from a grown atlas, the adopted renderer grew on demand.
            break;
        }
        check(
            started.elapsed() < Duration::from_secs(3),
            &format!("the adopted atlas grew: dim {:?}", atlas_dim(&mut app, warm)),
        )?;
        std::thread::sleep(Duration::from_millis(10));
    }
    let growths = app
        .__test_window_renderer_mut(warm)
        .map_or(0, |renderer| renderer.glyph_atlas_facts().growths);
    check(growths >= 1, "the growth is counted on the adopted renderer")
}

/// A warm-pool renderer starts at the atlas floor and grows only after adoption and drawing.
#[test]
fn windows_warm_renderer_atlas_grows_after_adoption() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("warm glyph atlas event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
