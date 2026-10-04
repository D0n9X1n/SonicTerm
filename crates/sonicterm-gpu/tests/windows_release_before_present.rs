#![cfg(target_os = "windows")]
//! The one releasing render call on a real renderer: the frame's source is lent once and dropped
//! before presentation, a stopped device returns its typed exits, the compatibility wrapper draws
//! the releasing call's pixels and clears dirt only after `Presented`, and both paths count alike.

use std::sync::{Arc, Mutex, MutexGuard};

use sonicterm_gpu::core::{
    acknowledge_receipts, FrameOutcome, GpuRenderer, PresentOutcome, RendererSettings, SkipReason,
    SurfaceAppearance, SurfaceRetryReason,
};
use sonicterm_gpu::device_errors::GpuFaultKind;
use sonicterm_gpu::frame_stats::FrameStats;
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::{CellFlags, Color, Grid},
        ui::tabs::TabBar,
    },
    BorrowedSource, CursorStyle, FrameSource, PaneRender, PixelRect,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 96;

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so every case runs inside this one.
        self.outcome = Some(run(active));
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

/// Whether this host enumerates no wgpu adapter at all, established apart from renderer
/// construction. That is the only limitation that turns a failed wgpu renderer into a skip;
/// surface configuration, device and resource errors on a host with an adapter fail the test.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// Classify a wgpu renderer construction failure: `HOST_INCAPABLE` only when no adapter exists.
fn wgpu_construction_failure(error: impl std::fmt::Display) -> String {
    if host_has_no_adapter() {
        format!("HOST_INCAPABLE: no wgpu adapter: {error}")
    } else {
        // When: host_has_no_adapter is false an adapter exists, so the construction error is a defect.
        format!("wgpu renderer construction failed on a host with an adapter: {error}")
    }
}

/// A renderer on its own window, presenting through GDI (`software`, `Force`) or through wgpu (`Off`,
/// which never degrades, even on a software adapter). The presenter it resolved is checked. A wgpu
/// renderer is skipped as `HOST_INCAPABLE: ...` only when the host enumerates no adapter.
fn fresh_renderer(
    active: &ActiveEventLoop,
    role: &'static str,
    software: bool,
) -> Result<GpuRenderer, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(WIDTH, HEIGHT))
                    .with_visible(true)
                    .with_title(role),
            )
            .map_err(|error| error.to_string())?,
    );
    let mode = if software { SoftwareRenderMode::Force } else { SoftwareRenderMode::Off };
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
            software_render_mode: mode,
        },
        role,
        glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
    };
    let mut renderer = match GpuRenderer::new(window, active, &Theme::default(), settings) {
        Ok(renderer) => renderer,
        Err(error) if !software => return Err(wgpu_construction_failure(error)),
        Err(error) => return Err(error.to_string()),
    };
    check(
        renderer.is_software_render_degraded() == software,
        &format!("{role}: the renderer presents through GDI={software}"),
    )?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    Ok(renderer)
}

/// A grid holding `text`, every row dirty.
fn text_grid(text: &str) -> Grid {
    let mut grid = Grid::new(16, 4);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.mark_all_dirty();
    grid
}

/// The one pane of every frame here, filling the window, following the live tail.
fn pane(grid: &mut Grid) -> PaneRender<'_> {
    pane_at(grid, None)
}

/// The one pane, its view at `viewport_top_abs`.
fn pane_at(grid: &mut Grid, viewport_top_abs: Option<u64>) -> PaneRender<'_> {
    PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: WIDTH, h: HEIGHT },
        grid,
        viewport_top_abs,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }
}

/// One frame through the releasing call.
fn release(renderer: &mut GpuRenderer, source: impl FrameSource) -> FrameOutcome {
    let theme = Theme::default();
    let tabs = TabBar::new();
    let fonts = renderer.begin_frame_fonts();
    renderer.render_releasing(
        &fonts, source, &theme, false, None, None, &tabs, false, None, None, None, None, None,
        None, None,
    )
}

/// One frame through the compatibility wrapper.
fn wrapped(renderer: &mut GpuRenderer, grid: &mut Grid) -> PresentOutcome {
    wrapped_at(renderer, grid, None)
}

