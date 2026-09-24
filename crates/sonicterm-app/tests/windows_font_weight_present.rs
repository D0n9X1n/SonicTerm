#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode, SubpixelAaMode},
    keymap::{Action, Keymap},
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{
        ClientToScreen, GetClipBox, GetDC, GetPixel, PtVisible, ReleaseDC, CLR_INVALID,
    },
    UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsHungAppWindow, IsIconic,
        IsWindowVisible, WindowFromPoint,
    },
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId, WindowLevel},
};

const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];
const WEIGHTS: [(&str, Action, usize); 3] = [
    ("weight-2", Action::IncreaseFontWeight, 4),
    ("weight-0-5", Action::DecreaseFontWeight, 6),
    ("reset", Action::ResetFontWeight, 1),
];

#[derive(Clone, Copy, Debug)]
enum Phase {
    Setup,
    BaselineRender,
    BaselineCapture,
    WeightAction { weight: usize, action: usize },
    CandidateRender(usize),
    CandidateCapture(usize),
    CacheRender(usize),
    CacheCapture(usize),
    Finish,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Setup | Self::BaselineRender | Self::BaselineCapture => "baseline",
            Self::WeightAction { weight, .. }
            | Self::CandidateRender(weight)
            | Self::CandidateCapture(weight)
            | Self::CacheRender(weight)
            | Self::CacheCapture(weight) => WEIGHTS[weight].0,
            Self::Finish => "scale-case",
        }
    }
}

struct ScaleCase {
    app: App,
    id: WindowId,
    geometry: Option<(f32, f32, f32)>,
    baseline: Option<image::RgbaImage>,
    emoji: Option<image::RgbaImage>,
    candidate: Option<image::RgbaImage>,
    output: Option<PathBuf>,
}

struct Probe {
    window: Option<Arc<Window>>,
    next_scale: usize,
    phase: Phase,
    case: Option<ScaleCase>,
    renderer_baseline: Option<usize>,
    deadline: Instant,
    outcome: Option<Result<(), String>>,
}

impl Probe {
    fn release_case(&mut self, mut result: Result<(), String>) -> Result<(), String> {
        drop(self.case.take());
        if let Some(baseline) = self.renderer_baseline.take() {
            let observed = sonicterm_gpu::core::live_renderer_count();
            if observed != baseline {
                result = Err(format!("renderer retained between scale cases: expected={baseline} observed={observed}; native_result={result:?}"));
            }
        }
        result
    }

    fn finish(&mut self, active: &ActiveEventLoop, result: Result<(), String>) {
        self.outcome = Some(self.release_case(result));
        active.exit();
    }

