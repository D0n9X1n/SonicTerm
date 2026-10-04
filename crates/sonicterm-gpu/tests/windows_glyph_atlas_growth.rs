#![cfg(target_os = "windows")]
//! Glyph atlas growth on a real renderer: a frame whose glyphs grow the atlas mid-assembly retries
//! without a reset or a second rasterization, the GPU upload follows the new size (the software
//! presenter keeps its 1x1 placeholder), the next presented frame draws a fresh renderer's pixels,
//! and every growth episode is counted once, with one growth-to-present sample per presented frame.
//!
//! The incremental cases rewrite one persistent grid, so its revision and dirty generation advance
//! every frame and no frame takes the unchanged-key shortcut; each asserts that its frame assembled.

use std::{path::PathBuf, sync::Arc};

use sonicterm_gpu::core::{
    GlyphAtlasStart, GpuRenderer, PresentOutcome, RendererSettings, SurfaceAppearance,
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

/// Cells of the sliding window the incremental cases draw: the newest key and seven retained ones.
const SLIDING_COLS: u16 = 8;
/// Body size of the incremental cases, large enough that the 256 start fills within 376 keys.
const SLIDING_FONT_PX: f32 = 40.0;
/// Body size of the two-growth case, so one screen of distinct glyphs outgrows 512 at once.
const LARGE_FONT_PX: f32 = 160.0;

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

/// A visible window of `size` and a counting renderer on it whose glyph atlas starts at the 256
/// floor, presenting through GDI (`software`, `Force`) or wgpu (`Off`, which never degrades). The
/// shipped face keeps glyph sizes the same on every host. A wgpu renderer is skipped as
/// `HOST_INCAPABLE: ...` only when the host enumerates no adapter.
fn counting_renderer(
    active: &ActiveEventLoop,
    role: &'static str,
    software: bool,
    font_px: f32,
    size: PhysicalSize<u32>,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(size)
                    .with_visible(true)
                    .with_title(role),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs = [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let mode = if software { SoftwareRenderMode::Force } else { SoftwareRenderMode::Off };
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: font_px,
        line_height_mult: 1.0,
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
        glyph_atlas_start: GlyphAtlasStart::Minimum,
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &Theme::default(), settings) {
        Ok(renderer) => renderer,
        Err(error) if !software => return Err(wgpu_construction_failure(error)),
        Err(error) => return Err(error.to_string()),
    };
    check(
        renderer.is_software_render_degraded() == software,
        &format!("{role}: the renderer presents through GDI={software}"),
    )?;
    check(
        renderer.glyph_atlas_facts().dim == sonicterm_text::glyph_atlas::MIN_ATLAS_DIM,
        &format!("{role}: the atlas starts at the 256 floor"),
    )?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    renderer.set_frame_counting(true);
    Ok((window, renderer))
}

/// The `index`th distinct ASCII fast-path key: a printable character in one of four style faces.
fn key_cell(index: usize) -> (char, CellFlags) {
    let printable: Vec<char> = ('!'..='~').collect();
    let character = printable[index % printable.len()];
    let flags = match (index / printable.len()) % 4 {
        0 => CellFlags::empty(),
        1 => CellFlags::BOLD,
        2 => CellFlags::ITALIC,
        _ => CellFlags::BOLD | CellFlags::ITALIC,
    };
    (character, flags)
}

/// Distinct keys the incremental cases may draw before they give up on growing the atlas.
fn key_count() -> usize {
    ('!'..='~').count() * 4
}

/// The one-row grid the incremental cases rewrite for every frame.
fn sliding_grid() -> Grid {
    Grid::new(SLIDING_COLS, 1)
}

/// Rewrite `grid` to hold keys `newest - 7 ..= newest` and mark it dirty. Writing the cells
/// advances the grid's revision and dirty generation, so the frame key changes even when the
/// visible keys repeat, and the next frame assembles.
fn slide_to(grid: &mut Grid, newest: usize) {
    grid.goto(0, 0);
    let first = newest.saturating_sub(usize::from(SLIDING_COLS) - 1);
    for index in first..=newest {
        let (character, flags) = key_cell(index);
        grid.put_char(character, Color::Default, Color::Default, flags);
    }
    grid.mark_all_dirty();
}

/// Frames assembled so far: every assembly lands in exactly one `assembly_us` bucket, and the
/// unchanged-key shortcut and the cached reblit assemble nothing.
fn assembled(stats: &FrameStats) -> u64 {
    stats.assembly_buckets.iter().sum()
}

/// A grid filling `window` at the renderer's cell size, every cell a distinct key.
fn filled_grid(renderer: &GpuRenderer, window: &Window) -> Grid {
    let (cell_w, cell_h) = renderer.cell_size();
    let size = window.inner_size();
    let cols = ((size.width as f32 / cell_w).floor() as u16).max(1);
    let rows = ((size.height as f32 / cell_h).floor() as u16).max(1);
    let mut grid = Grid::new(cols, rows);
    for row in 0..rows {
        grid.goto(row, 0);
        for col in 0..cols {
            let (character, flags) =
                key_cell(usize::from(row) * usize::from(cols) + usize::from(col));
            grid.put_char(character, Color::Default, Color::Default, flags);
        }
    }
    grid.mark_all_dirty();
    grid
}

/// One frame of `grid` filling `window`, through the compatibility wrapper.
fn frame(renderer: &mut GpuRenderer, window: &Window, grid: &mut Grid) -> PresentOutcome {
    let size = window.inner_size();
    let theme = Theme::default();
    let tabs = TabBar::new();
    let fonts = renderer.begin_frame_fonts();
    let mut panes = [PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: size.width, h: size.height },
        grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    renderer.render_with_outcome(
        &fonts, &mut panes, &theme, false, None, None, &tabs, false, None, None, None, None, None,
        None, None,
    )
}

/// Draw `grid` until a frame presents, at most four tries (one retry is all a frame may need).
fn present(renderer: &mut GpuRenderer, window: &Window, grid: &mut Grid) -> Result<(), String> {
    for _ in 0..4 {
        grid.mark_all_dirty();
        if matches!(frame(renderer, window, grid), PresentOutcome::Presented) {
            return Ok(());
        }
    }
    Err(String::from("the frame never presented"))
}

/// Growth-to-present samples recorded so far.
fn samples(stats: &FrameStats) -> u64 {
    stats.atlas_growth_to_present_buckets.iter().sum()
}

/// The counts the memory line and the counters reconcile: the atlas's own growth count, which the
/// memory snapshot prints as `glyph_atlas_growths`, must equal the counted growths since the
/// renderer was built, which is when it started counting.
fn reconciles(renderer: &GpuRenderer) -> Result<(), String> {
    let facts = renderer.glyph_atlas_facts().growths;
    let counted = renderer.frame_stats().glyph_atlas_growths;
    check(facts == counted, &format!("memory line growths {facts} == counted {counted}"))
}

/// What the renderer showed just before the frame that grew its atlas.
struct BeforeGrowth {
    newest: usize,
    resets: u64,
    misses: u64,
    resident: usize,
    dim: u32,
    stats: FrameStats,
}

/// Draw the sliding window on `grid` one new key per frame until the frame that inserts a key would
/// grow the atlas; present everything before it and return the state just before that frame. One
/// new key per frame can grow the atlas only once, so the growing frame grows it exactly once.
/// Every frame must assemble, so none can pass by reusing the previous frame.
fn slide_until_growth(
    renderer: &mut GpuRenderer,
    window: &Window,
    grid: &mut Grid,
) -> Result<(BeforeGrowth, PresentOutcome), String> {
    for newest in 0..key_count() {
        let before = BeforeGrowth {
            newest,
            resets: renderer.__test_glyph_atlas_resets(),
            misses: renderer.__test_glyph_atlas_misses(),
            resident: renderer.glyph_atlas_len(),
            dim: renderer.glyph_atlas_facts().dim,
            stats: renderer.frame_stats(),
        };
        slide_to(grid, newest);
        let outcome = frame(renderer, window, grid);
        let assembled_now = assembled(&renderer.frame_stats()) - assembled(&before.stats);
        check(
            assembled_now == 1,
            &format!("key {newest}: the frame assembled once, not {assembled_now}: {outcome:?}"),
        )?;
        if renderer.glyph_atlas_facts().dim != before.dim {
            // When: the dimension moved, this frame grew the atlas; the caller checks its retry.
            return Ok((before, outcome));
        }
        check(
            matches!(outcome, PresentOutcome::Presented),
            &format!("key {newest} presented without growth: {outcome:?}"),
        )?;
    }
    Err(format!("{} keys at {SLIDING_FONT_PX}px never grew the 256 atlas", key_count()))
}

/// Tests 7 and 8 (case a of test 15): the growing frame retries through the growth path, with no
/// reset and no tile rasterized again; the GPU upload follows the new size under wgpu and stays 1x1
/// under GDI; the retried frame presents; one growth and exactly one growth-to-present sample are
/// counted. Under GDI the presented frame equals a fresh renderer's pixels for the same cells.
fn growth_retry(active: &ActiveEventLoop, software: bool) -> Result<(), String> {
    let size = PhysicalSize::new(480, 120);
    let role = if software { "atlas-growth-gdi" } else { "atlas-growth-wgpu" };
    let (window, mut renderer) =
        match counting_renderer(active, role, software, SLIDING_FONT_PX, size) {
            Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
                // When: the host cannot create a wgpu renderer, report the capability, not a pass.
                println!("capability=HOST_INCAPABLE case={role} reason={reason}");
                return Ok(());
            }
            other => other?,
        };
    let mut grid = sliding_grid();
    let (before, outcome) = slide_until_growth(&mut renderer, &window, &mut grid)?;
    check(
        matches!(outcome, PresentOutcome::AtlasRetry),
        &format!("{role}: the growing frame retries: {outcome:?}"),
    )?;
    check(
        renderer.__test_glyph_atlas_resets() == before.resets,
        &format!("{role}: the growth retry never resets the atlas in place"),
    )?;
    let ((cpu_w, cpu_h), gpu) = renderer.__test_glyph_atlas_dimensions();
    check(
        (cpu_w, cpu_h) == (before.dim * 2, before.dim * 2),
        &format!("{role}: the atlas doubled once: {cpu_w}x{cpu_h} from {}", before.dim),
    )?;
    let expected_gpu = if software { (1, 1) } else { (cpu_w, cpu_h) };
    check(gpu == expected_gpu, &format!("{role}: the upload is {gpu:?}, want {expected_gpu:?}"))?;
    let assembled_before_retry = assembled(&renderer.frame_stats());
    grid.mark_all_dirty();
    let retried = frame(&mut renderer, &window, &mut grid);
    check(
        matches!(retried, PresentOutcome::Presented),
        &format!("{role}: the retried frame presents: {retried:?}"),
    )?;
    check(
        assembled(&renderer.frame_stats()) == assembled_before_retry + 1,
        &format!("{role}: the retried frame assembled"),
    )?;
    check(
        renderer.__test_glyph_atlas_dimensions().1 == expected_gpu,
        &format!("{role}: the upload keeps its size through the present"),
    )?;
    // Only the new key missed: the seven retained tiles were hits in both the growing and the
    // retried frame, so none was rasterized again.
    let missed = renderer.__test_glyph_atlas_misses() - before.misses;
    let added = renderer.glyph_atlas_len() - before.resident;
    check(missed == 1 && added == 1, &format!("{role}: misses +{missed}, resident +{added}"))?;
    let stats = renderer.frame_stats();
    check(
        stats.glyph_atlas_growths == before.stats.glyph_atlas_growths + 1,
        &format!("{role}: one growth counted"),
    )?;
    check(samples(&stats) == samples(&before.stats) + 1, &format!("{role}: one sample"))?;
    check(stats.atlas_growth_abandoned == 0, &format!("{role}: nothing abandoned"))?;
    reconciles(&renderer)?;
    if software {
        // A fresh renderer draws the same eight cells from an atlas that never grew.
        let (fresh_window, mut fresh) =
            counting_renderer(active, "atlas-growth-fresh", true, SLIDING_FONT_PX, size)?;
        check(fresh_window.inner_size() == window.inner_size(), "both windows are one size")?;
        let mut fresh_grid = sliding_grid();
        slide_to(&mut fresh_grid, before.newest);
        present(&mut fresh, &fresh_window, &mut fresh_grid)?;
        check(fresh.glyph_atlas_facts().growths == 0, "the fresh atlas never grew")?;
        let (grown, expected) =
            (software_pixels(&renderer, &window), software_pixels(&fresh, &window));
        check(
            !grown.is_empty() && grown == expected,
            "the grown atlas draws a fresh renderer's pixels",
        )?;
    }
    Ok(())
}