/// One frame through the compatibility wrapper, the pane's view at `viewport_top_abs`.
fn wrapped_at(
    renderer: &mut GpuRenderer,
    grid: &mut Grid,
    viewport_top_abs: Option<u64>,
) -> PresentOutcome {
    let theme = Theme::default();
    let tabs = TabBar::new();
    let fonts = renderer.begin_frame_fonts();
    let mut panes = [pane_at(grid, viewport_top_abs)];
    renderer.render_with_outcome(
        &fonts, &mut panes, &theme, false, None, None, &tabs, false, None, None, None, None, None,
        None, None,
    )
}

/// A source shaped like the App's: it owns the grid's lock and records when it is lent and dropped.
struct Held<'lock> {
    grid: MutexGuard<'lock, Grid>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl FrameSource for Held<'_> {
    fn lend<R>(
        mut self,
        assemble: impl for<'slice, 'grid> FnOnce(&'slice mut [PaneRender<'grid>]) -> R,
    ) -> R {
        self.log.lock().unwrap().push("lend");
        let mut panes = [pane(&mut self.grid)];
        assemble(&mut panes)
    }
}

impl Drop for Held<'_> {
    // Lifecycle: dropping the source releases the grid lock it holds; the log records when.
    fn drop(&mut self) {
        self.log.lock().unwrap().push("drop");
    }
}

/// Every pixel of a renderer's software frame, as BGRA.
fn software_pixels(renderer: &GpuRenderer) -> Vec<[u8; 4]> {
    (0..HEIGHT)
        .flat_map(|pixel_y| (0..WIDTH).map(move |pixel_x| (pixel_x, pixel_y)))
        .filter_map(|(pixel_x, pixel_y)| {
            renderer.__test_software_frame_pixel_bgra(pixel_x, pixel_y)
        })
        .collect()
}

/// Test 15: each frame lends its source exactly once and drops it before presentation, which
/// finds the grid's lock free, on the GDI and the wgpu presenter.
fn lend_drop_present(active: &ActiveEventLoop) -> Result<(), String> {
    for software in [true, false] {
        let mut renderer = match fresh_renderer(active, "release-order", software) {
            Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
                // When: the host cannot create a wgpu renderer, report the capability, not a pass.
                println!("capability=HOST_INCAPABLE case=release-order-wgpu reason={reason}");
                continue;
            }
            other => other?,
        };
        let grid = Arc::new(Mutex::new(text_grid("order")));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (hook_grid, hook_log) = (Arc::clone(&grid), Arc::clone(&log));
        renderer.__set_present_hook(Some(Box::new(move || {
            let free = hook_grid.try_lock().is_ok();
            hook_log.lock().unwrap().push(if free {
                "present, grid free"
            } else {
                "present, grid held"
            });
            false
        })));
        for frame in 0..2 {
            log.lock().unwrap().clear();
            grid.lock().unwrap().put_char('x', Color::Default, Color::Default, CellFlags::empty());
            let outcome =
                release(&mut renderer, Held { grid: grid.lock().unwrap(), log: Arc::clone(&log) });
            check(
                matches!(outcome.outcome, PresentOutcome::Presented),
                &format!("software={software} frame {frame} presented: {:?}", outcome.outcome),
            )?;
            check(!outcome.receipts.is_empty(), "a presented frame returns receipts")?;
            let events = log.lock().unwrap().clone();
            check(
                events == ["lend", "drop", "present, grid free"],
                &format!("software={software} frame {frame}: {events:?}"),
            )?;
        }
    }
    Ok(())
}