    fn advance(&mut self, active: &ActiveEventLoop, window: &Arc<Window>) -> Result<bool, String> {
        let scale = SCALES[self.next_scale];
        match self.phase {
            Phase::Setup => {
                self.renderer_baseline = Some(sonicterm_gpu::core::live_renderer_count());
                self.case = Some(setup_scale_case(active, window, scale)?);
                self.phase = Phase::BaselineRender;
            }
            Phase::BaselineRender => {
                let case = self.case.as_mut().unwrap();
                render(&mut case.app, active, case.id);
                self.phase = Phase::BaselineCapture;
            }
            Phase::BaselineCapture => {
                self.case.as_mut().unwrap().capture_baseline(window, scale)?;
                self.phase = Phase::WeightAction { weight: 0, action: 0 };
            }
            Phase::WeightAction { weight, action } => {
                assert!(self.case.as_mut().unwrap().app.run_action(&WEIGHTS[weight].1));
                self.phase = if action + 1 == WEIGHTS[weight].2 {
                    Phase::CandidateRender(weight)
                } else {
                    Phase::WeightAction { weight, action: action + 1 }
                };
            }
            Phase::CandidateRender(weight) => {
                let case = self.case.as_mut().unwrap();
                render(&mut case.app, active, case.id);
                self.phase = Phase::CandidateCapture(weight);
            }
            Phase::CandidateCapture(weight) => {
                self.case.as_mut().unwrap().capture_candidate(window, scale, WEIGHTS[weight].0)?;
                self.phase = Phase::CacheRender(weight);
            }
            Phase::CacheRender(weight) => {
                let case = self.case.as_mut().unwrap();
                render(&mut case.app, active, case.id);
                self.phase = Phase::CacheCapture(weight);
            }
            Phase::CacheCapture(weight) => {
                let case = self.case.as_ref().unwrap();
                assert_eq!(
                    &capture(&case.app, case.id, window, scale, "cache-hit")?,
                    case.candidate.as_ref().unwrap(),
                    "{} cache hit changed pixels",
                    WEIGHTS[weight].0
                );
                self.phase = if weight + 1 == WEIGHTS.len() {
                    Phase::Finish
                } else {
                    Phase::WeightAction { weight: weight + 1, action: 0 }
                };
            }
            Phase::Finish => {
                self.release_case(Ok(()))?;
                self.next_scale += 1;
                self.phase = Phase::Setup;
                return Ok(self.next_scale == SCALES.len());
            }
        }
        Ok(false)
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.window.is_some() || self.outcome.is_some() {
            return;
        }
        let result = active.create_window(
            Window::default_attributes()
                .with_title("SonicTerm font weight verification")
                .with_position(PhysicalPosition::new(100, 100))
                .with_inner_size(PhysicalSize::new(980, 410))
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_active(false)
                .with_visible(true),
        );
        match result {
            Ok(window) => {
                window.request_redraw();
                self.window = Some(Arc::new(window));
            }
            Err(error) => self.finish(active, Err(error.to_string())),
        }
    }

    fn window_event(&mut self, active: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !matches!(event, WindowEvent::RedrawRequested)
            || self.outcome.is_some()
            || self.window.as_ref().is_none_or(|window| window.id() != id)
        {
            return;
        }
        if Instant::now() >= self.deadline {
            self.finish(active, Err("native font probe exceeded its 180-second deadline".into()));
            return;
        }
        let window = self.window.as_ref().unwrap().clone();
        let scale = SCALES[self.next_scale];
        let phase = self.phase;
        let label = phase.label();
        let started = Instant::now();
        let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
            panic!("Windows handle");
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        let hung_before =
            // SAFETY: window keeps this HWND live while callback entry responsiveness is observed.
            unsafe { IsHungAppWindow(hwnd).as_bool() };
        eprintln!("font_probe_phase event=enter hwnd={hwnd:?} scale={scale} label={label} phase={phase:?} hung={hung_before}");
        // Catch assertion failures only to release the retained renderer and preserve the original failure as the test result.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(
                !hung_before,
                "native font callback entered unresponsive at scale {scale} phase {phase:?}"
            );
            let result = self.advance(active, &window);
            let hung_after =
                // SAFETY: window retains the same live HWND across this phase's native work.
                unsafe { IsHungAppWindow(hwnd).as_bool() };
            assert!(
                !hung_after,
                "native font callback became unresponsive at scale {scale} phase {phase:?}"
            );
            result
        }))
        .unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_owned()));
            Err(message.unwrap_or_else(|| "native font phase panicked".into()))
        });
        let hung_after =
            // SAFETY: the local Arc keeps hwnd alive even after a failing phase releases ScaleCase.
            unsafe { IsHungAppWindow(hwnd).as_bool() };
        eprintln!("font_probe_phase event=exit hwnd={hwnd:?} scale={scale} label={label} phase={phase:?} elapsed={:?} hung={hung_after}", started.elapsed());
        match result {
            Ok(false) => {
                // Return to native dispatch between phases; direct App rendering cannot pump Windows messages.
                window.request_redraw();
            }
            Ok(true) => self.finish(active, Ok(())),
            Err(error) => self.finish(active, Err(error)),
        }
    }

    fn about_to_wait(&mut self, active: &ActiveEventLoop) {
        if self.outcome.is_some() {
            return;
        }
        if Instant::now() >= self.deadline {
            self.finish(active, Err("native font probe exceeded its 180-second deadline".into()));
        } else {
            active.set_control_flow(ControlFlow::WaitUntil(self.deadline));
        }
    }
}

