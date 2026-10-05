//! Frame-scratch, tab-title, palette and search-run reuse on a real renderer.
//!
//! Forced assemblies of an unchanged or slightly edited scene reuse the renderer's frame scratch
//! (fewer allocations than with reuse off, same pixels), keep it across failed and retried
//! frames, and hold it within its cap. Warm tab titles and search-overlay runs draw without
//! shaping, every face replacement makes them prepare again, and their pixels equal a renderer
//! with reuse off, on wgpu and on GDI. The UI palette follows the theme passed to `render`.
//! A covered-window trim releases what the renderer can rebuild, restores on the next present on
//! wgpu and GDI, and leaves a pending glyph-atlas retry to settle on that present.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sonicterm_gpu::core::{
    settle_borrowed_frame, unpad_readback_rows, GlyphAtlasStart, GpuRenderer, PresentOutcome,
    RendererSettings, SurfaceAppearance,
};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{CursorShape, ScrollbarMode, SoftwareRenderMode},
            theme::{Hex, Theme},
        },
        grid::grid::{CellFlags, Color, Grid, UnderlineStyle},
        ui::{
            search::SearchState,
            tabs::{Tab, TabBar},
        },
    },
    BorrowedSource, CursorStyle, PaneRender, PixelRect,
};
use sonicterm_types::{ClassCoverage, ResourceClass};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

/// Allocations made while a counting thread had counting on.
static COUNTED_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Whether this thread's allocations are counted.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

// SAFETY: every operation forwards its exact pointer, layout and size to `System`; the
// bookkeeping is a const-initialized thread-local read and an atomic add, which never allocate.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` is forwarded unchanged after allocation-free bookkeeping.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` is the valid layout received from the allocator caller.
        unsafe { System.alloc(layout) }
    }

    // SAFETY: `ptr` and `layout` are forwarded unchanged.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` are the matching pair received from the allocator caller.
        unsafe { System.dealloc(ptr, layout) }
    }

    // SAFETY: `ptr`, `layout` and `new_size` are forwarded unchanged after bookkeeping.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: all arguments are the exact valid values received from the allocator caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Count one allocation when this thread is counting; `try_with` tolerates thread teardown.
fn note_allocation() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        COUNTED_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Allocations `work` makes on this thread, with its output.
fn allocations_of<Output>(work: impl FnOnce() -> Output) -> (Output, usize) {
    let before = COUNTED_ALLOCATIONS.load(Ordering::Relaxed);
    COUNTING.with(|counting| counting.set(true));
    let output = work();
    COUNTING.with(|counting| counting.set(false));
    (output, COUNTED_ALLOCATIONS.load(Ordering::Relaxed) - before)
}

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

/// Whether this host enumerates no wgpu adapter at all: the only reason to skip.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// A visible `width_px` by `height_px` window and a counting renderer on it in `mode`, built with
/// `theme`, with the tab bar shown when `tab_bar`; `Err` names a host with no adapter.
fn renderer(
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
    theme: &Theme,
    size_px: (u32, u32),
    tab_bar: bool,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(size_px.0, size_px.1))
                    .with_visible(true)
                    .with_title("frame-reuse"),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs = [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: 16.0,
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
        role: "frame-reuse",
        glyph_atlas_start: GlyphAtlasStart::Normal,
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, theme, settings) {
        Ok(renderer) => renderer,
        Err(error) if host_has_no_adapter() => {
            // When: host_has_no_adapter holds, no renderer can exist on this host.
            return Err(format!("HOST_INCAPABLE: no wgpu adapter: {error}"));
        }
        Err(error) => return Err(format!("renderer construction failed: {error}")),
    };
    renderer.set_tab_bar_visible(tab_bar);
    renderer.set_cursor_blink(false);
    renderer.set_cursor_shape(CursorShape::Block);
    renderer.set_window_focused(true);
    renderer.set_frame_counting(true);
    renderer.__enable_retained_frame_readback();
    Ok((window, renderer))
}

/// One pane of a scene: its id, rect and grid.
struct ScenePane {
    id: u64,
    rect: PixelRect,
    grid: Grid,
}

/// What a case draws besides the renderer's own state.
struct Scene {
    panes: Vec<ScenePane>,
    tabs: TabBar,
    search: Option<SearchState>,
    theme: Theme,
    /// The font-fallback generation the last drawn frame applied.
    generation: Option<u64>,
}