/// Every pixel of a renderer's software frame, as BGRA.
fn software_pixels(renderer: &GpuRenderer, window: &Window) -> Vec<[u8; 4]> {
    let size = window.inner_size();
    (0..size.height)
        .flat_map(|pixel_y| (0..size.width).map(move |pixel_x| (pixel_x, pixel_y)))
        .filter_map(|(pixel_x, pixel_y)| {
            renderer.__test_software_frame_pixel_bgra(pixel_x, pixel_y)
        })
        .collect()
}

/// Test 15b: a real growth and a real eviction in one assembly take the reset path. A probe
/// renderer finds the growing key and the resident count just before it; this renderer replays the
/// same keys, so it packs the same tiles, then lowers its entry cap to two above that count and
/// draws a window whose three newest keys are new: the first grows the atlas, the second fills it to
/// the cap, and the third evicts. The growth is still counted once, the reset frame keeps its
/// timing, and the next presented frame records the one sample; nothing is abandoned.
fn growth_with_eviction(active: &ActiveEventLoop) -> Result<(), String> {
    let size = PhysicalSize::new(480, 120);
    let growing = {
        let (probe_window, mut probe) =
            counting_renderer(active, "atlas-growth-probe", true, SLIDING_FONT_PX, size)?;
        slide_until_growth(&mut probe, &probe_window, &mut sliding_grid())?.0
    };
    let (window, mut renderer) =
        counting_renderer(active, "atlas-growth-eviction", true, SLIDING_FONT_PX, size)?;
    let mut grid = sliding_grid();
    for newest in 0..growing.newest {
        slide_to(&mut grid, newest);
        present(&mut renderer, &window, &mut grid)?;
    }
    check(
        renderer.glyph_atlas_len() == growing.resident && renderer.glyph_atlas_facts().growths == 0,
        "precondition: the replay packed the probe's tiles without growing",
    )?;
    check(renderer.glyph_atlas_facts().fit != "evicted", "precondition: nothing evicted yet")?;
    let (resets, stats) = (renderer.__test_glyph_atlas_resets(), renderer.frame_stats());
    renderer.__set_glyph_atlas_entry_cap(growing.resident + 2);
    slide_to(&mut grid, growing.newest + 2);
    let outcome = frame(&mut renderer, &window, &mut grid);
    renderer.__set_glyph_atlas_entry_cap(sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES);
    check(matches!(outcome, PresentOutcome::AtlasRetry), &format!("reset frame: {outcome:?}"))?;
    check(assembled(&renderer.frame_stats()) == assembled(&stats) + 1, "the frame assembled")?;
    check(renderer.glyph_atlas_facts().growths == 1, "the frame grew the atlas once")?;
    // The reset zeroes the eviction counter; the fit label keeps that an eviction happened.
    check(renderer.glyph_atlas_facts().fit == "evicted", "the same frame evicted")?;
    check(renderer.__test_glyph_atlas_resets() == resets + 1, "the frame took the reset path")?;
    let counted = renderer.frame_stats();
    check(counted.glyph_atlas_growths == stats.glyph_atlas_growths + 1, "growth counted")?;
    check(samples(&counted) == samples(&stats), "no sample before a present")?;
    present(&mut renderer, &window, &mut grid)?;
    let after = renderer.frame_stats();
    check(samples(&after) == samples(&stats) + 1, "the next present records one sample")?;
    check(after.atlas_growth_abandoned == 0, "a reset abandons nothing")?;
    reconciles(&renderer)
}