fn render(app: &mut App, active: &ActiveEventLoop, id: WindowId) {
    assert!(app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1)));
    ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
}

fn capture(
    app: &App,
    id: WindowId,
    window: &Window,
    scale: f32,
    phase: &str,
) -> Result<image::RgbaImage, String> {
    let size = window.inner_size();
    let mut pixels = image::RgbaImage::new(size.width, size.height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let [b, g, r, a] = app
            .__test_window_software_frame_pixel_bgra(id, x, y)
            .ok_or("missing software frame")?;
        *pixel = image::Rgba([r, g, b, a]);
    }
    let RawWindowHandle::Win32(handle) =
        window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Err("not a Win32 window".into());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let dc =
        // SAFETY: window owns a live HWND; the matching DC is released before returning.
        unsafe { GetDC(Some(hwnd)) };
    if dc.0.is_null() {
        return Err("GetDC failed".into());
    }
    let mut mismatch = None;
    let mut native = image::RgbaImage::new(size.width.min(840), size.height.min(260));
    let evidence = std::env::var_os("SONICTERM_FONT_PROBE_DIR").is_some();
    let step = if evidence { 1 } else { 3 };
    for y in (0..native.height()).step_by(step) {
        for x in (0..native.width()).step_by(step) {
            let observed =
                // SAFETY: dc belongs to the live client area and x/y are within its dimensions.
                unsafe { GetPixel(dc, x as i32, y as i32) }.0;
            native.put_pixel(
                x,
                y,
                image::Rgba([observed as u8, (observed >> 8) as u8, (observed >> 16) as u8, 255]),
            );
            let p = pixels.get_pixel(x, y).0;
            let expected = u32::from(p[0]) | (u32::from(p[1]) << 8) | (u32::from(p[2]) << 16);
            if observed == CLR_INVALID || observed != expected {
                let mut clip = RECT::default();
                let (clip_kind, point_visible, visible, iconic) =
                    // SAFETY: dc and hwnd remain live; clip is writable and x/y are scalar coordinates.
                    unsafe {
                    (
                        GetClipBox(dc, &mut clip),
                        PtVisible(dc, x as i32, y as i32).as_bool(),
                        IsWindowVisible(hwnd).as_bool(),
                        IsIconic(hwnd).as_bool(),
                    )
                };
                let mut screen = POINT { x: x as i32, y: y as i32 };
                let mut covering_pid = 0;
                let (mapped, covering, foreground) =
                    // SAFETY: hwnd is live, screen and covering_pid are writable; returned handles are only observed.
                    unsafe {
                    let mapped = ClientToScreen(hwnd, &mut screen).as_bool();
                    let covering = WindowFromPoint(screen);
                    GetWindowThreadProcessId(covering, Some(&mut covering_pid));
                    (mapped, covering, GetForegroundWindow())
                };
                let mut class = [0u16; 128];
                let (class_len, hung) =
                    // SAFETY: covering is observed without ownership; class is a writable buffer and hwnd remains live.
                    unsafe {
                    (GetClassNameW(covering, &mut class), IsHungAppWindow(hwnd).as_bool())
                };
                let class = String::from_utf16_lossy(&class[..class_len.max(0) as usize]);
                mismatch = Some(format!(
                    "native pixel ({x},{y}) expected={expected:#x} observed={observed:#x}; hwnd={hwnd:?} scale={scale} phase={phase} size={size:?} clip_kind={clip_kind:?} clip={clip:?} point_visible={point_visible} visible={visible} iconic={iconic}; mapped={mapped} screen={screen:?} covering={covering:?} covering_pid={covering_pid} self_pid={} foreground={foreground:?} covering_class={class} hung={hung}",
                    std::process::id()
                ));
                break;
            }
        }
        if mismatch.is_some() {
            break;
        }
    }
    let _ =
        // SAFETY: dc is released exactly once to the same live HWND that acquired it.
        unsafe { ReleaseDC(Some(hwnd), dc) };
    if let Some(error) = mismatch {
        return Err(error);
    }
    if let Some(path) = std::env::var_os("SONICTERM_FONT_PROBE_DIR") {
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        native.save(PathBuf::from(path).join("native-latest.png")).map_err(|e| e.to_string())?;
    }
    Ok(pixels)
}