/// Test 15a: on a stopped device the call returns the one-time stop report and then the later
/// stopped outcome, with no receipts, the source lent and dropped, and the dirt kept; an empty
/// source is still `NoPanes`.
fn stopped_device_returns_its_typed_exits(active: &ActiveEventLoop) -> Result<(), String> {
    let mut renderer = fresh_renderer(active, "release-stopped", true)?;
    let grid = Arc::new(Mutex::new(text_grid("stop")));
    let log = Arc::new(Mutex::new(Vec::new()));
    let healthy =
        release(&mut renderer, Held { grid: grid.lock().unwrap(), log: Arc::clone(&log) });
    check(matches!(healthy.outcome, PresentOutcome::Presented), "the healthy frame presented")?;
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    grid.lock().unwrap().mark_all_dirty();
    log.lock().unwrap().clear();
    let first = release(&mut renderer, Held { grid: grid.lock().unwrap(), log: Arc::clone(&log) });
    check(
        matches!(&first.outcome, PresentOutcome::RenderingUnavailable(context) if context.reports_stop),
        &format!("the first stopped frame carries the stop report: {:?}", first.outcome),
    )?;
    check(first.receipts.is_empty(), "a stopped frame returns no receipts")?;
    check(*log.lock().unwrap() == ["lend", "drop"], "the source is lent and dropped")?;
    check(grid.lock().unwrap().dirty_count() > 0, "a stopped frame keeps the dirt")?;
    let later = release(&mut renderer, Held { grid: grid.lock().unwrap(), log: Arc::clone(&log) });
    check(
        matches!(&later.outcome, PresentOutcome::RenderingUnavailable(context) if !context.reports_stop),
        &format!("a later stopped frame is silent: {:?}", later.outcome),
    )?;
    check(later.receipts.is_empty(), "a later stopped frame returns no receipts")?;
    let mut no_panes: [PaneRender<'_>; 0] = [];
    let empty = release(&mut renderer, BorrowedSource(&mut no_panes[..]));
    check(
        matches!(empty.outcome, PresentOutcome::Skipped(SkipReason::NoPanes)),
        &format!("an empty source on a stopped device is NoPanes: {:?}", empty.outcome),
    )
}

/// Test 9: the wrapper on a dirty standalone grid gives the releasing call's outcome and pixels,
/// clears the dirt after `Presented`, and keeps it after a planned `Noop` skip, an atlas retry and a
/// stopped frame. The wgpu presenter's surface retry is covered by
/// `wrapper_keeps_dirt_after_a_surface_retry`.
fn wrapper_matches_the_releasing_call(active: &ActiveEventLoop) -> Result<(), String> {
    let mut wrapper = fresh_renderer(active, "release-wrapper", true)?;
    let mut releasing = fresh_renderer(active, "release-wrapper", true)?;
    let (mut wrapper_grid, mut releasing_grid) = (text_grid("pixels"), text_grid("pixels"));
    let wrapped_outcome = wrapped(&mut wrapper, &mut wrapper_grid);
    let released = {
        let mut panes = [pane(&mut releasing_grid)];
        let FrameOutcome { outcome, receipts } =
            release(&mut releasing, BorrowedSource(&mut panes[..]));
        if matches!(outcome, PresentOutcome::Presented) {
            let _cleared = acknowledge_receipts(&receipts, &mut panes);
        }
        outcome
    };
    check(
        matches!(wrapped_outcome, PresentOutcome::Presented)
            && matches!(released, PresentOutcome::Presented),
        &format!("both present: {wrapped_outcome:?} / {released:?}"),
    )?;
    check(
        wrapper_grid.dirty_count() == 0 && releasing_grid.dirty_count() == 0,
        "a presented frame clears its dirt",
    )?;
    let wrapper_pixels = software_pixels(&wrapper);
    check(
        !wrapper_pixels.is_empty() && wrapper_pixels == software_pixels(&releasing),
        "the wrapper draws the releasing call's pixels",
    )?;
    let unchanged = wrapped(&mut wrapper, &mut wrapper_grid);
    check(
        !matches!(unchanged, PresentOutcome::Presented),
        &format!("an unchanged frame presents nothing: {unchanged:?}"),
    )?;

    // A planned skip: the view sits at the top of history and the only dirt is a live row below it,
    // outside the drawable damage, so the plan is Noop and that dirt must survive.
    let mut scrolled = fresh_renderer(active, "release-wrapper-noop", true)?;
    let mut history = text_grid("history");
    for _ in 0..8 {
        history.scroll_up(1);
    }
    check(history.scrollback_len() > usize::from(history.rows), "the live rows are off the view")?;
    let first = wrapped_at(&mut scrolled, &mut history, Some(0));
    check(matches!(first, PresentOutcome::Presented), &format!("the scrolled view: {first:?}"))?;
    history.goto(3, 0);
    history.put_char('z', Color::Default, Color::Default, CellFlags::empty());
    let noop = wrapped_at(&mut scrolled, &mut history, Some(0));
    check(
        matches!(noop, PresentOutcome::Skipped(SkipReason::Noop)),
        &format!("offscreen-only dirt plans Noop: {noop:?}"),
    )?;
    check(history.dirty_rows().any(|row| row == 3), "a Noop skip keeps the dirt")?;

    // An atlas retry: the atlas changes during assembly, so nothing is presented and dirt survives.
    scrolled.__change_glyph_atlas_during_next_assembly();
    let retry = wrapped_at(&mut scrolled, &mut history, None);
    check(
        matches!(retry, PresentOutcome::AtlasRetry),
        &format!("a changed atlas retries: {retry:?}"),
    )?;
    check(history.dirty_count() > 0, "an atlas retry keeps the dirt")?;
    let after = wrapped_at(&mut scrolled, &mut history, None);
    check(matches!(after, PresentOutcome::Presented), &format!("the retried frame: {after:?}"))?;
    check(history.dirty_count() == 0, "the presented retry clears the dirt")?;
    wrapper.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    wrapper_grid.mark_all_dirty();
    let stopped = wrapped(&mut wrapper, &mut wrapper_grid);
    check(
        matches!(stopped, PresentOutcome::RenderingUnavailable(_)),
        &format!("the stopped wrapper frame: {stopped:?}"),
    )?;
    check(wrapper_grid.dirty_count() > 0, "a stopped wrapper frame keeps its dirt")
}

/// The wrapper on a wgpu renderer whose next acquire is forced to report occlusion: the real
/// `render_with_outcome` path returns `SurfaceRetry`, keeps every dirty row, and the next frame
/// presents and clears them.
fn wrapper_keeps_dirt_after_a_surface_retry(active: &ActiveEventLoop) -> Result<(), String> {
    let mut renderer = match fresh_renderer(active, "release-wrapper-surface", false) {
        Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
            // When: fresh_renderer found no adapter at all, report the capability, not a pass.
            println!("capability=HOST_INCAPABLE case=release-wrapper-surface reason={reason}");
            return Ok(());
        }
        other => other?,
    };
    let mut grid = text_grid("surface");
    let first = wrapped(&mut renderer, &mut grid);
    check(matches!(first, PresentOutcome::Presented), &format!("the first frame: {first:?}"))?;
    grid.mark_all_dirty();
    renderer.__occlude_next_surface_acquire();
    let retry = wrapped(&mut renderer, &mut grid);
    check(
        matches!(retry, PresentOutcome::SurfaceRetry(SurfaceRetryReason::Occluded)),
        &format!("the occluded acquire retries: {retry:?}"),
    )?;
    check(grid.dirty_count() == usize::from(grid.rows), "a surface retry keeps every dirty row")?;
    let after = wrapped(&mut renderer, &mut grid);
    check(matches!(after, PresentOutcome::Presented), &format!("the retried frame: {after:?}"))?;
    check(grid.dirty_count() == 0, "the presented retry clears the dirt")
}