/// Test 15c: growth, then device loss before any present: the growth is counted, its timing is
/// abandoned, and no sample is recorded.
fn growth_then_device_loss(active: &ActiveEventLoop) -> Result<(), String> {
    let size = PhysicalSize::new(480, 120);
    let (window, mut renderer) =
        counting_renderer(active, "atlas-growth-loss", true, SLIDING_FONT_PX, size)?;
    let mut grid = sliding_grid();
    let (before, outcome) = slide_until_growth(&mut renderer, &window, &mut grid)?;
    check(matches!(outcome, PresentOutcome::AtlasRetry), &format!("growth: {outcome:?}"))?;
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    grid.mark_all_dirty();
    let stopped = frame(&mut renderer, &window, &mut grid);
    check(
        matches!(stopped, PresentOutcome::RenderingUnavailable(_)),
        &format!("the stopped frame: {stopped:?}"),
    )?;
    let stats = renderer.frame_stats();
    check(stats.glyph_atlas_growths == before.stats.glyph_atlas_growths + 1, "growth counted")?;
    check(stats.atlas_growth_abandoned == 1, "the pending timing is abandoned once")?;
    check(samples(&stats) == samples(&before.stats), "no sample without a present")?;
    reconciles(&renderer)
}

/// Test 15d: one frame of large distinct glyphs grows the atlas at least twice; both growths are
/// counted, and the next presented frame records exactly one sample for the episode.
fn two_growths_in_one_frame(active: &ActiveEventLoop) -> Result<(), String> {
    let size = PhysicalSize::new(1000, 700);
    let (window, mut renderer) =
        counting_renderer(active, "atlas-growth-twice", true, LARGE_FONT_PX, size)?;
    let mut grid = filled_grid(&renderer, &window);
    let outcome = frame(&mut renderer, &window, &mut grid);
    let growths = renderer.glyph_atlas_facts().growths;
    check(
        growths >= 2,
        &format!(
            "precondition: one frame of {}x{} cells at {LARGE_FONT_PX}px grew {growths} times; \
             enlarge the window or the font",
            grid.cols, grid.rows
        ),
    )?;
    check(matches!(outcome, PresentOutcome::AtlasRetry), &format!("growth: {outcome:?}"))?;
    check(renderer.frame_stats().glyph_atlas_growths == growths, "every growth is counted")?;
    present(&mut renderer, &window, &mut grid)?;
    let stats = renderer.frame_stats();
    check(samples(&stats) == 1, &format!("one sample per episode, got {}", samples(&stats)))?;
    check(stats.atlas_growth_abandoned == 0, "nothing abandoned")?;
    reconciles(&renderer)
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for (name, case) in [
        ("growth retry wgpu", (|active| growth_retry(active, false)) as fn(&ActiveEventLoop) -> _),
        ("growth retry gdi", |active| growth_retry(active, true)),
        ("growth with eviction", growth_with_eviction),
        ("growth then device loss", growth_then_device_loss),
        ("two growths in one frame", two_growths_in_one_frame),
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

/// A glyph atlas that grows during a frame retries without a reset, follows its new size on both
/// presenters, draws a fresh renderer's pixels and counts every growth episode once.
#[test]
fn windows_glyph_atlas_growth_on_a_real_renderer() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("glyph atlas growth event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
