#![cfg(target_os = "windows")]
//! The idle image-atlas release driven through the app on a real renderer, on the hosted runner's WARP
//! device: the deadline wake releases without a frame, the aggregate memory reading moves only at the
//! next retention sample while the pane's media charge stays put, and a reactivated tab promotes again
//! and draws the pixels of a never-released frame.

use sonicterm_app::app::{retention::RETENTION_SAMPLE_INTERVAL, App};
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::{Action, Keymap},
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_render_model::InlineImage;
use sonicterm_types::ResourceClass;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

/// The interval after which a media-free promoted atlas is released.
const IDLE_INTERVAL: Duration = Duration::from_secs(30);
/// Bytes of the 32x32 BGRA test image.
const IMAGE_BYTES: usize = 32 * 32 * 4;

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so every case runs inside this one.
        self.outcome = Some(run_cases(active));
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

/// One app with a real window and renderer: tab 0 holds a pane showing a 32x32 image at its origin,
/// tab 1 a media-free pane. `mode` picks the presenter: `Off` presents through wgpu, `Force` through GDI.
struct Fixture {
    app: App,
    window: Arc<Window>,
    id: WindowId,
    image_pane: u64,
}

fn fixture(
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
    role: &'static str,
) -> Result<Fixture, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 360))
                    .with_visible(true)
                    .with_title("SonicTerm idle image atlas through the app"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = mode;
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
                software_render_mode: mode,
            },
            role,
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    // The app's pacing flag follows the presenter: GDI under Force, wgpu under Off on WARP.
    app.__test_set_software_render_degrade(matches!(mode, SoftwareRenderMode::Force));
    let image_pane = app.__test_seed_tab("image");
    let plain_pane = app.__test_seed_tab("plain");
    let id = app.__test_main_window_id().ok_or("no main window")?;
    check(app.__test_attach_window_renderer(id, window.clone(), renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    // A hidden cursor keeps the re-shown frame comparable to the first one.
    for pane in [image_pane, plain_pane] {
        check(app.__test_advance_pane_parser(pane, b"\x1b[?25l"), "cursor hidden")?;
    }
    let image = InlineImage {
        id: 1,
        row: 0,
        col: 0,
        width: 32,
        height: 32,
        bgra: Arc::from([0, 0, 255, 255].repeat(32 * 32)),
    };
    check(app.__test_set_pane_inline_images(id, image_pane, vec![image]), "image seeded")?;
    let mut fixture = Fixture { app, window, id, image_pane };
    fixture.activate(active, 0)?;
    Ok(fixture)
}

impl Fixture {
    /// Show tab `index` and present the frame it asks for.
    fn activate(&mut self, active: &ActiveEventLoop, index: usize) -> Result<(), String> {
        self.app.run_action_for_window(&Action::ActivateTab(index), self.id);
        self.render(active)
    }

    /// Present frames until the window is idle, so the release is collectable; at most five, since a
    /// frame that uploads or changes focus may ask for one more.
    fn render(&mut self, active: &ActiveEventLoop) -> Result<(), String> {
        for _ in 0..5 {
            self.present_one(active)?;
            if self.blockers().starts_with("collectable=true") {
                // When: the window has nothing pending or in flight, the frame sequence has settled.
                return Ok(());
            }
        }
        Err(format!("the window never settled: {}", self.blockers()))
    }

    /// Dispatch redraws until one is presented, retrying briefly as the link-preview fixture does, since
    /// a frame can be skipped while the swapchain settles.
    fn present_one(&mut self, active: &ActiveEventLoop) -> Result<(), String> {
        let started = Instant::now();
        loop {
            self.app.__test_set_window_last_render(self.id, started - Duration::from_secs(1));
            ApplicationHandler::window_event(
                &mut self.app,
                active,
                self.id,
                WindowEvent::RedrawRequested,
            );
            if self.app.__test_window_last_render(self.id).is_some_and(|time| time >= started) {
                // When: the window's last render moved past `started`, a frame was presented.
                return Ok(());
            }
            check(started.elapsed() < Duration::from_secs(3), "the frame was presented")?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn blockers(&self) -> String {
        self.app.__test_window_release_blockers(self.id)
    }

    fn dimensions(&self) -> Result<((u32, u32), (u32, u32)), String> {
        self.app
            .__test_window_image_atlas_dimensions(self.id)
            .ok_or_else(|| String::from("no renderer"))
    }

    fn atlas_bytes(&self) -> Result<usize, String> {
        self.app.__test_window_image_atlas_bytes(self.id).ok_or_else(|| String::from("no renderer"))
    }

    fn deadline(&self) -> Result<Instant, String> {
        self.app
            .__test_window_image_atlas_release_deadline(self.id)
            .ok_or_else(|| format!("a media-free frame arms the deadline: {}", self.blockers()))
    }

    fn media_charge(&self) -> Result<sonicterm_types::ResourceAmount, String> {
        self.app
            .__test_pane_charges(self.id, self.image_pane)
            .and_then(|charges| charges.get(&ResourceClass::InlineMediaRetained).copied())
            .ok_or_else(|| String::from("the image pane has no media charge"))
    }

    fn pixels(&self) -> Result<Vec<[u8; 4]>, String> {
        let size = self.window.inner_size();
        (0..size.height)
            .flat_map(|pixel_y| (0..size.width).map(move |pixel_x| (pixel_x, pixel_y)))
            .map(|(pixel_x, pixel_y)| {
                self.app
                    .__test_window_software_frame_pixel_bgra(self.id, pixel_x, pixel_y)
                    .ok_or_else(|| String::from("software frame pixel unavailable"))
            })
            .collect()
    }
}

/// The deadline wake releases the atlas through the app: promote, show a media-free frame, then collect
/// and service at the deadline. The CPU atlas and its GPU mirror become 1x1, the renderer's reading drops
/// at once, no frame is presented and nothing is left pending. A still image never arms a release; the
/// base build kept the 2048x2048 atlas through the same sequence.
fn timer_release(active: &ActiveEventLoop) -> Result<(), String> {
    let mut fixture = fixture(active, SoftwareRenderMode::Off, "idle-atlas-app-timer")?;
    let (cpu, gpu) = fixture.dimensions()?;
    check(cpu != (1, 1) && gpu == cpu, "an image frame promotes the atlas and its GPU mirror")?;
    check(
        fixture.app.__test_window_image_atlas_release_deadline(fixture.id).is_none(),
        "a still image arms no deadline",
    )?;
    check(
        fixture.app.__test_collect_and_service_redraw_due(Instant::now() + IDLE_INTERVAL * 2) == 0,
        "a still image is never collected",
    )?;
    check(fixture.dimensions()?.0 == cpu, "a still image stays promoted")?;

    fixture.activate(active, 1)?;
    let deadline = fixture.deadline()?;
    let promoted_bytes = fixture.atlas_bytes()?;
    let presented = fixture.app.__test_window_last_render(fixture.id);
    fixture.app.__test_collect_and_service_redraw_due(deadline - Duration::from_millis(1));
    check(fixture.dimensions()?.0 == cpu, "nothing is released before the deadline")?;
    check(
        fixture.app.__test_collect_and_service_redraw_due(deadline) == 1,
        &format!("the deadline collects the release: {}", fixture.blockers()),
    )?;
    check(fixture.dimensions()? == ((1, 1), (1, 1)), "the CPU atlas and its GPU mirror are 1x1")?;
    check(
        fixture.atlas_bytes()? == 4 && promoted_bytes > 4,
        "the renderer's reading drops at once",
    )?;
    check(fixture.app.__test_window_last_render(fixture.id) == presented, "no frame is presented")?;
    check(
        fixture.blockers().starts_with("collectable=true") && fixture.deadline().is_err(),
        &format!("no repaint is queued and no deadline is left: {}", fixture.blockers()),
    )
}

/// The aggregate `renderer_total_bytes` keeps its old figure after a release and moves only at the next
/// retention sample, by exactly the released atlas bytes; the pane's `InlineMediaRetained` charge is the
/// same before the release, after it, and after the next sample.
fn aggregate_and_pane_charge(active: &ActiveEventLoop) -> Result<(), String> {
    let mut fixture = fixture(active, SoftwareRenderMode::Off, "idle-atlas-app-aggregate")?;
    fixture.activate(active, 1)?;
    let deadline = fixture.deadline()?;
    // The aggregate line is emitted, and its totals kept, only while `memory` is recorded at INFO.
    let subscriber = Registry::default()
        .with(EnvFilter::try_new("memory=info").map_err(|error| error.to_string())?);
    sonicterm_logging::test_capture::with_default(subscriber, || -> Result<(), String> {
        let first_sample = deadline - Duration::from_millis(1);
        check(fixture.app.__test_sample_pane_retention_at(first_sample), "the first pass runs")?;
        let before =
            fixture.app.__test_last_sampled_renderer_bytes().ok_or("no aggregate emitted")?;
        let charge = fixture.media_charge()?;
        check(charge.bytes >= IMAGE_BYTES, "precondition: the decoded image is charged")?;
        let promoted_bytes = fixture.atlas_bytes()?;
        check(
            fixture.app.__test_collect_and_service_redraw_due(deadline) == 1,
            "the release runs",
        )?;
        check(
            !fixture.app.__test_sample_pane_retention_at(deadline + Duration::from_millis(1)),
            "a pass inside the interval does nothing",
        )?;
        check(
            fixture.app.__test_last_sampled_renderer_bytes() == Some(before),
            "the aggregate keeps its figure until the next sample",
        )?;
        check(fixture.media_charge()? == charge, "the release leaves the pane charge alone")?;
        check(
            fixture.app.__test_sample_pane_retention_at(first_sample + RETENTION_SAMPLE_INTERVAL),
            "the next pass is due one interval later",
        )?;
        let after =
            fixture.app.__test_last_sampled_renderer_bytes().ok_or("no aggregate emitted")?;
        check(
            before.checked_sub(after) == promoted_bytes.checked_sub(4),
            &format!("the aggregate drops by the released bytes: {before} -> {after}, atlas {promoted_bytes} -> 4"),
        )?;
        check(fixture.media_charge()? == charge, "the next sample leaves the pane charge alone")
    })
}

/// After the deadline release, reactivating the image tab promotes the atlas again and draws exactly the
/// software pixels of the same tab sequence without a release (0, 1, 0), so only the release differs.
fn reshow_pixels(active: &ActiveEventLoop) -> Result<(), String> {
    let mut fixture = fixture(active, SoftwareRenderMode::Force, "idle-atlas-app-reshow")?;
    fixture.activate(active, 1)?;
    let plain = fixture.pixels()?;
    fixture.activate(active, 0)?;
    let expected = fixture.pixels()?;
    check(expected != plain, "the image tab draws something the plain tab does not")?;
    fixture.activate(active, 1)?;
    let deadline = fixture.deadline()?;
    check(fixture.app.__test_collect_and_service_redraw_due(deadline) == 1, "the release runs")?;
    check(fixture.dimensions()?.0 == (1, 1), "the CPU atlas is released")?;
    fixture.activate(active, 0)?;
    check(fixture.dimensions()?.0 != (1, 1), "the re-shown image promotes again")?;
    check(fixture.pixels()? == expected, "the re-shown frame matches the never-released frame")
}

#[test]
fn windows_idle_image_atlas_through_the_app() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("idle atlas event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for (name, case) in [
        ("timer release", timer_release as fn(&ActiveEventLoop) -> Result<(), String>),
        ("aggregate and pane charge", aggregate_and_pane_charge),
        ("reshow pixels", reshow_pixels),
    ] {
        if let Err(error) = case(active) {
            failures.push(format!("{name}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