/// Write `text` from `(row, col)` in default colours.
fn write(grid: &mut Grid, row: u16, col: u16, text: &str) {
    grid.goto(row, col);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A grid filling `rect` with a short line of text on every row.
fn text_grid(renderer: &GpuRenderer, rect: PixelRect) -> Grid {
    let (cell_w, cell_h) = renderer.cell_size();
    let cols = ((rect.w as f32 / cell_w).floor() as u16).max(1);
    let rows = ((rect.h as f32 / cell_h).floor() as u16).max(1);
    let mut grid = Grid::new(cols, rows);
    for row in 0..rows {
        write(&mut grid, row, 0, &format!("row {row} abc"));
    }
    grid
}

/// One pane over the surface below the tab bar, with `titles` as tabs.
fn single_scene(renderer: &GpuRenderer, titles: &[&str]) -> Scene {
    let (width_px, height_px) = renderer.surface_size();
    let bar_px = renderer.tab_bar_logical_height().ceil() as u32;
    let rect = PixelRect {
        x: 0,
        y: i32::try_from(bar_px).unwrap_or(i32::MAX),
        w: width_px,
        h: height_px.saturating_sub(bar_px),
    };
    let mut tabs = TabBar::new();
    for title in titles {
        tabs.push(Tab::new(*title));
    }
    Scene {
        panes: vec![ScenePane { id: 1, rect, grid: text_grid(renderer, rect) }],
        tabs,
        search: None,
        theme: Theme::default(),
        generation: None,
    }
}

/// Draw `scene` once through the releasing call, measuring tab widths first as the App does.
fn frame(renderer: &mut GpuRenderer, scene: &mut Scene) -> PresentOutcome {
    let fonts = renderer.begin_frame_fonts();
    scene.generation = Some(fonts.generation());
    let _ = renderer.measure_tab_widths(&fonts, &mut scene.tabs, false, false, Instant::now());
    let mut panes: Vec<PaneRender<'_>> = scene
        .panes
        .iter_mut()
        .enumerate()
        .map(|(index, pane)| PaneRender {
            id: pane.id,
            rect_px: pane.rect,
            grid: &mut pane.grid,
            viewport_top_abs: None,
            is_active: index == 0,
            cursor_style: CursorStyle::BlockSteady,
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: Vec::new(),
        })
        .collect();
    let released = renderer.render_releasing(
        &fonts,
        BorrowedSource(&mut panes[..]),
        &scene.theme,
        false,
        None,
        None,
        &scene.tabs,
        false,
        scene.search.as_ref(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    settle_borrowed_frame(released, &mut panes)
}

/// Draw `scene` until a frame presents, at most four tries.
fn present(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<(), String> {
    for _ in 0..4 {
        if matches!(frame(renderer, scene), PresentOutcome::Presented) {
            return Ok(());
        }
    }
    Err(String::from("the frame never presented"))
}

/// Draw `scene` on `renderer` until its applied font-fallback generation has held for a second
/// with no tofu (five seconds while tofu remains), within twenty seconds, and return that
/// generation. Two renderers discover fallback faces independently, so a comparison of their
/// pixels starts only after each has settled.
fn settle_fallback(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<u64, String> {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    let mut held: Option<(u64, Instant)> = None;
    loop {
        forced(renderer, scene)?;
        let generation = scene.generation.ok_or("a drawn frame records its fonts")?;
        let tofu =
            !renderer.last_missing_tofu().is_empty() || !renderer.last_missing_chrome().is_empty();
        let since = match held {
            Some((kept, since)) if kept == generation => since,
            _ => {
                // A first observation or a newly applied generation restarts the wait.
                held = Some((generation, Instant::now()));
                Instant::now()
            }
        };
        let needed = std::time::Duration::from_secs(if tofu { 5 } else { 1 });
        if since.elapsed() >= needed {
            return Ok(generation);
        }
        check(
            Instant::now() < deadline,
            &format!("the font fallback settles; generation {generation}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Present `scene` with the retained frame forgotten, so the frame assembles even unchanged.
fn forced(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<(), String> {
    renderer.invalidate_retained_frame();
    present(renderer, scene)
}

/// `counts` read before and after one forced frame that presented in exactly one render attempt,
/// running `prepare` before each try, at most four tries. An atlas retry assembles a frame twice
/// and notes its chrome twice, so an exact per-frame count is read only across a single attempt.
fn counted_forced<Counts>(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    prepare: impl Fn(&mut GpuRenderer),
    counts: fn(&GpuRenderer) -> Counts,
) -> Result<(Counts, Counts), String> {
    for _ in 0..4 {
        prepare(renderer);
        let attempts_before = renderer.frame_stats().attempts.attempts;
        let before = counts(renderer);
        forced(renderer, scene)?;
        if renderer.frame_stats().attempts.attempts - attempts_before == 1 {
            // When: one attempt drew the frame, no retry noted the frame's chrome a second time.
            return Ok((before, counts(renderer)));
        }
    }
    Err(String::from("no forced frame presented in a single render attempt"))
}

/// Check that `scene`'s last frame applied the `settled` fallback generation, so a pixel
/// comparison after it does not race a fallback face applied since settling.
fn check_settled(scene: &Scene, settled: u64, context: &str) -> Result<(), String> {
    check(
        scene.generation == Some(settled),
        &format!(
            "{context}: no fallback generation applied after settling ({:?}, settled {settled})",
            scene.generation
        ),
    )
}

/// The presented frame's pixels: the GDI frame on the software presenter, else the retained
/// wgpu frame read back.
fn pixels(renderer: &mut GpuRenderer) -> Result<Vec<u8>, String> {
    if renderer.is_software_render_degraded() && cfg!(target_os = "windows") {
        // When: the GDI presenter drew the frame, its software frame holds the pixels.
        return gdi_pixels(renderer);
    }
    let readback = renderer.__copy_retained_frame().ok_or("readback is enabled")?;
    let slice = readback.buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    readback
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll the readback: {error}"))?;
    let mapped = slice.get_mapped_range().map_err(|error| format!("map the readback: {error}"))?;
    let bytes = unpad_readback_rows(
        &mapped,
        readback.width_px,
        readback.height_px,
        readback.padded_row_bytes,
    );
    drop(mapped);
    readback.buffer.unmap();
    Ok(bytes)
}

/// Every BGRA byte of a GDI renderer's software frame. The hook exists only on Windows.
#[cfg(not(target_os = "windows"))]
fn gdi_pixels(_renderer: &GpuRenderer) -> Result<Vec<u8>, String> {
    Err(String::from("the GDI presenter exists only on Windows"))
}

/// Every BGRA byte of a GDI renderer's software frame, each read through the test hook.
#[cfg(target_os = "windows")]
fn gdi_pixels(renderer: &GpuRenderer) -> Result<Vec<u8>, String> {
    let (width, height) = renderer.surface_size();
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for pixel_y in 0..height {
        for pixel_x in 0..width {
            bytes.extend(
                renderer
                    .__test_software_frame_pixel_bgra(pixel_x, pixel_y)
                    .ok_or_else(|| format!("GDI pixel ({pixel_x}, {pixel_y}) is readable"))?,
            );
        }
    }
    Ok(bytes)
}

/// Both presenter modes this file compares on.
const MODES: [SoftwareRenderMode; 2] = [SoftwareRenderMode::Off, SoftwareRenderMode::Force];

type Case = fn(&ActiveEventLoop) -> Result<(), String>;

/// T7: forced assemblies reuse the frame scratch: ten forced frames with one changed cell each
/// allocate fewer times with reuse on than off, by at least one per non-empty scratch vector per
/// frame, and the last frames' pixels are equal. Each renderer settles its font fallback first.
fn assembled_frames_reuse_scratch(active: &ActiveEventLoop) -> Result<(), String> {
    let mut runs = Vec::new();
    for reuse in [true, false] {
        let (_window, mut renderer) =
            renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
        renderer.__set_frame_reuse(reuse);
        let mut scene = single_scene(&renderer, &["shell"]);
        let settled = settle_fallback(&mut renderer, &mut scene)?;
        // Every edit's glyphs are drawn once first, so no measured frame rasterizes or grows the
        // atlas and both sides count only their assembly.
        for index in 0..10u16 {
            write(&mut scene.panes[0].grid, 0, 0, &format!("edit {index:02}"));
            forced(&mut renderer, &mut scene)?;
        }
        for _ in 0..5 {
            forced(&mut renderer, &mut scene)?;
        }
        let vectors = renderer.retained_amounts().frame_scratch.items;
        let (outcome, allocations) = allocations_of(|| -> Result<(), String> {
            for index in 0..10u16 {
                write(&mut scene.panes[0].grid, 0, 0, &format!("edit {index:02}"));
                forced(&mut renderer, &mut scene)?;
            }
            Ok(())
        });
        outcome?;
        check_settled(&scene, settled, &format!("reuse {reuse}"))?;
        runs.push((allocations, vectors, pixels(&mut renderer)?));
    }
    let (with_reuse, vectors, reused_pixels) = &runs[0];
    let (without_reuse, _, cold_pixels) = &runs[1];
    check(*vectors > 0, "the warm scratch holds allocated vectors")?;
    check(
        with_reuse + 10 * vectors <= *without_reuse,
        &format!("reuse saves a vector per frame: {with_reuse} vs {without_reuse} ({vectors})"),
    )?;
    check(reused_pixels == cold_pixels, "reuse draws the same pixels")
}

/// One forced frame's allocations and what it changed: whether it drew in a single attempt with
/// no row-cache miss and no atlas growth (a steady frame), and the scratch bytes it left.
struct ForcedFrame {
    allocations: usize,
    steady: bool,
    scratch_bytes: usize,
    figures: String,
}

/// Draw one forced frame of `scene`, counting its allocations and reading the identity figures
/// that say whether it refilled a cache. Atlas growths are counted after assembly in the same
/// render call, so the growth delta is exact; unchanged atlas bytes (pixels plus the dirty list)
/// are a stricter stability condition on top of it.
fn measured_forced(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<ForcedFrame, String> {
    let before = renderer.frame_stats();
    let atlas_before = renderer.retained_amounts().glyph_atlas.bytes;
    let resets_before = renderer.__test_glyph_atlas_resets();
    let (outcome, allocations) = allocations_of(|| forced(renderer, scene));
    outcome?;
    let after = renderer.frame_stats();
    let atlas_after = renderer.retained_amounts().glyph_atlas.bytes;
    let attempts = after.attempts.attempts - before.attempts.attempts;
    let row_misses = after.row_cache_misses - before.row_cache_misses;
    let growths = after.glyph_atlas_growths - before.glyph_atlas_growths;
    let resets = renderer.__test_glyph_atlas_resets() - resets_before;
    let scratch_bytes = renderer.retained_amounts().frame_scratch.bytes;
    Ok(ForcedFrame {
        allocations,
        steady: attempts == 1
            && row_misses == 0
            && growths == 0
            && atlas_after == atlas_before
            && resets == 0,
        scratch_bytes,
        figures: format!(
            "allocations {allocations}, attempts {attempts}, row misses {row_misses}, row hits {}, \
             atlas growths {growths}, atlas bytes {atlas_before} -> {atlas_after}, resets {resets}, \
             scratch {scratch_bytes}",
            after.row_cache_hits - before.row_cache_hits
        ),
    })
}

/// Frames a recovery may take to become steady again. After an atlas reset the first frame
/// refills the row cache and its successful present clears it once more, as eviction resumes,
/// so the second frame refills it again; the third is steady.
const RECOVERY_FRAMES: usize = 4;

/// T8: after a failed assembly, a failed presentation and an atlas retry, the scratch keeps its
/// capacity, never grows while the caches refill, and the first steady frame allocates no more
/// than a steady frame before the fault and draws what a renderer with no failures draws. The
/// partial fallback exit has no renderer seam; the device-free driver covers it. Both renderers
/// settle their font fallback before any pixels are compared.
fn scratch_survives_failed_and_retried_frames(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut scene = single_scene(&candidate, &["shell"]);
    let mut oracle_scene = single_scene(&oracle, &["shell"]);
    let candidate_settled = settle_fallback(&mut candidate, &mut scene)?;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;
    // The most allocations a steady forced frame of this unchanged scene makes, over three
    // steady frames; a frame that still refilled a cache is not a steady sample.
    let mut steady_allocations = 0;
    let mut steady_samples = 0;
    let mut baseline_figures = Vec::new();
    for _ in 0..3 + RECOVERY_FRAMES {
        let measured = measured_forced(&mut candidate, &mut scene)?;
        baseline_figures.push(measured.figures.clone());
        if measured.steady {
            // When: `measured.steady` holds, the frame refilled nothing and is a baseline sample.
            steady_allocations = steady_allocations.max(measured.allocations);
            steady_samples += 1;
        }
        if steady_samples == 3 {
            break;
        }
    }
    check(
        steady_samples == 3,
        &format!("the settled scene draws three steady frames: {}", baseline_figures.join("; ")),
    )?;
    type Arm = fn(&mut GpuRenderer);
    let arms: [(&str, Arm); 3] = [
        ("failed assembly", GpuRenderer::__fail_next_assembly),
        ("failed presentation", GpuRenderer::__fail_next_present),
        ("atlas retry", GpuRenderer::__change_glyph_atlas_during_next_assembly),
    ];
    for (name, arm) in arms {
        let before = candidate.retained_amounts().frame_scratch.bytes;
        arm(&mut candidate);
        candidate.invalidate_retained_frame();
        let _ = frame(&mut candidate, &mut scene);
        let after = candidate.retained_amounts().frame_scratch.bytes;
        check(
            after == before,
            &format!("{name}: the scratch kept its capacity ({before} -> {after})"),
        )?;
        // Recovered frames refill the caches the fault dropped; the scratch grows in none of them,
        // and the first steady one is held to the steady bound.
        let mut recovery = Vec::new();
        let mut recovered = None;
        for _ in 0..RECOVERY_FRAMES {
            let measured = measured_forced(&mut candidate, &mut scene)?;
            recovery.push(measured.figures.clone());
            check(
                measured.scratch_bytes <= before,
                &format!("{name}: no recovered frame grows the scratch: {}", recovery.join("; ")),
            )?;
            if measured.steady {
                // When: `measured.steady` holds, the caches are full again and the frame is measured.
                recovered = Some(measured);
                break;
            }
        }
        let recovered = recovered.ok_or_else(|| {
            format!("{name}: a recovered frame is steady within bound: {}", recovery.join("; "))
        })?;
        check(
            recovered.allocations <= steady_allocations && recovered.scratch_bytes == before,
            &format!(
                "{name}: the recovered frame reuses the scratch (steady {steady_allocations}, \
                 scratch {before}): {}",
                recovery.join("; ")
            ),
        )?;
        forced(&mut oracle, &mut oracle_scene)?;
        forced(&mut oracle, &mut oracle_scene)?;
        check_settled(&scene, candidate_settled, &format!("{name} candidate"))?;
        check_settled(&oracle_scene, oracle_settled, &format!("{name} oracle"))?;
        check(
            pixels(&mut candidate)? == pixels(&mut oracle)?,
            &format!("{name}: the next frame draws as a renderer with no failure"),
        )?;
    }
    Ok(())
}

/// T9: tofu in every cell, curly underlines on every row, twelve panes then two: the reported
/// frame scratch stays within its class envelope.
fn scratch_caps_hold_after_unbounded_frames(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (960, 540), false)?;
    let ClassCoverage::UnchargedRetention { per_owner_bytes: cap } =
        ResourceClass::FrameScratch.coverage()
    else {
        return Err(String::from("FrameScratch records an uncharged envelope"));
    };
    let (width_px, height_px) = renderer.surface_size();
    let tiled = |count: u32| -> Vec<PixelRect> {
        let columns = count.min(4);
        let rows = count.div_ceil(columns);
        (0..count)
            .map(|index| PixelRect {
                x: i32::try_from((index % columns) * width_px / columns).unwrap_or(i32::MAX),
                y: i32::try_from((index / columns) * height_px / rows).unwrap_or(i32::MAX),
                w: width_px / columns,
                h: height_px / rows,
            })
            .collect()
    };
    for pane_count in [12u32, 2] {
        let mut scene = single_scene(&renderer, &["shell"]);
        scene.panes = tiled(pane_count)
            .into_iter()
            .enumerate()
            .map(|(index, rect)| {
                let mut grid = text_grid(&renderer, rect);
                for row in 0..grid.rows {
                    grid.goto(row, 0);
                    for _ in 0..grid.cols {
                        // A private-use character no face covers draws tofu, under a curly line.
                        grid.put_char_styled(
                            '\u{10fffd}',
                            Color::Default,
                            Color::Default,
                            CellFlags::UNDERLINE,
                            None,
                            UnderlineStyle::Curly,
                            None,
                        );
                    }
                }
                ScenePane { id: index as u64 + 1, rect, grid }
            })
            .collect();
        for _ in 0..3 {
            forced(&mut renderer, &mut scene)?;
        }
        let held = renderer.retained_amounts().frame_scratch.bytes;
        check(held <= cap, &format!("{pane_count} panes: {held} bytes within the {cap}-byte cap"))?;
    }
    Ok(())
}

/// The tab-title reuse and prepare counters now.
fn title_counts(renderer: &GpuRenderer) -> (u64, u64, u64) {
    let stats = renderer.frame_stats();
    (stats.tab_title_reuses, stats.tab_title_prepares, stats.shape_requests)
}

/// T15: three tabs, one cut, on a warm renderer: a forced assembly reuses all three titles with no
/// prepare and no shaping, and draws the same pixels; each face replacement then makes the next
/// assembly prepare every title again. The warm counts are read across a single render attempt.
fn renderer_reuses_titles_and_clears_on_face_replacement(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 240), true)?;
    let long = "a very long tab title that cannot fit its tab and must be cut short";
    let mut scene = single_scene(&renderer, &["shell", "logs", long]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let warm_pixels = pixels(&mut renderer)?;
    let (before, after) = counted_forced(&mut renderer, &mut scene, |_| {}, title_counts)?;
    check_settled(&scene, settled, "warm titles")?;
    check(after.0 - before.0 == 3, &format!("three warm reuses: {before:?} -> {after:?}"))?;
    check(after.1 == before.1, "no title prepares on a warm assembly")?;
    check(after.2 == before.2, "a warm assembly of ASCII rows and kept titles shapes nothing")?;
    check(pixels(&mut renderer)? == warm_pixels, "the reused titles draw the same pixels")?;

    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let adopted = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![fonts],
        16.0,
        72,
        1.0,
    )
    .map_err(|error| error.to_string())?;
    type Replace = Box<dyn Fn(&mut GpuRenderer)>;
    let replacements: [(&str, Replace); 4] = [
        ("set_font", Box::new(|renderer| renderer.set_font("Rec Mono St.Helens", 17.0, 1.0, 1.0))),
        (
            "adopted body stack",
            Box::new(move |renderer| {
                renderer.__test_adopt_body_font_stack("adopted", adopted.clone())
            }),
        ),
        (
            "same-scale rebuild",
            Box::new(|renderer| {
                let scale = renderer.scale_factor();
                renderer.force_rebuild_for_scale(scale);
            }),
        ),
        ("clear_shape_cache", Box::new(GpuRenderer::clear_shape_cache)),
    ];
    for (name, replace) in replacements {
        forced(&mut renderer, &mut scene)?;
        replace(&mut renderer);
        let before = title_counts(&renderer);
        forced(&mut renderer, &mut scene)?;
        let after = title_counts(&renderer);
        check(after.1 - before.1 >= 3, &format!("{name}: every title prepares again"))?;
    }
    Ok(())
}

/// T16: over 200 forced assemblies with changing titles, a renderer that keeps titles draws the
/// pixels of one that keeps none, on wgpu and on GDI. Both renderers settle their font fallback
/// first, and every comparison checks that neither applied a newer generation since.
fn cached_titles_draw_like_cold_titles(active: &ActiveEventLoop) -> Result<(), String> {
    let titles = ["shell", "logs", "build ~/src/sonicterm", "é ñ ü", "中文标题", "vim main.rs"];
    for mode in MODES {
        let (_window, mut warm) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        let (_cold_window, mut cold) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        cold.__set_frame_reuse(false);
        // Every title is drawn once on each renderer and its fallback faces settle first, so no
        // comparison races an asynchronous fallback discovery.
        let mut warm_all = single_scene(&warm, &titles);
        let warm_settled = settle_fallback(&mut warm, &mut warm_all)?;
        let mut cold_all = single_scene(&cold, &titles);
        let cold_settled = settle_fallback(&mut cold, &mut cold_all)?;
        for assembly in 0..200usize {
            let chosen: Vec<&str> =
                (0..3).map(|offset| titles[(assembly / 3 + offset) % titles.len()]).collect();
            let mut warm_scene = single_scene(&warm, &chosen);
            let mut cold_scene = single_scene(&cold, &chosen);
            forced(&mut warm, &mut warm_scene)?;
            forced(&mut cold, &mut cold_scene)?;
            check(
                (warm_scene.generation, cold_scene.generation)
                    == (Some(warm_settled), Some(cold_settled)),
                &format!(
                    "{mode:?} assembly {assembly}: no fallback generation applied after settling \
                     ({:?}, {:?})",
                    warm_scene.generation, cold_scene.generation
                ),
            )?;
            check(
                pixels(&mut warm)? == pixels(&mut cold)?,
                &format!("{mode:?} assembly {assembly}: kept titles draw like cold titles"),
            )?;
        }
    }
    Ok(())
}

/// T19: the palette is seeded from the constructor theme, so the first frame derives none; after
/// `set_theme(A)` a frame rendered with theme B draws B's tab bar, as a renderer that only ever
/// rendered B does. Both renderers settle their font fallback before the pixels are compared.
fn renderer_palette_follows_the_render_theme(active: &ActiveEventLoop) -> Result<(), String> {
    let theme_a = Theme::default();
    let mut theme_b = Theme::default();
    theme_b.colors.tab.active_fg = Hex("#ff00aa".to_string());
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut scene = single_scene(&candidate, &["shell", "logs"]);
    let candidate_settled = settle_fallback(&mut candidate, &mut scene)?;
    check(candidate.__palette_computes() == 0, "the constructor theme's palette is seeded")?;
    candidate.set_theme(&theme_a);
    scene.theme = theme_b.clone();
    forced(&mut candidate, &mut scene)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut oracle_scene = single_scene(&oracle, &["shell", "logs"]);
    oracle_scene.theme = theme_b;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;
    check_settled(&scene, candidate_settled, "theme B candidate")?;
    check_settled(&oracle_scene, oracle_settled, "theme B oracle")?;
    check(pixels(&mut candidate)? == pixels(&mut oracle)?, "the render theme's palette is drawn")?;
    check(candidate.__palette_computes() == 1, "the changed colors derive the palette once")
}

/// The chrome-run reuse and prepare counters and the shaping requests now.
fn run_counts(renderer: &GpuRenderer) -> (u64, u64, u64) {
    let stats = renderer.frame_stats();
    (stats.chrome_run_reuses, stats.chrome_run_prepares, stats.shape_requests)
}

/// T25: with search open on a fixed query and the tab bar hidden, a cold assembly prepares the
/// icon and label runs (2) and reuses them three times; a warm one prepares none, reuses five and
/// shapes nothing; and the overlay draws as a renderer that keeps no runs, on wgpu and on GDI.
/// Each count is read across a single render attempt, and both renderers settle their font
/// fallback before the pixels are compared.
fn search_overlay_reuses_its_runs(active: &ActiveEventLoop) -> Result<(), String> {
    for mode in MODES {
        let (_window, mut warm) = renderer(active, mode, &Theme::default(), (640, 240), false)?;
        let (_cold_window, mut cold) =
            renderer(active, mode, &Theme::default(), (640, 240), false)?;
        cold.__set_frame_reuse(false);
        let mut scenes = Vec::new();
        for target in [&warm, &cold] {
            let mut scene = single_scene(target, &["shell"]);
            let mut search = SearchState::new();
            search.bind_pane(1);
            search.set_query("abc", &scene.panes[0].grid);
            scene.search = Some(search);
            scenes.push(scene);
        }
        let (warm_scene, cold_scene) = scenes.split_at_mut(1);
        let (warm_scene, cold_scene) = (&mut warm_scene[0], &mut cold_scene[0]);
        // Settling fills the atlas; the cache is then emptied before each try at a cold assembly.
        let warm_settled = settle_fallback(&mut warm, warm_scene)?;
        let cold_settled = settle_fallback(&mut cold, cold_scene)?;
        let (before, cold_counts) =
            counted_forced(&mut warm, warm_scene, GpuRenderer::clear_shape_cache, run_counts)?;
        check(
            (cold_counts.1 - before.1, cold_counts.0 - before.0) == (2, 3),
            &format!(
                "{mode:?}: a cold overlay prepares 2 and reuses 3: {before:?} -> {cold_counts:?}"
            ),
        )?;
        let (cold_counts, warm_counts) = counted_forced(&mut warm, warm_scene, |_| {}, run_counts)?;
        check(
            (warm_counts.1 - cold_counts.1, warm_counts.0 - cold_counts.0) == (0, 5),
            &format!("{mode:?}: a warm overlay reuses 5: {cold_counts:?} -> {warm_counts:?}"),
        )?;
        check(warm_counts.2 == cold_counts.2, "a warm overlay shapes nothing")?;
        forced(&mut cold, cold_scene)?;
        forced(&mut cold, cold_scene)?;
        check_settled(warm_scene, warm_settled, &format!("{mode:?} warm overlay"))?;
        check_settled(cold_scene, cold_settled, &format!("{mode:?} cold overlay"))?;
        check(
            pixels(&mut warm)? == pixels(&mut cold)?,
            &format!("{mode:?}: kept runs draw the overlay as cold runs do"),
        )?;
    }
    Ok(())
}

/// `scene` split into two side-by-side panes, so a trim releases more than one pane's rows.
fn split_scene(renderer: &GpuRenderer, titles: &[&str]) -> Scene {
    let mut scene = single_scene(renderer, titles);
    let whole = scene.panes[0].rect;
    let left = PixelRect { w: whole.w / 2, ..whole };
    let right = PixelRect {
        x: whole.x + i32::try_from(left.w).unwrap_or(i32::MAX),
        w: whole.w - left.w,
        ..whole
    };
    scene.panes = vec![
        ScenePane { id: 1, rect: left, grid: text_grid(renderer, left) },
        ScenePane { id: 2, rect: right, grid: text_grid(renderer, right) },
    ];
    scene
}

/// The parts a covered-window trim releases, as a renderer reports them.
fn released_parts(renderer: &GpuRenderer) -> [sonicterm_types::ResourceAmount; 5] {
    let parts = renderer.retained_amounts();
    [
        parts.row_glyph_cache,
        parts.row_quad_cache,
        parts.row_ink,
        parts.frame_scratch,
        parts.chrome_cache,
    ]
}

/// A trim on the GPU presenter returns every released part to a new renderer's figure, keeps the
/// glyph atlas, lowers the vertex and upload storage, leaves a 1x1 frame texture marked trimmed, and
/// reports the frame texture and present buffers it gave back as request sizes.
fn trim_releases_and_accounts(active: &ActiveEventLoop) -> Result<(), String> {
    // The fresh renderer is built first: the case's own binding shadows the helper's name.
    let (_fresh_window, fresh) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    settle_fallback(&mut renderer, &mut scene)?;
    let surface = renderer.surface_size();
    let before = renderer.retained_amounts();
    let report = renderer.trim_for_occlusion();
    let after = renderer.retained_amounts();
    check(!report.refused, "a usable device admits the trim")?;
    check(released_parts(&renderer) == released_parts(&fresh), "released parts read as new")?;
    check(after.glyph_atlas == before.glyph_atlas, "the glyph atlas is kept")?;
    check(after.image_atlas == fresh.retained_amounts().image_atlas, "no media: the placeholder")?;
    check(
        after.vertex_scratch.bytes < before.vertex_scratch.bytes,
        "vertex and upload storage fall",
    )?;
    check(renderer.frame_texture_extent() == (1, 1), "the frame texture is 1x1")?;
    check(renderer.__frame_texture_trimmed(), "the frame texture is marked trimmed")?;
    check(report.frame_texture_before == surface, "the report names the released texture")?;
    let texture_bytes = u64::from(surface.0) * u64::from(surface.1) * 4 - 4;
    check(
        report.gpu_released_requested_bytes >= texture_bytes
            && report.present_buffer_bytes_before > 0,
        &format!("the report counts the texture and buffers: {report:?}"),
    )
}

/// After a trim, one frame of the same renderer presents at once: the texture is back at the surface
/// size and the pixels equal the settled reference with the same fallback generation. A resize after
/// a trim installs the surface-sized texture itself, so the next present rebuilds nothing.
fn trim_restores_without_a_resize(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let _ = renderer.trim_for_occlusion();
    check(
        matches!(frame(&mut renderer, &mut scene), PresentOutcome::Presented),
        "the first frame after the trim presents",
    )?;
    check(renderer.frame_texture_extent() == renderer.surface_size(), "the texture is restored")?;
    check(!renderer.__frame_texture_trimmed(), "the mark is cleared")?;
    check_settled(&scene, settled, "restored frame")?;
    check(pixels(&mut renderer)? == reference, "the restored frame equals the reference")?;

    let _ = renderer.trim_for_occlusion();
    check(renderer.try_resize(400, 200), "the resize is accepted")?;
    check(!renderer.__frame_texture_trimmed(), "the resize cleared the mark")?;
    let rebuilds = renderer.__frame_texture_rebuilds();
    let mut resized = split_scene(&renderer, &["shell", "logs"]);
    present(&mut renderer, &mut resized)?;
    check(renderer.__frame_texture_rebuilds() == rebuilds, "the next present rebuilds nothing")
}

/// Under the software presenter a trim leaves the 1x1 texture alone and never marks it, releases
/// the CPU parts and any upload storage, keeps the software frame, and the next GDI frame equals the
/// settled reference with the same fallback generation.
fn trim_on_the_software_presenter(active: &ActiveEventLoop) -> Result<(), String> {
    // The fresh renderer is built first: the case's own binding shadows the helper's name.
    let (_fresh_window, fresh) =
        renderer(active, SoftwareRenderMode::Force, &Theme::default(), (480, 240), true)?;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Force, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let rebuilds = renderer.__frame_texture_rebuilds();
    let before = renderer.retained_amounts();
    let _ = renderer.trim_for_occlusion();
    let after = renderer.retained_amounts();
    check(renderer.frame_texture_extent() == (1, 1), "the GDI texture stays 1x1")?;
    check(!renderer.__frame_texture_trimmed(), "the GDI texture is never marked")?;
    check(renderer.__frame_texture_rebuilds() == rebuilds, "no texture was built")?;
    check(released_parts(&renderer) == released_parts(&fresh), "released parts read as new")?;
    check(after.software_frame == before.software_frame, "the software frame is kept")?;
    check(after.vertex_scratch.bytes <= before.vertex_scratch.bytes, "upload storage is released")?;
    forced(&mut renderer, &mut scene)?;
    check_settled(&scene, settled, "GDI frame after the trim")?;
    check(pixels(&mut renderer)? == reference, "the next GDI frame equals the reference")
}

/// Counts WARN events on `sonic::glyph_atlas` carrying the retry-settlement message.
struct SettlementCounter(Arc<AtomicUsize>);

/// The settlement message the atlas retry logs once it presents.
const SETTLEMENT_MESSAGE: &str = "glyph atlas compaction retry presented with eviction disabled";

/// Reads one event's message field.
#[derive(Default)]
struct MessageOf(String);

impl tracing::field::Visit for MessageOf {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // When: the field is the event message, it is the text compared.
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SettlementCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target() != "sonic::glyph_atlas" || *metadata.level() != tracing::Level::WARN {
            // When: the target or level differs, the event is not the settlement line.
            return;
        }
        let mut message = MessageOf::default();
        event.record(&mut message);
        if message.0 == SETTLEMENT_MESSAGE {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A trim between an atlas retry and its present keeps the retry armed and eviction off and adds no
/// reset; the first recovered present settles it once, with the reference pixels and generation, and
/// the next frame settles nothing more.
fn trim_during_a_pending_atlas_retry(active: &ActiveEventLoop) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let settlements = Arc::new(AtomicUsize::new(0));
    let subscriber =
        tracing_subscriber::Registry::default().with(SettlementCounter(Arc::clone(&settlements)));
    sonicterm_logging::test_capture::with_default(subscriber, || -> Result<(), String> {
        renderer.__change_glyph_atlas_during_next_assembly();
        renderer.invalidate_retained_frame();
        check(
            matches!(frame(&mut renderer, &mut scene), PresentOutcome::AtlasRetry),
            "the changed atlas retries",
        )?;
        check(renderer.__test_glyph_atlas_retry_state() == (true, false), "the retry is armed")?;
        let resets = renderer.__test_glyph_atlas_resets();
        let _ = renderer.trim_for_occlusion();
        check(renderer.__test_glyph_atlas_retry_state() == (true, false), "the trim keeps it")?;
        check(renderer.__test_glyph_atlas_resets() == resets, "the trim adds no reset")?;
        check(
            matches!(frame(&mut renderer, &mut scene), PresentOutcome::Presented),
            "the first recovered frame presents",
        )?;
        check(renderer.__test_glyph_atlas_retry_state() == (false, true), "the retry settled")?;
        check(settlements.load(Ordering::SeqCst) == 1, "the settlement is logged once")?;
        check_settled(&scene, settled, "settled retry")?;
        check(pixels(&mut renderer)? == reference, "the settled frame equals the reference")?;
        forced(&mut renderer, &mut scene)?;
        check(renderer.__test_glyph_atlas_retry_state() == (false, true), "still settled")?;
        check(settlements.load(Ordering::SeqCst) == 1, "no second settlement")?;
        check(renderer.__test_glyph_atlas_resets() == resets, "no reset afterwards")?;
        check(pixels(&mut renderer)? == reference, "the next frame equals the reference")
    })
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let cases: [(&str, Case); 11] = [
        ("assembled frames reuse scratch", assembled_frames_reuse_scratch),
        ("scratch survives failed and retried frames", scratch_survives_failed_and_retried_frames),
        ("scratch caps hold", scratch_caps_hold_after_unbounded_frames),
        ("titles reuse and clear", renderer_reuses_titles_and_clears_on_face_replacement),
        ("cached titles draw like cold", cached_titles_draw_like_cold_titles),
        ("palette follows the render theme", renderer_palette_follows_the_render_theme),
        ("search overlay reuses its runs", search_overlay_reuses_its_runs),
        ("trim releases and accounts", trim_releases_and_accounts),
        ("trim restores without a resize", trim_restores_without_a_resize),
        ("trim on the software presenter", trim_on_the_software_presenter),
        ("trim during a pending atlas retry", trim_during_a_pending_atlas_retry),
    ];
    let mut failures = Vec::new();
    for (name, case) in cases {
        match case(active) {
            Ok(()) => {}
            Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
                // When: the host has no adapter, no renderer case can run.
                println!("capability={reason}");
                return Ok(());
            }
            Err(reason) => failures.push(format!("{name}: {reason}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Frame-scratch, title, palette and search-run reuse on a real renderer, on WARP and GDI.
#[cfg(target_os = "windows")]
#[test]
fn windows_frame_reuse_on_a_real_renderer() {
    use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("frame reuse event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
