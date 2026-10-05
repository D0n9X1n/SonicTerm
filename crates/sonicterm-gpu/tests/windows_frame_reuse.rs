//! Frame-scratch, tab-title, palette and search-run reuse on a real renderer.
//!
//! Forced assemblies of an unchanged or slightly edited scene reuse the renderer's frame scratch
//! (fewer allocations than with reuse off, same pixels), keep it across failed and retried
//! frames, and hold it within its cap. Warm tab titles and search-overlay runs draw without
//! shaping, every face replacement makes them prepare again, and their pixels equal a renderer
//! with reuse off, on wgpu and on GDI. The UI palette follows the theme passed to `render`.
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
    }
}

/// Draw `scene` once through the releasing call, measuring tab widths first as the App does.
fn frame(renderer: &mut GpuRenderer, scene: &mut Scene) -> PresentOutcome {
    let fonts = renderer.begin_frame_fonts();
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

/// Present `scene` with the retained frame forgotten, so the frame assembles even unchanged.
fn forced(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<(), String> {
    renderer.invalidate_retained_frame();
    present(renderer, scene)
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
                    .ok_or(format!("GDI pixel ({pixel_x}, {pixel_y}) is readable"))?,
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
/// frame, and the last frames' pixels are equal.
fn assembled_frames_reuse_scratch(active: &ActiveEventLoop) -> Result<(), String> {
    let mut runs = Vec::new();
    for reuse in [true, false] {
        let (_window, mut renderer) =
            renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
        renderer.__set_frame_reuse(reuse);
        let mut scene = single_scene(&renderer, &["shell"]);
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

/// T8: after a failed assembly, a failed presentation and an atlas retry, the scratch keeps its
/// capacity and the next forced frame draws what a renderer with no failures draws. The partial
/// fallback exit has no renderer seam; the device-free driver covers it.
fn scratch_survives_failed_and_retried_frames(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut scene = single_scene(&candidate, &["shell"]);
    let mut oracle_scene = single_scene(&oracle, &["shell"]);
    for _ in 0..3 {
        forced(&mut candidate, &mut scene)?;
        forced(&mut oracle, &mut oracle_scene)?;
    }
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
        forced(&mut candidate, &mut scene)?;
        forced(&mut oracle, &mut oracle_scene)?;
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
/// assembly prepare every title again.
fn renderer_reuses_titles_and_clears_on_face_replacement(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 240), true)?;
    let long = "a very long tab title that cannot fit its tab and must be cut short";
    let mut scene = single_scene(&renderer, &["shell", "logs", long]);
    present(&mut renderer, &mut scene)?;
    forced(&mut renderer, &mut scene)?;
    let warm_pixels = pixels(&mut renderer)?;
    let before = title_counts(&renderer);
    forced(&mut renderer, &mut scene)?;
    let after = title_counts(&renderer);
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
/// pixels of one that keeps none, on wgpu and on GDI.
fn cached_titles_draw_like_cold_titles(active: &ActiveEventLoop) -> Result<(), String> {
    let titles = ["shell", "logs", "build ~/src/sonicterm", "é ñ ü", "中文标题", "vim main.rs"];
    for mode in MODES {
        let (_window, mut warm) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        let (_cold_window, mut cold) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        cold.__set_frame_reuse(false);
        for assembly in 0..200usize {
            let chosen: Vec<&str> =
                (0..3).map(|offset| titles[(assembly / 3 + offset) % titles.len()]).collect();
            let mut warm_scene = single_scene(&warm, &chosen);
            let mut cold_scene = single_scene(&cold, &chosen);
            forced(&mut warm, &mut warm_scene)?;
            forced(&mut cold, &mut cold_scene)?;
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
/// rendered B does.
fn renderer_palette_follows_the_render_theme(active: &ActiveEventLoop) -> Result<(), String> {
    let theme_a = Theme::default();
    let mut theme_b = Theme::default();
    theme_b.colors.tab.active_fg = Hex("#ff00aa".to_string());
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut scene = single_scene(&candidate, &["shell", "logs"]);
    present(&mut candidate, &mut scene)?;
    check(candidate.__palette_computes() == 0, "the constructor theme's palette is seeded")?;
    candidate.set_theme(&theme_a);
    scene.theme = theme_b.clone();
    forced(&mut candidate, &mut scene)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut oracle_scene = single_scene(&oracle, &["shell", "logs"]);
    oracle_scene.theme = theme_b;
    forced(&mut oracle, &mut oracle_scene)?;
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
        // The first frame fills the atlas; the cache is then emptied for a cold assembly.
        present(&mut warm, warm_scene)?;
        warm.clear_shape_cache();
        let before = run_counts(&warm);
        forced(&mut warm, warm_scene)?;
        let cold_counts = run_counts(&warm);
        check(
            (cold_counts.1 - before.1, cold_counts.0 - before.0) == (2, 3),
            &format!(
                "{mode:?}: a cold overlay prepares 2 and reuses 3: {before:?} -> {cold_counts:?}"
            ),
        )?;
        forced(&mut warm, warm_scene)?;
        let warm_counts = run_counts(&warm);
        check(
            (warm_counts.1 - cold_counts.1, warm_counts.0 - cold_counts.0) == (0, 5),
            &format!("{mode:?}: a warm overlay reuses 5: {cold_counts:?} -> {warm_counts:?}"),
        )?;
        check(warm_counts.2 == cold_counts.2, "a warm overlay shapes nothing")?;
        forced(&mut cold, cold_scene)?;
        forced(&mut cold, cold_scene)?;
        check(
            pixels(&mut warm)? == pixels(&mut cold)?,
            &format!("{mode:?}: kept runs draw the overlay as cold runs do"),
        )?;
    }
    Ok(())
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let cases: [(&str, Case); 7] = [
        ("assembled frames reuse scratch", assembled_frames_reuse_scratch),
        ("scratch survives failed and retried frames", scratch_survives_failed_and_retried_frames),
        ("scratch caps hold", scratch_caps_hold_after_unbounded_frames),
        ("titles reuse and clear", renderer_reuses_titles_and_clears_on_face_replacement),
        ("cached titles draw like cold", cached_titles_draw_like_cold_titles),
        ("palette follows the render theme", renderer_palette_follows_the_render_theme),
        ("search overlay reuses its runs", search_overlay_reuses_its_runs),
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
