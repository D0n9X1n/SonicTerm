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
    let font_dirs =
        [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono")];
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

/// One forced frame's allocations and what it changed: whether it drew in a single attempt with
/// no row-cache miss and no atlas growth (a steady frame), and the scratch bytes it left.
struct ForcedFrame {
    allocations: usize,
    steady: bool,
    scratch_bytes: usize,
    figures: String,
}

/// Frames a recovery may take to become steady again. After an atlas reset the first presented
/// frame refills the row cache and keeps it when the retry settles, so the next frame is steady;
/// the bound leaves room for a frame that still grows the atlas.
const RECOVERY_FRAMES: usize = 4;

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

/// What one forced `frame()` call did: its outcome, the counters it moved and its allocations.
struct SingleFrame {
    outcome: PresentOutcome,
    attempts: u64,
    presented: u64,
    hits: u64,
    misses: u64,
    resets: u64,
    allocations: usize,
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let cases: [(&str, Case); 14] = [
        ("assembled frames reuse scratch", assembled_frames_reuse_scratch),
        ("scratch survives failed and retried frames", scratch_survives_failed_and_retried_frames),
        ("scratch caps hold", scratch_caps_hold_after_unbounded_frames),
        ("titles reuse and clear", renderer_reuses_titles_and_clears_on_face_replacement),
        ("cached titles draw like cold", cached_titles_draw_like_cold_titles),
        ("palette follows the render theme", renderer_palette_follows_the_render_theme),
        ("search overlay reuses its runs", search_overlay_reuses_its_runs),
        ("trim releases and accounts", trim_releases_and_accounts),
        (
            "graphics ledger follows the renderer",
            graphics_ledger_follows_create_resize_trim_and_drop,
        ),
        ("trim restores without a resize", trim_restores_without_a_resize),
        ("trim on the software presenter", trim_on_the_software_presenter),
        ("trim during a pending atlas retry", trim_during_a_pending_atlas_retry),
        ("atlas retry keeps rebuilt rows", atlas_retry_keeps_rebuilt_rows),
        (
            "atlas retry keeps the complete row",
            atlas_retry_keeps_the_complete_row_of_a_refused_frame,
        ),
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

#[path = "support/windows_frame_reuse_scratch.rs"]
mod scratch;
use scratch::assembled_frames_reuse_scratch;
use scratch::scratch_caps_hold_after_unbounded_frames;
use scratch::scratch_survives_failed_and_retried_frames;
#[path = "support/windows_frame_reuse_chrome.rs"]
mod chrome;
use chrome::cached_titles_draw_like_cold_titles;
use chrome::renderer_palette_follows_the_render_theme;
use chrome::renderer_reuses_titles_and_clears_on_face_replacement;
use chrome::search_overlay_reuses_its_runs;
#[path = "support/windows_frame_reuse_retention.rs"]
mod retention;
use retention::graphics_ledger_follows_create_resize_trim_and_drop;
use retention::split_scene;
use retention::trim_on_the_software_presenter;
use retention::trim_releases_and_accounts;
use retention::trim_restores_without_a_resize;
#[path = "support/windows_frame_reuse_retry.rs"]
mod retry;
use retry::atlas_retry_keeps_rebuilt_rows;
use retry::atlas_retry_keeps_the_complete_row_of_a_refused_frame;
use retry::trim_during_a_pending_atlas_retry;