fn row_crop(image: &image::RgbaImage, geometry: (f32, f32, f32), row: u32) -> image::RgbaImage {
    let (cw, ch, top) = geometry;
    image::imageops::crop_imm(
        image,
        0,
        (top + row as f32 * ch).ceil() as u32,
        (cw * 42.0).floor() as u32,
        ch.floor() as u32,
    )
    .to_image()
}

fn digit_top(image: &image::RgbaImage, geometry: (f32, f32, f32), row: u32, col: u32) -> u32 {
    let (cw, ch, top) = geometry;
    let background = image.get_pixel(image.width() - 1, image.height() - 1).0;
    let x0 = (col as f32 * cw).round() as u32;
    let x1 = ((col + 1) as f32 * cw).round() as u32;
    let y0 = (top + row as f32 * ch).ceil() as u32;
    let y1 = (top + (row + 1) as f32 * ch).floor() as u32;
    (y0..y1)
        .find(|y| {
            (x0..x1).any(|x| {
                let pixel = image.get_pixel(x, *y).0;
                (0..3).any(|channel| pixel[channel].abs_diff(background[channel]) > 80)
            })
        })
        .expect("digit must contain visible native ink")
}

fn setup_scale_case(
    active: &ActiveEventLoop,
    window: &Arc<Window>,
    scale: f32,
) -> Result<ScaleCase, String> {
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let theme = Theme::default();
    let mut config = Config::default();
    config.font.size = 14.5;
    config.font.line_height = 1.1;
    config.font.weight_scale = 1.0;
    config.font.subpixel_aa = SubpixelAaMode::Rgb;
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
            font_dirs: &[fonts],
            font_size: config.font.size,
            line_height_mult: config.font.line_height,
            font_weight_scale: config.font.weight_scale,
            subpixel_aa: config.font.subpixel_aa,
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: config.appearance.backdrop,
                opacity: 1.0,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "font-weight-test",
        },
    )
    .map_err(|e| e.to_string())?;
    renderer.set_scale_factor(scale);
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let pane = app.__test_seed_tab("font-weight");
    let id = app.__test_main_window_id().unwrap();
    let text = concat!(
        "\x1b[?25l",
        "\x1b[0m277 tests passed 0123456789 H0B8 regular\r\n",
        "\x1b[1m277 tests passed 0123456789 H0B8 bold\x1b[0m\r\n",
        "\x1b[3m277 tests passed 0123456789 H0B8 italic\x1b[0m\r\n",
        "\x1b[1;3m277 tests passed 0123456789 H0B8 bold italic\x1b[0m\r\n",
        "Combining: a\u{301} e\u{308}  symbols: \u{e0b0} \u{2713}\r\n",
        "\x1b[38;2;220;220;220mColor: \u{1f600} \u{1f680}\x1b[0m\r\n"
    );
    assert!(app.__test_advance_pane_parser(pane, text.as_bytes()));
    assert!(app.__test_attach_window_renderer(id, window.clone(), renderer));
    Ok(ScaleCase {
        app,
        id,
        geometry: None,
        baseline: None,
        emoji: None,
        candidate: None,
        output: std::env::var_os("SONICTERM_FONT_PROBE_DIR")
            .map(|path| PathBuf::from(path).join(format!("scale-{scale}"))),
    })
}