/// The deterministic counters test 10 compares; durations are left out.
fn deterministic(stats: &FrameStats) -> [u64; 11] {
    [
        stats.shape_requests,
        stats.row_cache_hits,
        stats.row_cache_misses,
        stats.row_cache_invalidate_visits,
        stats.recolor_glyphs_visited,
        stats.full_frames,
        stats.vertex_bytes,
        stats.index_bytes,
        stats.software_frames,
        stats.gpu_frames,
        stats.damaged_frames,
    ]
}

/// Assembled frames recorded in `assembly_us`: its sample count, not its durations.
fn assembly_samples(stats: &FrameStats) -> u64 {
    stats.assembly_buckets.iter().sum()
}

/// One renderer's two-frame scene, through the releasing call with a held source or the wrapper.
struct Scene {
    grid: Arc<Mutex<Grid>>,
    log: Arc<Mutex<Vec<&'static str>>>,
    held: bool,
}

impl Scene {
    fn new(held: bool) -> Self {
        Scene {
            grid: Arc::new(Mutex::new(text_grid("counters"))),
            log: Arc::new(Mutex::new(Vec::new())),
            held,
        }
    }

    /// Frame 0 draws the text; frame 1 changes a row first. Both must present.
    fn frame(&self, renderer: &mut GpuRenderer, frame: u32) -> Result<(), String> {
        if frame == 1 {
            let mut changed = self.grid.lock().unwrap();
            changed.goto(1, 0);
            for character in "changed".chars() {
                changed.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
        }
        let outcome = if self.held {
            let FrameOutcome { outcome, receipts } = release(
                renderer,
                Held { grid: self.grid.lock().unwrap(), log: Arc::clone(&self.log) },
            );
            let mut applied = self.grid.lock().unwrap();
            for receipt in &receipts {
                let _cleared = receipt.try_apply(&mut applied);
            }
            outcome
        } else {
            wrapped(renderer, &mut self.grid.lock().unwrap())
        };
        check(
            matches!(outcome, PresentOutcome::Presented),
            &format!("counter frame {frame} held={}: {outcome:?}", self.held),
        )
    }
}

/// A fresh renderer that counts into its own sink.
fn counting_renderer(active: &ActiveEventLoop) -> Result<GpuRenderer, String> {
    let mut renderer = fresh_renderer(active, "release-counters", true)?;
    renderer.set_frame_counting(true);
    Ok(renderer)
}

/// Test 10: on fresh renderers the releasing call with a held source and the wrapper record equal
/// deterministic counts and one assembly sample per frame; two counting renderers alternated
/// A, B, A, B each equal their solo run; a renderer without a sink records nothing.
fn counters_match_on_fresh_renderers(active: &ActiveEventLoop) -> Result<(), String> {
    let mut solo_wrapper = counting_renderer(active)?;
    let wrapper_scene = Scene::new(false);
    let mut solo_held = counting_renderer(active)?;
    let held_scene = Scene::new(true);
    for frame in 0..2 {
        wrapper_scene.frame(&mut solo_wrapper, frame)?;
    }
    for frame in 0..2 {
        held_scene.frame(&mut solo_held, frame)?;
    }
    let (wrapper_stats, held_stats) = (solo_wrapper.frame_stats(), solo_held.frame_stats());
    check(
        deterministic(&wrapper_stats) == deterministic(&held_stats),
        &format!(
            "deterministic counts: {:?} vs {:?}",
            deterministic(&wrapper_stats),
            deterministic(&held_stats)
        ),
    )?;
    check(
        assembly_samples(&wrapper_stats) == 2 && assembly_samples(&held_stats) == 2,
        "one assembly sample per assembled frame on both paths",
    )?;
    check(held_stats.assembly_sum_us > 0, "assembly time is recorded")?;

    let (mut first, mut second) = (counting_renderer(active)?, counting_renderer(active)?);
    let (first_scene, second_scene) = (Scene::new(false), Scene::new(true));
    for frame in 0..2 {
        first_scene.frame(&mut first, frame)?;
        second_scene.frame(&mut second, frame)?;
    }
    check(
        deterministic(&first.frame_stats()) == deterministic(&wrapper_stats)
            && deterministic(&second.frame_stats()) == deterministic(&held_stats),
        "each alternated renderer's sink equals its solo run",
    )?;
    check(
        assembly_samples(&first.frame_stats()) == 2 && assembly_samples(&second.frame_stats()) == 2,
        "neither alternated sink holds the other's assembly samples",
    )?;

    let mut silent = fresh_renderer(active, "release-counters", true)?;
    Scene::new(true).frame(&mut silent, 0)?;
    check(silent.frame_stats() == FrameStats::ZERO, "a renderer without a sink records nothing")
}

fn run(active: &ActiveEventLoop) -> Result<(), String> {
    lend_drop_present(active)?;
    stopped_device_returns_its_typed_exits(active)?;
    wrapper_matches_the_releasing_call(active)?;
    wrapper_keeps_dirt_after_a_surface_retry(active)?;
    counters_match_on_fresh_renderers(active)
}

/// The releasing call lends once, releases before presentation, keeps its typed exits, matches
/// the compatibility wrapper's pixels and dirt, and counts as the wrapper does.
#[test]
fn windows_release_before_present_on_a_real_renderer() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("release-before-present event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
