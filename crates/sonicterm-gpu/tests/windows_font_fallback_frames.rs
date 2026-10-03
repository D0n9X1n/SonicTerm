#![cfg(target_os = "windows")]
//! A real renderer (the forced GDI presenter) in production frame order: a character the
//! configured font lacks draws as tofu without the frame waiting, the fallback worker's
//! completion wakes the renderer once, and a later frame draws the resolved glyph in the grid,
//! in an unchanged preedit and in a tab title whose width changes.

use std::{
    sync::{mpsc, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use sonicterm_gpu::core::{
    FontFallbackWaker, GpuRenderer, PresentOutcome, RendererSettings, SurfaceAppearance,
};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::{CellFlags, Color, Grid, Pos},
        ui::{
            ime::ImeState,
            tabs::{Tab, TabBar},
        },
    },
    CursorStyle, PaneRender, PixelRect,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

/// A character the controlled primary face lacks; only the test locator's Rec Mono has it.
const UNRESOLVED: char = 'é';
/// How long the fallback worker may take to find and publish a face on a hosted runner.
const FALLBACK_DEADLINE: Duration = Duration::from_secs(20);

enum Outcome {
    Exercised,
    HostIncapable(String),
}

struct Probe {
    outcome: Option<Result<Outcome, String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so the whole case runs inside this one.
        self.outcome = Some(run(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

/// Answers every fallback request with Rec Mono, which has the character the primary face lacks,
/// so the test controls coverage and advances instead of depending on the host's system fonts.
struct RecMonoLocator;

impl sonicterm_font::locator::FontLocator for RecMonoLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        _: &[char],
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontOrigin};
        let handle = FontDataHandle {
            source: FontDataSource::OnDisk(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
            ),
            index: 0,
            variation: 0,
            origin: FontOrigin::BuiltIn,
            coverage: None,
        };
        Ok(vec![sonicterm_font::parser::ParsedFont::from_locator(&handle)?])
    }
}

/// A temporary directory holding the ASCII-only primary face, removed on drop.
struct PrimaryFaceDir(std::path::PathBuf);

impl Drop for PrimaryFaceDir {
    // Lifecycle: dropping `PrimaryFaceDir` removes its temporary font directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A stack whose only primary face is the ASCII-only HarfBuzz sample font (family Roboto), with
/// fallback answered by `RecMonoLocator`: the character draws notdef until that face merges.
fn controlled_stack() -> Result<(sonicterm_engine::FontStack, PrimaryFaceDir), String> {
    let directory = PrimaryFaceDir(
        std::env::temp_dir().join(format!("sonicterm-fallback-frames-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&directory.0);
    std::fs::create_dir_all(&directory.0).map_err(|error| error.to_string())?;
    std::fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
        directory.0.join("primary.ttf"),
    )
    .map_err(|error| error.to_string())?;
    let stack = sonicterm_engine::FontStack::try_new_with_locator_for_test(
        "Roboto",
        vec![directory.0.clone()],
        Arc::new(RecMonoLocator),
        14.0,
        96,
    )
    .map_err(|error| error.to_string())?;
    Ok((stack, directory))
}

/// Opens the worker gate when dropped, so every early return releases a parked fallback worker.
struct GateRelease(Arc<(Mutex<bool>, Condvar)>);

impl Drop for GateRelease {
    // Lifecycle: dropping `GateRelease` opens the gate and wakes the parked worker.
    fn drop(&mut self) {
        let (open, opened) = &*self.0;
        *open.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        opened.notify_all();
    }
}

/// The state one frame draws from: the grid, the tab bar and the IME preedit.
struct Scene {
    grid: Grid,
    tabs: TabBar,
    ime: ImeState,
}

/// One frame in production order: prepare fonts, measure tab widths, render. An atlas retry
/// draws again, as the App's redraw does.
fn draw(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    theme: &Theme,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    for _ in 0..4 {
        let fonts = renderer.begin_frame_fonts();
        let _ = renderer.measure_tab_widths(&fonts, &mut scene.tabs, false, false, Instant::now());
        let mut panes = [PaneRender {
            id: 1,
            rect_px: PixelRect { x: 0, y: 0, w: size.width, h: size.height },
            grid: &mut scene.grid,
            viewport_top_abs: None,
            is_active: true,
            cursor_style: CursorStyle::BlockSteady,
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: Vec::new(),
        }];
        match renderer.render_with_outcome(
            &fonts,
            &mut panes,
            theme,
            false,
            None,
            None,
            &scene.tabs,
            false,
            None,
            None,
            Some(&scene.ime),
            None,
            None,
            None,
            None,
        ) {
            PresentOutcome::Presented => return Ok(()),
            PresentOutcome::AtlasRetry => {}
            other => return Err(format!("the frame did not present: {other:?}")),
        }
    }
    Err(String::from("the frame kept retrying its atlas"))
}

/// The software frame's pixels in rows `first_row..last_row`, across the whole width.
fn band(
    renderer: &GpuRenderer,
    width_px: u32,
    first_row: u32,
    last_row: u32,
) -> Result<Vec<[u8; 4]>, String> {
    (first_row..last_row)
        .flat_map(|pixel_row| (0..width_px).map(move |pixel_col| (pixel_col, pixel_row)))
        .map(|(pixel_col, pixel_row)| {
            renderer
                .__test_software_frame_pixel_bgra(pixel_col, pixel_row)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

/// The tab-bar, grid-row and preedit-row bands of the last presented frame. The tab bar is
/// pinned to the bottom of the window, from `tab_bar_y_offset` to the surface's bottom edge.
fn bands(renderer: &GpuRenderer, size: PhysicalSize<u32>) -> Result<[Vec<[u8; 4]>; 3], String> {
    let top_px = renderer.top_inset().ceil() as u32;
    let cell_height_px = renderer.cell_size().1.ceil() as u32;
    let bottom_px = size.height;
    let tab_bar_top_px = (renderer.tab_bar_y_offset().floor() as u32).min(bottom_px);
    let row_end = (top_px + cell_height_px).min(tab_bar_top_px);
    let preedit_end = (top_px + 2 * cell_height_px).min(tab_bar_top_px);
    if tab_bar_top_px >= bottom_px || row_end <= top_px || preedit_end <= row_end {
        return Err(format!(
            "no room for the bands: top {top_px}, cell {cell_height_px}, bar {tab_bar_top_px}"
        ));
    }
    Ok([
        band(renderer, size.width, tab_bar_top_px, bottom_px)?,
        band(renderer, size.width, top_px, row_end)?,
        band(renderer, size.width, row_end, preedit_end)?,
    ])
}

fn run(active: &ActiveEventLoop) -> Result<Outcome, String> {
    let window = match active.create_window(
        Window::default_attributes()
            .with_inner_size(PhysicalSize::new(320, 120))
            .with_visible(true)
            .with_title("SonicTerm font fallback frames"),
    ) {
        Ok(window) => Arc::new(window),
        Err(error) => {
            return Ok(Outcome::HostIncapable(format!("window creation failed: {error}")))
        }
    };
    let theme = Theme::default();
    let settings = RendererSettings {
        font_family: "monospace",
        font_dirs: &[],
        font_size: 14.0,
        line_height_mult: 1.2,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role: "font-fallback-frames",
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &theme, settings) {
        Ok(renderer) => renderer,
        Err(error) => {
            return Ok(Outcome::HostIncapable(format!("renderer creation failed: {error}")))
        }
    };
    renderer.set_cursor_blink(false);
    renderer.set_tab_bar_visible(true);
    // The renderer draws with a stack whose coverage the test controls, through the path a font
    // reload takes, so frame 1's tofu and the later glyph never depend on the host's fonts.
    let (stack, _primary_face) = controlled_stack()?;
    renderer.__test_adopt_body_font_stack("fallback-frames-test", stack);
    // The waker runs on the fallback worker thread; the test receives its notice ids here.
    let (sender, receiver) = mpsc::channel::<u64>();
    let sender = Mutex::new(sender);
    let waker: FontFallbackWaker = Arc::new(move |notice_id| {
        let _ = sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(notice_id);
    });
    renderer.set_font_fallback_waker(waker);
    // Hold the fallback worker inside the pending-handle lock until frame 1 has drawn, so frame 1
    // deterministically shapes the character as notdef however fast the locator answers.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = Arc::clone(&gate);
    renderer.__test_set_fallback_append_hook(Arc::new(move || {
        // Bounded, so a test that fails before opening the gate never leaves the worker parked.
        let (open, opened) = &*worker_gate;
        let guard = open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = opened.wait_timeout_while(guard, FALLBACK_DEADLINE, |is_open| !*is_open);
    }));
    let _release_on_exit = GateRelease(Arc::clone(&gate));

    let size = window.inner_size();
    let mut scene = Scene { grid: Grid::new(16, 3), tabs: TabBar::new(), ime: ImeState::new() };
    scene.grid.put_char(UNRESOLVED, Color::Default, Color::Default, CellFlags::empty());
    scene.grid.cursor = Pos { row: 1, col: 0 };
    scene.tabs.push(Tab::new(format!("{UNRESOLVED}{UNRESOLVED}{UNRESOLVED}")));
    scene.ime.handle_enabled();
    scene.ime.handle_preedit(&UNRESOLVED.to_string(), None);

    // Frame 1 shapes without waiting, so the unresolved character is still tofu.
    let started = Instant::now();
    draw(&mut renderer, &mut scene, &theme, size)?;
    if started.elapsed() > Duration::from_secs(5) {
        return Err(format!(
            "the first frame took {:?}; it must not wait for fallback",
            started.elapsed()
        ));
    }
    if !renderer.last_missing_tofu().contains(&UNRESOLVED) {
        return Err(String::from("frame 1 drew the character before fallback resolved it"));
    }
    let width_before = scene.tabs.tabs()[0].content_width_px();
    let [tabs_before, row_before, preedit_before] = bands(&renderer, size)?;
    {
        // Release the worker: it publishes the face and completes the notice.
        let (open, opened) = &*gate;
        *open.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        opened.notify_all();
    }

    // Each delivered wake is acknowledged as the App's handler does, and a due one gets a frame,
    // until a frame draws the resolved glyph and the title has been measured with it. The title
    // stack shares the body configuration, so the frame that applies its generation remeasures.
    let deadline = Instant::now() + FALLBACK_DEADLINE;
    let mut wakes = 0_u32;
    while renderer.last_missing_tofu().contains(&UNRESOLVED)
        || scene.tabs.tabs()[0].content_width_px() == width_before
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let notice_id = receiver.recv_timeout(remaining).map_err(|_| {
            format!("no fallback wake within {FALLBACK_DEADLINE:?} ({wakes} so far)")
        })?;
        wakes += 1;
        if renderer.font_fallback_notice_id() != Some(notice_id) {
            return Err(String::from("the wake carried another renderer's notice"));
        }
        if renderer.acknowledge_font_fallback(notice_id) {
            draw(&mut renderer, &mut scene, &theme, size)?;
        }
    }

    let [tabs_after, row_after, preedit_after] = bands(&renderer, size)?;
    if row_after == row_before {
        return Err(String::from("the grid row still shows the tofu pixels"));
    }
    if preedit_after == preedit_before {
        return Err(String::from("the unchanged preedit was not redrawn with the real glyph"));
    }
    let width_after = scene.tabs.tabs()[0].content_width_px();
    if width_after.is_none() || width_after == width_before {
        return Err(format!(
            "the tab title width did not change: {width_before:?} -> {width_after:?}"
        ));
    }
    if tabs_after == tabs_before {
        return Err(String::from("the tab bar still shows the tofu title"));
    }
    Ok(Outcome::Exercised)
}

/// A character the font lacks draws tofu at once, and after the fallback worker publishes its
/// face, the wake leads to a frame that draws the real glyph in the grid, the preedit and the
/// tab title, whose measured width changes.
#[test]
fn a_fallback_face_reaches_the_grid_the_preedit_and_the_tab_title() {
    let event_loop = match EventLoop::builder().with_any_thread(true).build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            println!("capability=HOST_INCAPABLE reason=event-loop:{error}");
            return;
        }
    };
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("event loop runs");
    match probe.outcome.expect("resumed must run") {
        Ok(Outcome::Exercised) => println!("capability=EXERCISED presenter=windows-software"),
        Ok(Outcome::HostIncapable(reason)) => println!("capability=HOST_INCAPABLE reason={reason}"),
        Err(error) => panic!("font fallback frames: {error}"),
    }
}