fn emoji_crop(image: &image::RgbaImage, geometry: (f32, f32, f32)) -> image::RgbaImage {
    let (cw, ch, top) = geometry;
    image::imageops::crop_imm(
        image,
        (cw * 7.0).round() as u32,
        (top + ch * 5.0).ceil() as u32,
        (cw * 5.0).round() as u32,
        ch.floor() as u32,
    )
    .to_image()
}

impl ScaleCase {
    fn capture_baseline(&mut self, window: &Window, scale: f32) -> Result<(), String> {
        let geometry = self.app.__test_window_cell_geometry(self.id).unwrap();
        let baseline = capture(&self.app, self.id, window, scale, "baseline")?;
        for row in 0..4 {
            assert_eq!(
                digit_top(&baseline, geometry, row, 0),
                digit_top(&baseline, geometry, row, 1),
                "style row {row} at raster scale {scale} misaligned 2/7"
            );
        }
        let emoji = emoji_crop(&baseline, geometry);
        let color_ink: std::collections::HashSet<_> = emoji
            .pixels()
            .filter(|p| p[0] > p[2].saturating_add(100) && p[1] > p[2].saturating_add(60))
            .map(|p| p.0)
            .collect();
        assert!(
            color_ink.len() > 4,
            "emoji fixture must contain varied color artwork, not blank or neutral text"
        );
        if let Some(path) = &self.output {
            std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
            baseline.save(path.join("weight-1.png")).map_err(|e| e.to_string())?;
            std::fs::copy(
                path.parent().unwrap().join("native-latest.png"),
                path.join("native-weight-1.png"),
            )
            .map_err(|e| e.to_string())?;
        }
        self.geometry = Some(geometry);
        self.baseline = Some(baseline);
        self.emoji = Some(emoji);
        Ok(())
    }

    fn capture_candidate(
        &mut self,
        window: &Window,
        scale: f32,
        label: &str,
    ) -> Result<(), String> {
        let geometry = self.geometry.unwrap();
        assert_eq!(
            self.app.__test_window_cell_geometry(self.id).unwrap(),
            geometry,
            "{label} layout drift"
        );
        let candidate = capture(&self.app, self.id, window, scale, label)?;
        assert!(
            &emoji_crop(&candidate, geometry) == self.emoji.as_ref().unwrap(),
            "{label} changed color artwork"
        );
        for row in 0..5 {
            let before = row_crop(self.baseline.as_ref().unwrap(), geometry, row);
            let after = row_crop(&candidate, geometry, row);
            if label == "reset" {
                assert_eq!(before, after, "reset row {row} did not restore identity");
            } else {
                assert_ne!(before, after, "{label} did not reweight style row {row}");
            }
        }
        if let Some(path) = &self.output {
            candidate.save(path.join(format!("{label}.png"))).map_err(|e| e.to_string())?;
            std::fs::copy(
                path.parent().unwrap().join("native-latest.png"),
                path.join(format!("native-{label}.png")),
            )
            .map_err(|e| e.to_string())?;
        }
        self.candidate = Some(candidate);
        Ok(())
    }
}

// Native style/weight/cache checks retain GDI readback and assert responsiveness while yielding between phases.
#[test]
fn windows_font_weight_preserves_layout_and_updates_every_style() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe {
        window: None,
        next_scale: 0,
        phase: Phase::Setup,
        case: None,
        renderer_baseline: None,
        deadline: Instant::now() + Duration::from_secs(180),
        outcome: None,
    };
    event_loop.run_app(&mut probe).expect("font verification event loop");
    probe.outcome.expect("native scale checks must finish").unwrap_or_else(|e| panic!("{e}"));
}
