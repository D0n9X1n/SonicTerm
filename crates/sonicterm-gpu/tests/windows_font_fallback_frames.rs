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
            overlays::{NotificationBubble, NotificationLevel},
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

/// The state one frame draws from: the grid, the tab bar, the IME preedit and a notification.
struct Scene {
    grid: Grid,
    tabs: TabBar,
    ime: ImeState,
    notification: NotificationBubble,
}

/// Render one attempt of `fonts` for `scene`, as the App's redraw does after preparing fonts.
fn render_once(
    renderer: &mut GpuRenderer,
    fonts: &sonicterm_gpu::core::FrameFonts,
    scene: &mut Scene,
    theme: &Theme,
    size: PhysicalSize<u32>,
) -> PresentOutcome {
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
    renderer.render_with_outcome(
        fonts,
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
        Some(&scene.notification),
        None,
        None,
    )
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
        match render_once(renderer, &fonts, scene, theme, size) {
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
        glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &theme, settings) {
        Ok(renderer) => renderer,
        Err(error) => {
            return Ok(Outcome::HostIncapable(format!("renderer creation failed: {error}")))
        }
    };
    renderer.set_cursor_blink(false);
    renderer.set_tab_bar_visible(true);
    // The renderer counts, as a perf run's renderer does, so the attempts below are measured.
    renderer.set_frame_counting(true);
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
    let notification = NotificationBubble {
        level: NotificationLevel::Info,
        message: String::from("fallback"),
        expires_at: None,
    };
    let mut scene =
        Scene { grid: Grid::new(16, 3), tabs: TabBar::new(), ime: ImeState::new(), notification };
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
    // The fallback face must be applied by a frame whose first attempt retries, below.
    let mut wakes = 0_u32;
    let mut retry_checked = false;
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
            if retry_checked {
                draw(&mut renderer, &mut scene, &theme, size)?;
            } else {
                // When: this is the first frame to apply a generation, its first attempt is forced to retry.
                retry_checked = true;
                forced_retry_on_apply(&mut renderer, &mut scene, &theme, size)?;
            }
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
    if !retry_checked {
        return Err(String::from(
            "no frame applied the fallback generation, so the retry was not checked",
        ));
    }
    attempt_attribution(&mut renderer, &mut scene, &theme, size)?;
    Ok(Outcome::Exercised)
}

/// The first frame that applies a fallback generation, with its first attempt forced to an atlas
/// retry: that attempt carries the apply and folds once, unpresented. Each retry is a new
/// preparation; it carries an apply only when the worker published a newer generation since the
/// attempt before it, so the expected counts follow the tokens the preparations returned.
fn forced_retry_on_apply(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    theme: &Theme,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    let before = renderer.frame_stats();
    let fonts = renderer.begin_frame_fonts();
    let _ = renderer.measure_tab_widths(&fonts, &mut scene.tabs, false, false, Instant::now());
    renderer.__change_glyph_atlas_during_next_assembly();
    let first = render_once(renderer, &fonts, scene, theme, size);
    if !matches!(first, PresentOutcome::AtlasRetry) {
        return Err(format!("the forced attempt did not retry: {first:?}"));
    }
    // The forced attempt applied a generation; a retry applies again only when its token moved.
    let mut previous_token = (fonts.notice_id(), fonts.generation());
    let mut expected_applies = 1_u64;
    let mut retry_attempts = 0_u64;
    let mut presented_applied = None;
    for _ in 0..4 {
        let fonts = renderer.begin_frame_fonts();
        let token = (fonts.notice_id(), fonts.generation());
        let applied_here = token != previous_token;
        previous_token = token;
        if applied_here {
            // When: the worker published a newer generation since the attempt before this one.
            expected_applies += 1;
        }
        let _ = renderer.measure_tab_widths(&fonts, &mut scene.tabs, false, false, Instant::now());
        retry_attempts += 1;
        match render_once(renderer, &fonts, scene, theme, size) {
            PresentOutcome::Presented => {
                presented_applied = Some(applied_here);
                break;
            }
            PresentOutcome::AtlasRetry => {}
            other => return Err(format!("the retry did not present: {other:?}")),
        }
    }
    let Some(presented_applied) = presented_applied else {
        return Err(String::from("the retried frame kept retrying its atlas"));
    };
    let after = renderer.frame_stats();
    let applies = after.font_generation_applies - before.font_generation_applies;
    let apply = (
        after.apply_attempts.attempts - before.apply_attempts.attempts,
        after.apply_attempts.presented - before.apply_attempts.presented,
    );
    let every = (
        after.attempts.attempts - before.attempts.attempts,
        after.attempts.presented - before.attempts.presented,
    );
    let expected_apply = (expected_applies, u64::from(presented_applied));
    if applies != expected_applies || apply != expected_apply || every != (1 + retry_attempts, 1) {
        return Err(format!(
            "a retried apply: {applies} applies (expected {expected_applies}), apply (attempts, \
             presented) {apply:?} (expected {expected_apply:?}), every {every:?} (expected \
             ({}, 1))",
            1 + retry_attempts
        ));
    }
    Ok(())
}

/// The counted attempts of the real renderer: every fallback generation the frames applied is
/// carried by exactly one render attempt, however many retries or redraws followed, and that
/// attempt rasterized and shaped. Then one prepared token is rendered twice: both are attempts,
/// neither is an apply, and the notification text laid out inside them is shaped inside the
/// attempt rather than outside it.
fn attempt_attribution(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    theme: &Theme,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    let stats = renderer.frame_stats();
    let applied = stats.apply_attempts;
    if stats.font_generation_applies == 0 {
        return Err(String::from("the frames counted no fallback generation apply"));
    }
    if applied.attempts != stats.font_generation_applies {
        return Err(format!(
            "{} generation applies but {} apply attempts",
            stats.font_generation_applies, applied.attempts
        ));
    }
    if applied.raster_calls == 0 || applied.shape_requests == 0 || applied.attempt_ns == 0 {
        return Err(format!("the apply attempt measured no work: {applied:?}"));
    }
    let before = renderer.frame_stats();
    let fonts = renderer.begin_frame_fonts();
    for pass in 0..2 {
        // A full redraw each time, so the notification is laid out again.
        scene.grid.mark_all_dirty();
        let outcome = render_once(renderer, &fonts, scene, theme, size);
        if !matches!(outcome, PresentOutcome::Presented | PresentOutcome::AtlasRetry) {
            return Err(format!("reused-token pass {pass} did not present: {outcome:?}"));
        }
    }
    let after = renderer.frame_stats();
    let attempts = after.attempts.attempts - before.attempts.attempts;
    let shaped = after.shape_requests - before.shape_requests;
    let shaped_in_attempts = after.attempts.shape_requests - before.attempts.shape_requests;
    if attempts != 2 || after.apply_attempts != before.apply_attempts {
        return Err(format!(
            "a reused token: {attempts} attempts, applies {:?}",
            after.apply_attempts
        ));
    }
    if shaped == 0 || shaped_in_attempts != shaped {
        return Err(format!(
            "shaping in the passes {shaped}, inside their attempts {shaped_in_attempts}"
        ));
    }
    Ok(())
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
