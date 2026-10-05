//! Partial assembly on a real wgpu renderer, read back from its retained frame.
//!
//! Pixel parity: each narrowed case presents a `Partial` frame that emits fewer rows than the pane
//! has, and its retained pixels equal a full repaint of the same state, over a translucent
//! background. Failures after a partial plan (an acquisition retry, an atlas change during
//! assembly, a submission that fails after the retained draw) commit nothing, keep the dirt, clear
//! the key and retry as a `Full` frame. The row-ink table grows with a 2,000-row pane and releases
//! its records when the pane shrinks or closes, and a degraded frame is never partial.
//!
//! Content-keyed glyph rows: a warm renderer whose scrolled rows replay from the row glyph cache
//! draws what a cold oracle draws, through wgpu (scrolled Full and edited Partial frames) and
//! through GDI, at four scales; the oracle is built alike, presents the target frame, then drops
//! its pane caches and retained frame, which leave the atlas warm. An `Err` from assembly or from
//! the presenter call discards the staged glyph-row keys and keeps the committed ones.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use sonicterm_gpu::{
    core::{
        settle_borrowed_frame, unpad_readback_rows, GlyphAtlasStart, GpuRenderer, InjectedRowGlyph,
        PresentOutcome, PresentedDamage, RendererSettings, SurfaceAppearance, SurfaceRetryReason,
    },
    device_errors::DeviceStateWaker,
};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{CursorShape, ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::{CellFlags, Color, Grid, UnderlineStyle},
        ui::{
            selection::Selection,
            tabs::{Tab, TabBar},
        },
    },
    AckReceipt, AckRows, BorrowedSource, CursorStyle, InlineImage, PaneRender, PixelRect,
};
use sonicterm_types::{ClassCoverage, ResourceClass};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

/// The pane every single-pane case draws.
const PANE_ID: u64 = 1;
/// The second pane of the neighbouring-pane case.
const NEIGHBOUR_ID: u64 = 2;
/// The terminal background opacity: below 1, so every case composes over a translucent ground.
const OPACITY: f32 = 0.6;
/// The row each one-row edit rewrites; blank until the edit.
const EDIT_ROW: u16 = 10;
/// The cursor's row in the cursor cases, two rows above the edit row.
const CURSOR_ROW: u16 = 8;
/// The cursor's column: past the two glyphs of its row, so it rests on a blank cell.
const CURSOR_COL: u16 = 4;
/// Every text an edit writes, drawn once in the baseline so no edit grows the glyph atlas.
const WARM_TEXT: &str = "editfailxyz中😀e\u{301}a\u{308}o\u{302}uline";
/// The injected glyphs' color: opaque, and unlike any theme color, so stale ink is visible.
const INJECTED_COLOR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];

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

/// Pixel geometry the cases share: the whole surface as the pane, its text grid's origin (the
/// grid is bottom-aligned, so it starts below the pane's top), the cell size, the planner's
/// vertical ink pad and the grid size.
#[derive(Clone, Copy)]
struct Layout {
    pane: PixelRect,
    grid_x: f32,
    grid_y: f32,
    cell_w: f32,
    cell_h: f32,
    ink_pad: f32,
    cols: u16,
    rows: u16,
}

impl Layout {
    /// The top of viewport row `row` in surface pixels.
    fn row_top(&self, row: u16) -> f32 {
        self.grid_y + f32::from(row) * self.cell_h
    }

    /// The left edge of column `col` in surface pixels.
    fn col_left(&self, col: u16) -> f32 {
        self.grid_x + f32::from(col) * self.cell_w
    }

    /// Every cell of one pane of this layout.
    fn cells(&self) -> u64 {
        u64::from(self.cols) * u64::from(self.rows)
    }

    /// Row `row`'s ink-padded damage strip as the planner rounds it: its top floored from the
    /// row's top less the pad, its bottom ceiled from the next row's top plus the pad, spanning the
    /// grid and the pane, clipped to the pane. Neither the pitch nor the origin need be integral.
    fn strip_rect(&self, row: u16) -> Option<PixelRect> {
        let left = (self.grid_x.floor() as i32).min(self.pane.x);
        let right = ((self.grid_x + f32::from(self.cols) * self.cell_w).ceil() as i32)
            .max(self.pane.right());
        let top = (self.grid_y + f32::from(row) * self.cell_h - self.ink_pad).floor() as i32;
        let bottom =
            (self.grid_y + (f32::from(row) + 1.0) * self.cell_h + self.ink_pad).ceil() as i32;
        PixelRect {
            x: left,
            y: top,
            w: (right - left).max(1) as u32,
            h: (bottom - top).max(1) as u32,
        }
        .intersect(self.pane)
    }

    /// A glyph `rows_tall` rows tall under the cursor cell at (`row`, `col`), bottom-aligned with
    /// it, as `(x, y, w, h)` in surface pixels.
    fn cursor_glyph(&self, row: u16, col: u16, rows_tall: f32) -> (f32, f32, f32, f32) {
        let height = rows_tall * self.cell_h;
        (self.col_left(col), self.row_top(row) + self.cell_h - height, self.cell_w, height)
    }

    /// The share of `glyph`'s area the cursor cell at (`row`, `col`) covers; the block cursor
    /// recolors a glyph it covers by at least 20%.
    fn cursor_coverage(&self, row: u16, col: u16, glyph: (f32, f32, f32, f32)) -> f32 {
        let (left, top, width, height) = glyph;
        let (cell_left, cell_top) = (self.col_left(col), self.row_top(row));
        let overlap_w =
            ((left + width).min(cell_left + self.cell_w) - left.max(cell_left)).max(0.0);
        let overlap_h = ((top + height).min(cell_top + self.cell_h) - top.max(cell_top)).max(0.0);
        overlap_w * overlap_h / (width * height)
    }
}

/// `(x, y, w, h)` in surface pixels rounded outward to whole pixels, as the renderer bounds ink.
fn outward((left, top, width, height): (f32, f32, f32, f32)) -> PixelRect {
    let (x, y) = (left.floor() as i32, top.floor() as i32);
    let (right, bottom) = ((left + width).ceil() as i32, (top + height).ceil() as i32);
    PixelRect { x, y, w: (right - x).max(0) as u32, h: (bottom - y).max(0) as u32 }
}

/// The rows a one-row edit at `edit_row` must emit, from the geometry alone: the edited row, every
/// row whose strip meets the edited row's strip with positive area, and every row whose `record`
/// meets that strip.
fn edit_rows(
    layout: &Layout,
    edit_row: u16,
    record: impl Fn(u16) -> Option<PixelRect>,
) -> Vec<u16> {
    let Some(damage) = layout.strip_rect(edit_row) else {
        // When: the edited row's strip is off the pane, it damages nothing but is still emitted.
        return vec![edit_row];
    };
    let meets = |rect: Option<PixelRect>| rect.is_some_and(|rect| rect.intersect(damage).is_some());
    (0..layout.rows)
        .filter(|row| *row == edit_row || meets(layout.strip_rect(*row)) || meets(record(*row)))
        .collect()
}

/// The nearest row above `cursor_row` that showing the cursor skips while the recolored `glyph`
/// reaches it: its strip and its `record` both miss the cursor row's strip, and its record meets
/// `glyph`. `None` when no row qualifies.
fn skipped_reached_row(
    layout: &Layout,
    cursor_row: u16,
    glyph: PixelRect,
    record: impl Fn(u16) -> Option<PixelRect>,
) -> Option<u16> {
    let damage = layout.strip_rect(cursor_row)?;
    let misses = |rect: PixelRect| rect.intersect(damage).is_none();
    (0..cursor_row).rev().find(|row| {
        layout.strip_rect(*row).is_none_or(misses)
            && record(*row).is_some_and(|ink| misses(ink) && ink.intersect(glyph).is_some())
    })
}

/// One pane a case draws: its id, rectangle, grid and inline images.
struct ScenePane {
    id: u64,
    rect: PixelRect,
    grid: Grid,
    images: Vec<InlineImage>,
    active: bool,
}

/// What a case draws, besides the renderer's own state.
struct Scene {
    /// The terminal background opacity the renderer draws this scene at.
    opacity: f32,
    /// The receipts of the last frame of this scene that presented.
    receipts: Vec<AckReceipt>,
    /// The font-fallback generation the last presented frame of this scene applied.
    font_generation: Option<u64>,
    panes: Vec<ScenePane>,
    cursor_visible: bool,
    selection: Option<Selection>,
    tabs: TabBar,
}

impl Scene {
    /// Every cell of every pane, which a `Full` frame hashes once each.
    fn cells(&self) -> u64 {
        self.panes.iter().map(|pane| u64::from(pane.grid.cols) * u64::from(pane.grid.rows)).sum()
    }

    /// The first pane's grid.
    fn grid(&mut self) -> &mut Grid {
        &mut self.panes[0].grid
    }
}

/// The renderer counters a case compares before and after a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    partial_frames: u64,
    partial_fallbacks: u64,
    full_frames: u64,
    row_cells_hashed: u64,
    uploaded_bytes: u64,
    software_frames: u64,
    native_request_redraw: u64,
}

/// The renderer's counters now.
fn counts(renderer: &GpuRenderer) -> Counts {
    let stats = renderer.frame_stats();
    Counts {
        partial_frames: stats.partial_frames,
        partial_fallbacks: stats.partial_fallbacks,
        full_frames: stats.full_frames,
        row_cells_hashed: stats.row_cells_hashed,
        uploaded_bytes: stats.vertex_bytes + stats.index_bytes,
        software_frames: stats.software_frames,
        native_request_redraw: stats.native_request_redraw,
    }
}

/// What moved between `before` and `after`.
fn delta(before: Counts, after: Counts) -> Counts {
    Counts {
        partial_frames: after.partial_frames - before.partial_frames,
        partial_fallbacks: after.partial_fallbacks - before.partial_fallbacks,
        full_frames: after.full_frames - before.full_frames,
        row_cells_hashed: after.row_cells_hashed - before.row_cells_hashed,
        uploaded_bytes: after.uploaded_bytes - before.uploaded_bytes,
        software_frames: after.software_frames - before.software_frames,
        native_request_redraw: after.native_request_redraw - before.native_request_redraw,
    }
}

/// A visible window and a counting wgpu renderer on it with a translucent background, line
/// height 1 (so the vertical ink pad is about one row), no scrollbar and no tab bar; `Err` names a
/// host with no adapter as `HOST_INCAPABLE`.
fn wgpu_renderer(active: &ActiveEventLoop) -> Result<(Arc<Window>, GpuRenderer), String> {
    let (window, renderer) = renderer_in_mode(active, SoftwareRenderMode::Off)?;
    check(!renderer.is_software_render_degraded(), "the renderer presents through wgpu")?;
    Ok((window, renderer))
}

/// A visible window and a counting renderer on it in software render `mode`, configured as
/// [`wgpu_renderer`] configures one; `Err` names a host with no adapter as `HOST_INCAPABLE`.
fn renderer_in_mode(
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 480))
                    .with_visible(true)
                    .with_title("partial-assembly"),
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
            opacity: OPACITY,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: mode,
        },
        role: "partial-assembly",
        glyph_atlas_start: GlyphAtlasStart::Normal,
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &Theme::default(), settings) {
        Ok(renderer) => renderer,
        Err(error) if host_has_no_adapter() => {
            // When: host_has_no_adapter holds, no wgpu renderer can exist on this host.
            return Err(format!("HOST_INCAPABLE: no wgpu adapter: {error}"));
        }
        Err(error) => return Err(format!("wgpu renderer construction failed: {error}")),
    };
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    renderer.set_cursor_shape(CursorShape::Block);
    renderer.set_window_focused(true);
    renderer.set_frame_counting(true);
    renderer.__enable_retained_frame_readback();
    renderer.__enable_presented_damage();
    Ok((window, renderer))
}

/// The whole surface as one pane, the grid it holds and where that grid draws. With line height 1
/// the ink pad is the font's cell height rounded up; neither the cell height nor the grid origin
/// need be whole pixels, so every row set the cases expect is computed from this geometry.
fn layout(renderer: &GpuRenderer, window: &Window) -> Layout {
    let size = window.inner_size();
    let (cell_w, cell_h) = renderer.cell_size();
    let pane = PixelRect { x: 0, y: 0, w: size.width, h: size.height };
    let padding = [
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.padding_top_px(),
        renderer.padding_bottom_px(),
    ];
    let cols = (((pane.w as f32 - padding[0] - padding[1]) / cell_w).floor() as u16).max(1);
    let rows = (((pane.h as f32 - padding[2] - padding[3]) / cell_h).floor() as u16).max(1);
    let geometry = sonicterm_render_model::pane_content_geometry(pane, padding, cell_h, rows);
    Layout {
        pane,
        grid_x: geometry.grid.x,
        grid_y: geometry.grid.y,
        cell_w,
        cell_h,
        ink_pad: cell_h.ceil(),
        cols,
        rows,
    }
}

/// Write `text` from `(row, col)`.
fn write(grid: &mut Grid, row: u16, col: u16, text: &str) {
    grid.goto(row, col);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A grid of dense glyphs: every row full except the blank edit row and the cursor row, which
/// holds two glyphs; the last row holds every text an edit writes. The cursor is parked on the
/// edit row, so an edit dirties that row only.
fn dense_grid(cols: u16, rows: u16) -> Grid {
    let mut grid = Grid::new(cols, rows);
    for row in 0..rows {
        let text = if row == EDIT_ROW {
            continue;
        } else if row == CURSOR_ROW {
            "MW".to_owned()
        } else if row == rows - 1 {
            WARM_TEXT.to_owned()
        } else {
            // When: the row is neither the edit, cursor nor warm row, it is dense text.
            "MW".repeat(usize::from(cols) / 2)
        };
        write(&mut grid, row, 0, &text);
    }
    grid.goto(EDIT_ROW, 0);
    grid
}

/// One active pane over the whole surface with a dense grid and a hidden cursor.
fn single(layout: &Layout) -> Scene {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    Scene {
        opacity: OPACITY,
        receipts: Vec::new(),
        font_generation: None,
        panes: vec![ScenePane {
            id: PANE_ID,
            rect: layout.pane,
            grid: dense_grid(layout.cols, layout.rows),
            images: Vec::new(),
            active: true,
        }],
        cursor_visible: false,
        selection: None,
        tabs,
    }
}

/// Draw `scene` once through the releasing call over borrowed grids, as the App's adapters do.
/// A presented frame's receipts are applied to the grids and kept on the scene.
fn frame(renderer: &mut GpuRenderer, scene: &mut Scene) -> PresentOutcome {
    frame_with_receipts(renderer, scene).0
}

/// [`frame`], also returning the receipts the releasing call returned, read before settlement
/// discards those of any outcome but `Presented`, so a test can require a failure issues none.
fn frame_with_receipts(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
) -> (PresentOutcome, Vec<AckReceipt>) {
    let theme = Theme::default();
    let fonts = renderer.begin_frame_fonts();
    let generation = fonts.generation();
    let mut panes: Vec<PaneRender<'_>> = scene
        .panes
        .iter_mut()
        .map(|pane| PaneRender {
            id: pane.id,
            rect_px: pane.rect,
            grid: &mut pane.grid,
            viewport_top_abs: None,
            is_active: pane.active,
            cursor_style: CursorStyle::BlockSteady,
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: pane.images.clone(),
        })
        .collect();
    let released = renderer.render_releasing(
        &fonts,
        BorrowedSource(&mut panes[..]),
        &theme,
        scene.cursor_visible,
        scene.selection.as_ref(),
        None,
        &scene.tabs,
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let receipts = released.receipts.clone();
    let outcome = settle_borrowed_frame(released, &mut panes);
    drop(panes);
    if matches!(outcome, PresentOutcome::Presented) {
        // Only a presented frame issues receipts; a retry keeps the last presented frame's.
        scene.receipts = receipts.clone();
        scene.font_generation = Some(generation);
    }
    (outcome, receipts)
}

/// Draw `scene` until a frame presents, at most four tries, and return that frame's damage.
fn present(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<PresentedDamage, String> {
    for _ in 0..4 {
        if matches!(frame(renderer, scene), PresentOutcome::Presented) {
            return renderer.__take_presented_damage().ok_or_else(|| {
                String::from("a presented frame records its damage beside the frame key")
            });
        }
    }
    Err(String::from("the frame never presented"))
}

/// Start a case from a full repaint of `scene`, so the next frame is planned against it.
fn baseline(renderer: &mut GpuRenderer, scene: &mut Scene) -> Result<(), String> {
    renderer.invalidate_retained_frame();
    let damage = present(renderer, scene)?;
    check(damage.first_frame, "the baseline is a whole-surface first frame")
}

/// The retained GPU frame's tightly packed BGRA pixels and its width. This test maps and polls;
/// the renderer never does.
fn retained_pixels(renderer: &mut GpuRenderer) -> Result<(Vec<u8>, u32), String> {
    let readback =
        renderer.__copy_retained_frame().ok_or("readback is enabled and the device works")?;
    let slice = readback.buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    readback
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll the retained-frame readback: {error}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|error| format!("map the retained-frame readback: {error}"))?;
    let pixels = unpad_readback_rows(
        &mapped,
        readback.width_px,
        readback.height_px,
        readback.padded_row_bytes,
    );
    drop(mapped);
    readback.buffer.unmap();
    Ok((pixels, readback.width_px))
}

/// Watches the font-fallback generation the renderer applies until it has held still: for
/// `quiet` when the last frame drew no tofu, or for five times as long while tofu remains (a host
/// may lack a face, or its worker may still be searching).
struct FallbackQuiet {
    quiet: Duration,
    since: Option<(u64, Instant)>,
}

impl FallbackQuiet {
    fn new(quiet: Duration) -> Self {
        Self { quiet, since: None }
    }

    /// Record that a frame at `now` applied `generation`, drawing tofu when `tofu`; true once the
    /// generation has not changed for the quiet period.
    fn observe(&mut self, generation: u64, tofu: bool, now: Instant) -> bool {
        let since = match self.since {
            Some((held, since)) if held == generation => since,
            _ => {
                // A first observation or a publication restarts the wait.
                self.since = Some((generation, now));
                now
            }
        };
        let needed = if tofu { self.quiet * 5 } else { self.quiet };
        now.duration_since(since) >= needed
    }
}

/// How many pixels differ between two equally sized BGRA frames outside `excluded` (everywhere
/// when `None`), and the rectangle bounding them.
fn differing_pixels(
    first: &[u8],
    second: &[u8],
    width_px: u32,
    excluded: Option<PixelRect>,
) -> (usize, Option<PixelRect>) {
    let width = width_px as usize;
    let mut count = 0;
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for (index, (before, after)) in first.chunks(4).zip(second.chunks(4)).enumerate() {
        let (column_px, row_px) = ((index % width) as i32, (index / width) as i32);
        let inside = excluded.is_some_and(|rect| {
            (rect.x..rect.right()).contains(&column_px) && (rect.y..rect.bottom()).contains(&row_px)
        });
        if before == after || inside {
            continue;
        }
        count += 1;
        bounds = Some(match bounds {
            None => (column_px, row_px, column_px, row_px),
            Some((left_px, top_px, right_px, bottom_px)) => (
                left_px.min(column_px),
                top_px.min(row_px),
                right_px.max(column_px),
                bottom_px.max(row_px),
            ),
        });
    }
    let rect = bounds.map(|(left_px, top_px, right_px, bottom_px)| PixelRect {
        x: left_px,
        y: top_px,
        w: (right_px - left_px + 1) as u32,
        h: (bottom_px - top_px + 1) as u32,
    });
    (count, rect)
}

/// The first pixel `(x, y)` where two equally sized frames differ, if any.
fn first_difference(first: &[u8], second: &[u8], width_px: u32) -> Option<(usize, usize)> {
    let width = width_px as usize;
    first
        .chunks(4)
        .zip(second.chunks(4))
        .position(|(left, right)| left != right)
        .map(|index| (index % width, index / width))
}

/// The narrow frame's own checks: it applied the baseline's font-fallback generation, and it damaged
/// less than the surface. A generation applied before the narrow frame clears the frame key, so
/// production repaints that frame in full; the generation is checked first so that case fails by
/// name rather than as a whole-surface narrow frame.
fn check_narrow_frame(
    case: &str,
    baseline_generation: Option<u64>,
    narrow_generation: Option<u64>,
    narrow: &PresentedDamage,
) -> Result<(), String> {
    check(
        baseline_generation == narrow_generation,
        &format!(
            "{case}: font fallback changed between baseline {baseline_generation:?} and narrow \
             {narrow_generation:?}; rerun after warm-up"
        ),
    )?;
    check(
        !narrow.first_frame && narrow.is_narrow(),
        &format!("{case}: the frame damages less than the surface: {narrow:?}"),
    )
}

/// How a narrowed frame is expected to be assembled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// A presented `Partial` frame that hashes fewer cells than the scene has.
    Partial,
    /// A `Partial` plan reassembled `Full` in the same frame by the post-assembly check.
    Fallback,
}

/// Present `scene` with narrow damage, check how it was assembled and that it changed no retained
/// pixel outside its damage, then repaint the same state in full and require the two retained
/// frames equal byte for byte, with the ground's alpha following the scene's background opacity.
///
/// The comparison is valid only within one font state: a fallback publication applied between the
/// frames clears the frame key and changes how a glyph outside the damage draws, which production
/// repaints in full on the next frame. All three frames must apply one fallback generation, and a
/// change fails by name, not as a pixel difference.
/// Returns the narrow frame's damage, its counter deltas and its receipts.
fn narrow_matches_full(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    case: &str,
    expect: Expect,
) -> Result<(PresentedDamage, Counts, Vec<AckReceipt>), String> {
    let baseline_generation = scene.font_generation;
    let (previous_pixels, _) = retained_pixels(renderer)?;
    let before = counts(renderer);
    let narrow = present(renderer, scene)?;
    let moved = delta(before, counts(renderer));
    let narrow_generation = scene.font_generation;
    // The full comparison frame below replaces the scene's receipts, so keep the narrow frame's.
    let narrow_receipts = std::mem::take(&mut scene.receipts);
    check_narrow_frame(case, baseline_generation, narrow_generation, &narrow)?;
    let assembled = match expect {
        Expect::Partial => {
            moved.partial_frames == 1
                && moved.full_frames == 0
                && moved.partial_fallbacks == 0
                && moved.row_cells_hashed < scene.cells()
        }
        Expect::Fallback => {
            moved.partial_fallbacks == 1 && moved.full_frames == 1 && moved.partial_frames == 0
        }
    };
    check(
        assembled,
        &format!("{case}: expected {expect:?}, counted {moved:?} over {} cells", scene.cells()),
    )?;
    let (narrow_pixels, width_px) = retained_pixels(renderer)?;
    // The narrow frame draws under its damage scissor, so every pixel outside it is retained.
    let outside = differing_pixels(&previous_pixels, &narrow_pixels, width_px, Some(narrow.damage));
    check(
        outside.0 == 0,
        &format!(
            "{case}: the narrow frame changed no pixel outside its damage {:?}: {outside:?}",
            narrow.damage
        ),
    )?;
    renderer.invalidate_retained_frame();
    let full = present(renderer, scene)?;
    check(full.first_frame, &format!("{case}: the comparison frame repaints in full"))?;
    let (full_pixels, _) = retained_pixels(renderer)?;
    let generations = [baseline_generation, narrow_generation, scene.font_generation];
    check(
        generations.iter().all(|generation| *generation == generations[0]),
        &format!(
            "{case}: a font fallback was published during the case (baseline, narrow, full \
             generations {generations:?}); settle_fallback must run first"
        ),
    )?;
    check(
        narrow_pixels.len() == full_pixels.len(),
        &format!("{case}: both frames read back at one size"),
    )?;
    let difference = first_difference(&narrow_pixels, &full_pixels, width_px);
    check(
        difference.is_none(),
        &format!(
            "{case}: narrow damage {:?} equals a full repaint; first differing pixel \
             {difference:?}, differing {:?}, generations {generations:?}",
            narrow.damage,
            differing_pixels(&narrow_pixels, &full_pixels, width_px, None)
        ),
    )?;
    // The cleared ground is opaque at opacity 1 and keeps an alpha below opaque under it.
    let corner = &full_pixels[..4];
    check(
        (corner[3] == u8::MAX) == (scene.opacity >= 1.0),
        &format!("{case}: the ground's alpha {corner:?} follows opacity {}", scene.opacity),
    )?;
    Ok((narrow, moved, narrow_receipts))
}

/// Test 6: every acceptance case presents `Partial`, emitting fewer rows than the pane has, and
/// equals a full repaint: a one-row edit, a tall glyph overhanging from the row above and from the
/// row below, combining marks, wide CJK and emoji at the pane's right edge, every underline style,
/// an inline image crossing the damage edge, a selection change and a cursor toggle. The matrix
/// runs at opacity 1 and 0.6, each over default row backgrounds and over ANSI row backgrounds.
fn pixel_parity(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let theme = Theme::default();
    let mut cases = Ok(());
    for opacity in [1.0, OPACITY] {
        renderer.set_theme_with_opacity(&theme, opacity);
        for ansi_rows in [false, true] {
            cases = cases.and_then(|()| parity_cases(renderer, layout, opacity, ansi_rows));
        }
    }
    // Later cases draw at the shared translucent opacity, whatever this matrix ended on.
    renderer.set_theme_with_opacity(&theme, OPACITY);
    cases
}

/// Give the rows around the edit row non-default ANSI backgrounds: the rows above and below keep
/// dense text over palette backgrounds 4 and 2, and the edit row holds blank cells over palette
/// background 1, so an edit redraws text over a painted row between painted neighbours.
fn paint_ansi_rows(grid: &mut Grid, cols: u16) {
    for (row, background) in [(EDIT_ROW - 1, 4), (EDIT_ROW, 1), (EDIT_ROW + 1, 2)] {
        grid.goto(row, 0);
        for col in 0..cols {
            let character = if row == EDIT_ROW {
                ' '
            } else if col % 2 == 0 {
                'M'
            } else {
                'W'
            };
            grid.put_char(
                character,
                Color::Default,
                Color::Indexed(background),
                CellFlags::empty(),
            );
        }
    }
    grid.goto(EDIT_ROW, 0);
}

/// Test 6's cases at one background `opacity`, over ANSI row backgrounds when `ansi_rows`.
fn parity_cases(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    opacity: f32,
    ansi_rows: bool,
) -> Result<(), String> {
    let scene = || {
        let mut scene = single(layout);
        scene.opacity = opacity;
        if ansi_rows {
            paint_ansi_rows(scene.grid(), layout.cols);
        }
        scene
    };
    let label = |case: &str| {
        format!("{case} at opacity {opacity}{}", if ansi_rows { " over ANSI rows" } else { "" })
    };
    let mut edit = scene();
    baseline(renderer, &mut edit)?;
    write(edit.grid(), EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut edit, &label("one-row edit"), Expect::Partial)?;

    // A glyph two and a half rows tall attached to the row above, and one attached to the row
    // below reaching up into the edit row: each row's record reaches the damage.
    for (case, slot, top_row) in [
        ("tall glyph from the row above", EDIT_ROW - 1, f32::from(EDIT_ROW - 1)),
        ("tall glyph from the row below", EDIT_ROW + 1, f32::from(EDIT_ROW) - 0.5),
    ] {
        let rect_px = (
            layout.col_left(3),
            layout.grid_y + top_row * layout.cell_h,
            layout.cell_w,
            2.5 * layout.cell_h,
        );
        renderer.__inject_row_glyph(Some(InjectedRowGlyph {
            pane_id: PANE_ID,
            slot,
            rect_px,
            color: INJECTED_COLOR,
        }));
        let mut overhang = scene();
        baseline(renderer, &mut overhang)?;
        write(overhang.grid(), EDIT_ROW, 0, "xyz");
        narrow_matches_full(renderer, &mut overhang, &label(case), Expect::Partial)?;
    }
    renderer.__inject_row_glyph(None);

    let mut marks = scene();
    baseline(renderer, &mut marks)?;
    write(marks.grid(), EDIT_ROW, 0, "e\u{301}a\u{308}o\u{302}");
    narrow_matches_full(renderer, &mut marks, &label("combining marks"), Expect::Partial)?;

    let mut wide = scene();
    baseline(renderer, &mut wide)?;
    write(wide.grid(), EDIT_ROW, layout.cols - 4, "中😀");
    narrow_matches_full(
        renderer,
        &mut wide,
        &label("wide CJK and emoji at the edge"),
        Expect::Partial,
    )?;

    // Every underline style on the edit row, under an unchanged underlined neighbour row.
    let styles = [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    let mut underlined = scene();
    let styled = |grid: &mut Grid, row: u16| {
        grid.goto(row, 0);
        for (style, character) in styles.into_iter().zip("uline".chars()) {
            grid.put_char_styled(
                character,
                Color::Default,
                Color::Default,
                CellFlags::UNDERLINE,
                None,
                style,
                None,
            );
        }
    };
    styled(underlined.grid(), EDIT_ROW - 1);
    underlined.grid().goto(EDIT_ROW, 0);
    baseline(renderer, &mut underlined)?;
    styled(underlined.grid(), EDIT_ROW);
    narrow_matches_full(
        renderer,
        &mut underlined,
        &label("every underline style"),
        Expect::Partial,
    )?;

    // An unchanged image anchored on the row after the edit, three rows tall: the edit damages its
    // own row padded by one row each way, so the image's top row lies inside the damage and its
    // lower two rows outside it.
    let mut image = scene();
    let (image_row, image_col) = (EDIT_ROW + 1, 6);
    let (width, height) = ((3.0 * layout.cell_w) as u32, (3.0 * layout.cell_h) as u32);
    image.panes[0].images.push(InlineImage {
        id: 1,
        row: image_row,
        col: image_col,
        width,
        height,
        bgra: Arc::from(vec![200u8; (width * height * 4) as usize]),
    });
    let image_rect = outward((
        layout.col_left(image_col),
        layout.row_top(image_row),
        width as f32,
        height as f32,
    ));
    let strip_bottom = layout.strip_rect(EDIT_ROW).ok_or("the edit row's strip")?.bottom();
    check(
        image_rect.y < strip_bottom && strip_bottom < image_rect.bottom(),
        &format!("the image {image_rect:?} crosses the strip's edge {strip_bottom}"),
    )?;
    baseline(renderer, &mut image)?;
    write(image.grid(), EDIT_ROW, 0, "edit");
    let (damage, _, _) = narrow_matches_full(
        renderer,
        &mut image,
        &label("inline image crossing the damage"),
        Expect::Partial,
    )?;
    let inside = image_rect.intersect(damage.damage);
    check(
        inside.is_some_and(|inside| inside.h > 0 && inside.h < image_rect.h),
        &format!(
            "part of the image {image_rect:?} lies inside the damage {:?} and part outside: \
             {inside:?}",
            damage.damage
        ),
    )?;

    let mut select = scene();
    let selected_row = select.grid().scrollback_len() as u64 + u64::from(EDIT_ROW - 1);
    let mut selection = Selection::new(selected_row, 0);
    selection.extend(selected_row, 3);
    select.selection = Some(selection);
    baseline(renderer, &mut select)?;
    if let Some(selection) = select.selection.as_mut() {
        selection.extend(selected_row, 9);
    }
    narrow_matches_full(renderer, &mut select, &label("selection change"), Expect::Partial)?;

    let mut cursor = scene();
    cursor.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut cursor)?;
    for visible in [true, false] {
        cursor.cursor_visible = visible;
        narrow_matches_full(renderer, &mut cursor, &label("cursor toggle"), Expect::Partial)?;
    }
    Ok(())
}

/// Test 7: a one-row edit on a pane of at least 20 rows presents one partial frame, no full frame,
/// and hashes exactly the rows the planner emits, computed from the captured geometry and the
/// baseline records by [`edit_rows`]: the edited row, every row whose rounded strip meets the
/// edit's strip, and every row whose record meets it. How many rows that is depends on the pitch
/// (seven at 19.2 px, five at 20 px). The frame uploads fewer bytes than the same state planned
/// `Full`.
fn emission_and_upload_shrink(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    let expected = edit_rows(layout, EDIT_ROW, |row| renderer.__test_row_ink(PANE_ID, row));
    check(
        expected.contains(&EDIT_ROW) && expected.len() < usize::from(layout.rows),
        &format!("the edit reaches some rows, not all {}: {expected:?}", layout.rows),
    )?;
    write(scene.grid(), EDIT_ROW, 0, "edit");
    let before = counts(renderer);
    present(renderer, &mut scene)?;
    let partial = delta(before, counts(renderer));
    let cols = u64::from(layout.cols);
    let expected_cells = expected.len() as u64 * cols;
    check(
        partial.partial_frames == 1
            && partial.full_frames == 0
            && partial.row_cells_hashed == expected_cells,
        &format!(
            "one partial frame hashing rows {expected:?}, {expected_cells} cells: {partial:?}"
        ),
    )?;
    renderer.invalidate_retained_frame();
    let before = counts(renderer);
    present(renderer, &mut scene)?;
    let full = delta(before, counts(renderer));
    check(
        full.full_frames == 1 && full.row_cells_hashed == layout.cells(),
        &format!("the same state planned Full hashes every row: {full:?}"),
    )?;
    check(
        partial.uploaded_bytes < full.uploaded_bytes,
        &format!("the partial frame uploads less: {partial:?} vs {full:?}"),
    )
}

/// Tests 16 and 17, pixel parts: a row whose glyph overhangs into the damage from a neighbouring
/// pane, and a row whose tall glyph reaches the damage from five rows above, are emitted through
/// their ink records though their padded strips miss it, and the frame equals a full repaint.
fn overhanging_records(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    // Two side-by-side panes; pane 1's edit-row glyph runs four pixels past its right edge.
    let half_w = layout.pane.w / 2;
    let half_cols = ((half_w as f32 / layout.cell_w).floor() as u16).max(1);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    let mut split = Scene {
        opacity: OPACITY,
        receipts: Vec::new(),
        font_generation: None,
        panes: vec![
            ScenePane {
                id: PANE_ID,
                rect: PixelRect { w: half_w, ..layout.pane },
                grid: dense_grid(half_cols, layout.rows),
                images: Vec::new(),
                active: true,
            },
            ScenePane {
                id: NEIGHBOUR_ID,
                rect: PixelRect { x: half_w as i32, w: layout.pane.w - half_w, ..layout.pane },
                grid: dense_grid(half_cols, layout.rows),
                images: Vec::new(),
                active: false,
            },
        ],
        cursor_visible: false,
        selection: None,
        tabs,
    };
    renderer.__inject_row_glyph(Some(InjectedRowGlyph {
        pane_id: PANE_ID,
        slot: EDIT_ROW,
        rect_px: (half_w as f32 - 4.0, layout.row_top(EDIT_ROW), 12.0, layout.cell_h),
        color: INJECTED_COLOR,
    }));
    baseline(renderer, &mut split)?;
    let record =
        renderer.__test_row_ink(PANE_ID, EDIT_ROW).ok_or("the overhanging row's record")?;
    check(record.right() > half_w as i32, &format!("the record overhangs: {record:?}"))?;
    write(&mut split.panes[1].grid, EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut split, "neighbouring pane overhang", Expect::Partial)?;

    // A glyph four and a half rows tall attached to the row five above the edit.
    let tall_slot = EDIT_ROW - 5;
    renderer.__inject_row_glyph(Some(InjectedRowGlyph {
        pane_id: PANE_ID,
        slot: tall_slot,
        rect_px: (
            layout.col_left(3),
            layout.row_top(tall_slot),
            layout.cell_w,
            4.5 * layout.cell_h,
        ),
        color: INJECTED_COLOR,
    }));
    let mut vertical = single(layout);
    baseline(renderer, &mut vertical)?;
    write(vertical.grid(), EDIT_ROW, 0, "edit");
    let (damage, _, _) =
        narrow_matches_full(renderer, &mut vertical, "tall glyph five rows up", Expect::Partial)?;
    let record = renderer.__test_row_ink(PANE_ID, tall_slot).ok_or("the tall row's record")?;
    let strip = layout.strip_rect(tall_slot);
    check(
        record.intersect(damage.damage).is_some()
            && strip.is_none_or(|strip| strip.intersect(damage.damage).is_none()),
        &format!(
            "the tall row's record {record:?} reaches the damage {:?} its strip {strip:?} misses",
            damage.damage
        ),
    )?;
    renderer.__inject_row_glyph(None);
    Ok(())
}

/// Test 19, pixel part: a glyph four and a half rows tall under the hidden cursor. Showing the
/// cursor recolors it; widening the damage by its bounds reaches a row the partial plan did not
/// emit, so the frame is reassembled `Full` in the same frame, counted as one fallback and one
/// full frame, and equals a full repaint. Hiding it again emits the rows the previous recolor
/// reached, so no fallback is needed. Both frames go through the releasing call: the fallback's
/// receipt acknowledges every row (`AckRows::All`), the partial frame's only the rows it drew.
///
/// The geometry is checked first: the cursor cell covers at least the recolorer's 20% of the
/// glyph, and after the baseline [`skipped_reached_row`] finds, from the rounded strips and the
/// committed records, a dense row whose strip and record both miss the cursor's strip while its
/// record meets the glyph, so only the post-assembly check can catch it.
fn post_assembly_fallback(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let glyph = layout.cursor_glyph(CURSOR_ROW, CURSOR_COL, 4.5);
    let covered = layout.cursor_coverage(CURSOR_ROW, CURSOR_COL, glyph);
    check(covered >= 0.20, &format!("the cursor covers {covered} of the glyph, at least 20%"))?;
    renderer.__inject_test_glyph(Some((glyph, INJECTED_COLOR)));
    let mut scene = single(layout);
    scene.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut scene)?;
    let reached = skipped_reached_row(layout, CURSOR_ROW, outward(glyph), |row| {
        renderer.__test_row_ink(PANE_ID, row)
    });
    check(
        reached.is_some(),
        &format!(
            "a row above {CURSOR_ROW} misses the cursor strip {:?} in strip and record, and its \
             record meets the glyph {:?}",
            layout.strip_rect(CURSOR_ROW),
            outward(glyph)
        ),
    )?;
    scene.cursor_visible = true;
    let (_, _, fallback_receipts) = narrow_matches_full(
        renderer,
        &mut scene,
        "recolor reaching a skipped row",
        Expect::Fallback,
    )?;
    check(
        !fallback_receipts.is_empty()
            && fallback_receipts.iter().all(|receipt| receipt.rows == AckRows::All),
        &format!("the reassembled Full frame acknowledges every row: {fallback_receipts:?}"),
    )?;
    scene.cursor_visible = false;
    let (_, _, partial_receipts) = narrow_matches_full(
        renderer,
        &mut scene,
        "previous recolor rows emitted",
        Expect::Partial,
    )?;
    check(
        !partial_receipts.is_empty()
            && partial_receipts.iter().all(|receipt| matches!(receipt.rows, AckRows::Rows(_))),
        &format!("the partial frame acknowledges only its drawn rows: {partial_receipts:?}"),
    )?;
    renderer.__inject_test_glyph(None);
    Ok(())
}

/// A one-row edit armed to fail, from a fresh baseline: the edited row's record before the edit,
/// the retained pixels before it, and the counters before the failing frame.
fn failing_edit(
    renderer: &mut GpuRenderer,
    layout: &Layout,
) -> Result<(Scene, Option<PixelRect>, Vec<u8>, Counts), String> {
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    let record = renderer.__test_row_ink(PANE_ID, EDIT_ROW);
    let (pixels, _) = retained_pixels(renderer)?;
    write(scene.grid(), EDIT_ROW, 0, "fail");
    Ok((scene, record, pixels, counts(renderer)))
}

/// What a failed partial frame must leave: nothing presented or counted as presented, the plan
/// assembled as partial (some rows hashed, not all), no frame key, the edit's dirt kept, the edited
/// row's record and the retained pixels unchanged, and no presented damage.
fn nothing_committed(
    renderer: &mut GpuRenderer,
    scene: &Scene,
    record: Option<PixelRect>,
    pixels: Option<&[u8]>,
    before: Counts,
    case: &str,
) -> Result<(), String> {
    let moved = delta(before, counts(renderer));
    check(
        moved.partial_frames == 0
            && moved.full_frames == 0
            && moved.row_cells_hashed > 0
            && moved.row_cells_hashed < scene.cells(),
        &format!("{case}: a partial plan was assembled and nothing presented: {moved:?}"),
    )?;
    check(!renderer.__test_has_frame_key(), &format!("{case}: the frame key is cleared"))?;
    check(
        scene.panes[0].grid.dirty_rows().any(|row| row == usize::from(EDIT_ROW)),
        &format!("{case}: no receipt cleared the edit's dirt"),
    )?;
    check(
        renderer.__test_row_ink(PANE_ID, EDIT_ROW) == record,
        &format!("{case}: the edited row's record is unchanged"),
    )?;
    check(renderer.__take_presented_damage().is_none(), &format!("{case}: nothing presented"))?;
    if let Some(pixels) = pixels {
        let (after, _) = retained_pixels(renderer)?;
        check(after == pixels, &format!("{case}: the retained frame was not drawn"))?;
    }
    Ok(())
}

/// The frame after a failure is a whole-surface `Full` first frame that equals a full repaint and
/// rewrites the edited row's record.
fn retry_is_full(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    record: Option<PixelRect>,
    case: &str,
) -> Result<(), String> {
    let before = counts(renderer);
    let retried = present(renderer, scene)?;
    let moved = delta(before, counts(renderer));
    check(
        retried.first_frame && retried.damage == retried.surface,
        &format!("{case}: the retry repaints the surface: {retried:?}"),
    )?;
    check(
        moved.full_frames >= 1 && moved.partial_frames == 0,
        &format!("{case}: the retry is Full: {moved:?}"),
    )?;
    let (retried_pixels, _) = retained_pixels(renderer)?;
    renderer.invalidate_retained_frame();
    present(renderer, scene)?;
    let (full_pixels, _) = retained_pixels(renderer)?;
    check(retried_pixels == full_pixels, &format!("{case}: the retry equals a full repaint"))?;
    check(
        renderer.__test_row_ink(PANE_ID, EDIT_ROW) != record,
        &format!("{case}: the retry rewrites the edited row's record"),
    )
}

/// Tests 8a, 18 and 21: an acquisition Timeout or Occlusion after a partial plan was assembled
/// returns `SurfaceRetry` before the retained draw, commits no record, receipt or partial frame,
/// clears the key, and the retry is `Full`.
fn acquisition_failure(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    for reason in [SurfaceRetryReason::Timeout, SurfaceRetryReason::Occluded] {
        let case = format!("acquisition {reason:?}");
        let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
        renderer.__fail_next_surface_acquire(reason);
        let outcome = frame(renderer, &mut scene);
        check(
            matches!(outcome, PresentOutcome::SurfaceRetry(retried) if retried == reason),
            &format!("{case}: the frame is a surface retry: {outcome:?}"),
        )?;
        nothing_committed(renderer, &scene, record, Some(&pixels), before, &case)?;
        retry_is_full(renderer, &mut scene, record, &case)?;
    }
    Ok(())
}

/// Tests 9, 18 and 21: an atlas change during a partial assembly presents nothing, issues no
/// receipt, commits no record or partial frame, requests a redraw and clears the key, and the
/// retry is `Full`.
fn atlas_change(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
    renderer.__change_glyph_atlas_during_next_assembly();
    let outcome = frame(renderer, &mut scene);
    check(
        matches!(outcome, PresentOutcome::AtlasRetry),
        &format!("the frame is an atlas retry: {outcome:?}"),
    )?;
    check(
        delta(before, counts(renderer)).native_request_redraw >= 1,
        "the atlas retry requests a redraw",
    )?;
    nothing_committed(renderer, &scene, record, Some(&pixels), before, "atlas change")?;
    retry_is_full(renderer, &mut scene, record, "atlas change")
}

/// Tests 8b, 18 and 21: a submission that fails validation after the retained draw stops the
/// device; the frame is `RenderingUnavailable`, commits no record, receipt or partial frame and
/// clears the key. After recovery onto a new device, the first frame is `Full`.
fn submission_failure(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (mut scene, record, _pixels, before) = failing_edit(renderer, layout)?;
    renderer.__fail_next_frame_submission();
    let outcome = frame(renderer, &mut scene);
    check(
        matches!(outcome, PresentOutcome::RenderingUnavailable(_)),
        &format!("the frame is unavailable: {outcome:?}"),
    )?;
    // The retained frame was drawn into before the failure, so its pixels are not compared.
    nothing_committed(renderer, &scene, record, None, before, "submission failure")?;
    recover(renderer, active)?;
    retry_is_full(renderer, &mut scene, record, "submission failure")
}

/// Rebind `renderer` onto a newly requested device, in production's order: request, run,
/// prepare, commit.
fn recover(renderer: &mut GpuRenderer, active: &ActiveEventLoop) -> Result<(), String> {
    let request = renderer.recovery_request(active).map_err(|error| error.to_string())?;
    let recovered = request
        .run(|_generation| -> DeviceStateWaker { Arc::new(|| {}) })
        .map_err(|failure| failure.error().to_string())?;
    let (context, surface) = recovered.into_parts();
    let prepared = renderer
        .prepare_rebind(&context, Some(surface), SoftwareRenderMode::Off)
        .map_err(|error| error.to_string())?;
    renderer.commit_rebind(prepared).map_err(|error| error.to_string())?;
    check(renderer.device_accepts_gpu_work(), "the recovered device accepts work")
}

/// Test 24 and test 18's closed pane: a 2,000-row pane commits one record per row; shrunk to 40
/// rows its records fall to 40 and the reported bytes fall with the table, inside the class's
/// envelope; closing the pane drops its records.
fn row_ink_grows_and_shrinks(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let cols = layout.cols.min(200);
    let mut scene = single(layout);
    scene.panes[0].grid = dense_grid(cols, 2_000);
    present(renderer, &mut scene)?;
    let grown = renderer.retained_amounts().row_ink;
    check(grown.items == 2_000, &format!("one record per row of 2,000: {grown:?}"))?;
    let ClassCoverage::UnchargedRetention { per_owner_bytes } = ResourceClass::RowInk.coverage()
    else {
        return Err(String::from("RowInk records its uncharged envelope"));
    };
    check(grown.bytes <= per_owner_bytes, &format!("{grown:?} within {per_owner_bytes}"))?;
    scene.panes[0].grid = dense_grid(cols, 40);
    present(renderer, &mut scene)?;
    let shrunk = renderer.retained_amounts().row_ink;
    check(
        shrunk.items == 40 && shrunk.bytes * 10 < grown.bytes,
        &format!("the table releases its capacity: {grown:?} -> {shrunk:?}"),
    )?;
    renderer.invalidate_pane_caches(PANE_ID);
    check(
        renderer.retained_amounts().row_ink.items == 0
            && renderer.__test_row_ink(PANE_ID, 0).is_none(),
        "closing the pane drops its records",
    )
}

/// Test 11 at runtime: a degraded renderer presents through the GDI presenter, whose debug
/// assertion rejects a partial frame; a one-row edit there is a `Full` software frame with surface
/// damage and never partial.
fn degraded_is_never_partial(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    renderer.set_software_render_degrade(true);
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    write(scene.grid(), EDIT_ROW, 0, "edit");
    let before = counts(renderer);
    let damage = present(renderer, &mut scene)?;
    let moved = delta(before, counts(renderer));
    renderer.set_software_render_degrade(false);
    check(
        moved.partial_frames == 0 && moved.full_frames == 1 && moved.software_frames == 1,
        &format!("a degraded edit is one Full software frame: {moved:?}"),
    )?;
    check(damage.damage == damage.surface, &format!("degraded damage is the surface: {damage:?}"))
}

/// A grid holding only unique, non-blank rows of packaged ASCII and block glyphs, so a cold
/// renderer's frame misses on every emitted row and no row can hit another's entry.
fn unique_grid(cols: u16, rows: u16) -> Grid {
    let mut grid = Grid::new(cols, rows);
    for row in 0..rows {
        write(&mut grid, row, 0, &unique_row(u32::from(row), cols));
    }
    grid
}

/// The text of unique row `index`, at most `cols` characters: its index, then ASCII and blocks.
fn unique_row(index: u32, cols: u16) -> String {
    format!("{index:04} ab=>cd █▌│─ {index:x} MW").chars().take(usize::from(cols)).collect()
}

/// One active pane over the whole surface holding `unique_grid`, with a hidden cursor.
fn unique_scene(layout: &Layout) -> Scene {
    let mut scene = single(layout);
    scene.panes[0].grid = unique_grid(layout.cols, layout.rows);
    scene
}

/// Scroll the scene's grid one line and write a new unique row at the bottom.
fn scroll_one_line(scene: &mut Scene, rows: u16, cols: u16, index: u32) {
    let grid = scene.grid();
    grid.goto(rows - 1, 0);
    grid.carriage_return();
    grid.linefeed();
    write(grid, rows - 1, 0, &unique_row(index, cols));
}

/// The row-cache hits and misses `renderer` has counted.
fn row_cache_counts(renderer: &GpuRenderer) -> (u64, u64) {
    let stats = renderer.frame_stats();
    (stats.row_cache_hits, stats.row_cache_misses)
}

/// Draw `scene` once and require `Presented`, the presenter `degraded` names (a software frame
/// counted under GDI), and return the row-cache hits and misses the frame added.
fn presented_frame(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    degraded: bool,
    case: &str,
) -> Result<(u64, u64), String> {
    let (hits, misses) = row_cache_counts(renderer);
    let before = counts(renderer);
    let outcome = frame(renderer, scene);
    check(
        matches!(outcome, PresentOutcome::Presented),
        &format!("{case}: presented: {outcome:?}"),
    )?;
    let moved = delta(before, counts(renderer));
    check(
        renderer.is_software_render_degraded() == degraded
            && (!degraded || moved.software_frames >= 1),
        &format!("{case}: the expected presenter drew the frame: {moved:?}"),
    )?;
    let (hits_after, misses_after) = row_cache_counts(renderer);
    Ok((hits_after - hits, misses_after - misses))
}

/// The cold oracle: `oracle` first presents `scene` itself, so its atlas and font preparation are
/// warm, then drops every test pane's cached rows and the retained frame, which leave the atlas
/// alone, and presents `scene` again. That frame must hit no row and miss every emitted row.
fn cold_oracle_frame(
    oracle: &mut GpuRenderer,
    scene: &mut Scene,
    degraded: bool,
    case: &str,
) -> Result<(), String> {
    presented_frame(oracle, scene, degraded, case)?;
    for pane in &scene.panes {
        oracle.invalidate_pane_caches(pane.id);
    }
    oracle.invalidate_retained_frame();
    let rows: u64 = scene.panes.iter().map(|pane| u64::from(pane.grid.rows)).sum();
    let (hits, misses) = presented_frame(oracle, scene, degraded, case)?;
    check(
        hits == 0 && misses == rows,
        &format!("{case}: the oracle's row cache is cold: {hits} hits, {misses} misses of {rows}"),
    )
}

/// T8a and T8b at scales 1, 1.25, 1.5 and 2 on two wgpu renderers built alike: a warm candidate
/// scrolls one line (its moved rows hit) and must equal a cold oracle's Full repaint; then a
/// one-cell edit and a cursor toggle present one `Partial` frame that also equals the oracle.
fn warm_rows_match_a_cold_renderer(
    _renderer: &mut GpuRenderer,
    _layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    for scale in [1.0_f32, 1.25, 1.5, 2.0] {
        let case = format!("wgpu scale {scale}");
        let (window, mut candidate) = wgpu_renderer(active)?;
        let (_oracle_window, mut oracle) = wgpu_renderer(active)?;
        candidate.set_scale_factor(scale);
        oracle.set_scale_factor(scale);
        let layout = layout(&candidate, &window);
        let (mut warm_scene, mut cold_scene) = (unique_scene(&layout), unique_scene(&layout));
        baseline(&mut candidate, &mut warm_scene)?;
        let generation = warm_scene.font_generation;
        scroll_one_line(&mut warm_scene, layout.rows, layout.cols, 9_000);
        scroll_one_line(&mut cold_scene, layout.rows, layout.cols, 9_000);
        let before = counts(&candidate);
        let (hits, misses) = presented_frame(&mut candidate, &mut warm_scene, false, &case)?;
        let moved = delta(before, counts(&candidate));
        check(
            warm_scene.font_generation == generation,
            &format!("{case}: the font generation held across the scroll"),
        )?;
        check(
            moved.full_frames == 1 && moved.partial_frames == 0,
            &format!("{case}: the scroll is one Full assembly: {moved:?}"),
        )?;
        check(
            (hits, misses) == (u64::from(layout.rows) - 1, 1),
            &format!("{case}: every moved row hits and only the new row misses: {hits}, {misses}"),
        )?;
        let (warm, width) = retained_pixels(&mut candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, false, &case)?;
        let (cold, _) = retained_pixels(&mut oracle)?;
        check(
            first_difference(&warm, &cold, width).is_none(),
            &format!(
                "{case}: scrolled warm frame differs from the cold one at {:?}",
                first_difference(&warm, &cold, width)
            ),
        )?;

        for scene in [&mut warm_scene, &mut cold_scene] {
            write(scene.grid(), EDIT_ROW, 3, "Z");
            scene.cursor_visible = true;
        }
        let expected = expected_partial_rows(&layout, &warm_scene, &candidate);
        let (hits_before, misses_before) = row_cache_counts(&candidate);
        let before = counts(&candidate);
        candidate.__enable_emitted_rows();
        let narrow = present(&mut candidate, &mut warm_scene)?;
        let emitted = candidate.__take_emitted_rows();
        check(
            emitted == vec![(PANE_ID, expected.clone())],
            &format!("{case}: the frame emits exactly the rows {expected:?}: {emitted:?}"),
        )?;
        let moved = delta(before, counts(&candidate));
        let (hits_after, misses_after) = row_cache_counts(&candidate);
        let expected_rows = expected.len() as u64;
        check(
            moved.partial_frames == 1
                && moved.full_frames == 0
                && moved.partial_fallbacks == 0
                && !narrow.first_frame
                && narrow.is_narrow(),
            &format!("{case}: the edit presents one Partial frame: {moved:?} {narrow:?}"),
        )?;
        check(
            moved.row_cells_hashed == expected_rows * u64::from(layout.cols),
            &format!(
                "{case}: the frame emits exactly the rows {expected:?}: hashed {} cells",
                moved.row_cells_hashed
            ),
        )?;
        check(
            (hits_after - hits_before, misses_after - misses_before) == (expected_rows - 1, 1),
            &format!(
                "{case}: only the edited row misses among {expected:?}: {} hits, {} misses",
                hits_after - hits_before,
                misses_after - misses_before
            ),
        )?;
        let (warm, width) = retained_pixels(&mut candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, false, &case)?;
        let (cold, _) = retained_pixels(&mut oracle)?;
        check(
            first_difference(&warm, &cold, width).is_none(),
            &format!("{case}: the Partial frame differs from the cold Full repaint"),
        )?;
    }
    Ok(())
}

/// The rows a `Partial` frame of `scene` must emit, from the geometry and the grid's actual dirt:
/// the bounding union of every dirty row's ink-padded strip and the cursor row's strip (the cell
/// where the block cursor recolors), then every row whose strip or committed ink record meets
/// it. `Grid::goto` dirties the row the cursor leaves even when it is hidden, so the dirt is read
/// from the grid rather than assumed.
fn expected_partial_rows(layout: &Layout, scene: &Scene, renderer: &GpuRenderer) -> Vec<u16> {
    let grid = &scene.panes[0].grid;
    let mut reach: Vec<u16> = grid.dirty_rows().map(|row| row as u16).collect();
    if scene.cursor_visible {
        reach.push(grid.cursor.row);
    }
    let damage = reach
        .iter()
        .filter_map(|row| layout.strip_rect(*row))
        .reduce(|union, rect| union.union(rect));
    let Some(damage) = damage else {
        // When: nothing is dirty, a Partial frame emits no row.
        return Vec::new();
    };
    let meets = |rect: Option<PixelRect>| rect.is_some_and(|rect| rect.intersect(damage).is_some());
    (0..layout.rows)
        .filter(|row| {
            reach.contains(row)
                || meets(layout.strip_rect(*row))
                || meets(renderer.__test_row_ink(PANE_ID, *row))
        })
        .collect()
}

/// Every BGRA pixel of a GDI renderer's software frame, each read through the test hook and
/// required to exist. The hook and the GDI presenter exist only on Windows.
#[cfg(not(target_os = "windows"))]
fn gdi_pixels(_renderer: &GpuRenderer) -> Result<Vec<[u8; 4]>, String> {
    Err(String::from("the GDI presenter exists only on Windows"))
}

/// Every BGRA pixel of a GDI renderer's software frame, each read through the test hook and
/// required to exist.
#[cfg(target_os = "windows")]
fn gdi_pixels(renderer: &GpuRenderer) -> Result<Vec<[u8; 4]>, String> {
    let (width, height) = renderer.surface_size();
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for pixel_y in 0..height {
        for pixel_x in 0..width {
            pixels.push(
                renderer
                    .__test_software_frame_pixel_bgra(pixel_x, pixel_y)
                    .ok_or(format!("GDI pixel ({pixel_x}, {pixel_y}) is readable"))?,
            );
        }
    }
    Ok(pixels)
}

/// Pixels that differ from the frame's most common colour, its background.
fn foreground_pixels(pixels: &[[u8; 4]]) -> usize {
    let mut tally = std::collections::HashMap::new();
    for pixel in pixels {
        *tally.entry(*pixel).or_insert(0usize) += 1;
    }
    let background = tally.into_iter().max_by_key(|(_, count)| *count).map(|(pixel, _)| pixel);
    pixels.iter().filter(|pixel| Some(**pixel) != background).count()
}

/// T8c at scales 1, 1.25, 1.5 and 2: two renderers forced onto the GDI presenter, one warm (its
/// scrolled rows hit) and one cold, compose byte-identical software frames with visible text.
fn gdi_warm_and_cold_frames_match(
    _renderer: &mut GpuRenderer,
    _layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    for scale in [1.0_f32, 1.25, 1.5, 2.0] {
        let case = format!("GDI scale {scale}");
        let (window, mut candidate) = renderer_in_mode(active, SoftwareRenderMode::Force)?;
        let (_oracle_window, mut oracle) = renderer_in_mode(active, SoftwareRenderMode::Force)?;
        candidate.set_scale_factor(scale);
        oracle.set_scale_factor(scale);
        let layout = layout(&candidate, &window);
        let (mut warm_scene, mut cold_scene) = (unique_scene(&layout), unique_scene(&layout));
        candidate.invalidate_retained_frame();
        presented_frame(&mut candidate, &mut warm_scene, true, &case)?;
        scroll_one_line(&mut warm_scene, layout.rows, layout.cols, 9_100);
        scroll_one_line(&mut cold_scene, layout.rows, layout.cols, 9_100);
        let (hits, _) = presented_frame(&mut candidate, &mut warm_scene, true, &case)?;
        check(hits > 0, &format!("{case}: the warm GDI renderer replays moved rows"))?;
        let warm = gdi_pixels(&candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, true, &case)?;
        let cold = gdi_pixels(&oracle)?;
        check(
            foreground_pixels(&warm) > 100 && foreground_pixels(&cold) > 100,
            &format!("{case}: both GDI frames draw text"),
        )?;
        check(warm == cold, &format!("{case}: warm and cold GDI frames differ"))?;
    }
    Ok(())
}

/// T11's direct `Err` exits, each apart from an ordinary failed outcome: an `Err` from assembly
/// and an `Err` from the presenter call each return `Failed` with no receipt, discard the
/// staged glyph-row keys, keep the committed ones and the retained pixels, and the next
/// presented frame commits only its own keys and equals a full repaint.
fn error_exits_discard_staged_keys(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    for (case, assembly) in [("assembly Err", true), ("presenter Err", false)] {
        let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
        let committed = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        if assembly {
            renderer.__fail_next_assembly();
        } else {
            // When: the presenter seam is armed instead, the `Err` comes from presentation.
            renderer.__fail_next_present();
        }
        let (lookups_before_hits, lookups_before_misses) = row_cache_counts(renderer);
        let (outcome, raw_receipts) = frame_with_receipts(renderer, &mut scene);
        check(
            matches!(outcome, PresentOutcome::Failed(_)),
            &format!("{case}: Failed: {outcome:?}"),
        )?;
        check(
            raw_receipts.is_empty(),
            &format!("{case}: the releasing call returned no receipt: {raw_receipts:?}"),
        )?;
        let (lookups_after_hits, lookups_after_misses) = row_cache_counts(renderer);
        check(
            lookups_after_hits + lookups_after_misses > lookups_before_hits + lookups_before_misses,
            &format!(
                "{case}: row-cache lookups ran before the fault, so there was a stage to discard"
            ),
        )?;
        let moved = delta(before, counts(renderer));
        check(
            moved.partial_frames == 0 && moved.full_frames == 0,
            &format!("{case}: nothing presented: {moved:?}"),
        )?;
        check(
            scene.panes[0].grid.dirty_rows().any(|row| row == usize::from(EDIT_ROW)),
            &format!("{case}: no receipt cleared the edit's dirt"),
        )?;
        let keys = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        check(
            keys.map(|(_, staged)| staged) == Some(0)
                && keys.map(|(kept, _)| kept) == committed.map(|(kept, _)| kept),
            &format!("{case}: staged keys discarded, committed kept: {committed:?} -> {keys:?}"),
        )?;
        check(
            renderer.__test_row_ink(PANE_ID, EDIT_ROW) == record,
            &format!("{case}: the edited row's record is unchanged"),
        )?;
        let (after, _) = retained_pixels(renderer)?;
        check(after == pixels, &format!("{case}: the retained frame was not drawn"))?;
        present(renderer, &mut scene)?;
        let next = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        check(
            next.is_some_and(|(kept, _)| Some(kept) != committed.map(|(old, _)| old)),
            &format!("{case}: the next presented frame commits its own key: {next:?}"),
        )?;
        let (next_pixels, _) = retained_pixels(renderer)?;
        renderer.invalidate_retained_frame();
        present(renderer, &mut scene)?;
        let (full, _) = retained_pixels(renderer)?;
        check(next_pixels == full, &format!("{case}: the next frame equals a full repaint"))?;
    }
    Ok(())
}

/// One case: a renderer and its layout, and the event loop recovery runs on.
type Case = fn(&mut GpuRenderer, &Layout, &ActiveEventLoop) -> Result<(), String>;

/// Repaint every glyph the cases draw until the font-fallback generation the renderer applies has
/// held still. A fallback search runs on a worker: a publication applied between a case's narrow
/// frame and its full reference clears the frame key and redraws glyphs outside the narrow damage
/// with the new faces, so the reference would no longer show the narrow frame's state. This is a
/// bounded quiet heuristic, not a worker-completion barrier; each parity case rejects a later
/// apply.
fn settle_fallback(renderer: &mut GpuRenderer, layout: &Layout) -> Result<(), String> {
    // The dense grid's last row holds every text an edit writes; the underline case adds styles,
    // not glyphs.
    let mut scene = single(layout);
    let mut quiet = FallbackQuiet::new(Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        renderer.invalidate_retained_frame();
        present(renderer, &mut scene)?;
        let generation = scene.font_generation.ok_or("a presented frame records its fonts")?;
        let tofu = !renderer.last_missing_tofu().is_empty();
        if quiet.observe(generation, tofu, Instant::now()) {
            // When: the applied generation stays quiet, start cases; their checks reject a later apply.
            return Ok(());
        }
        check(
            Instant::now() < deadline,
            &format!(
                "the font fallback settles; generation {generation}, tofu {:?}",
                renderer.last_missing_tofu()
            ),
        )?;
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let (window, mut renderer) = match wgpu_renderer(active) {
        Ok(built) => built,
        Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
            // When: the host has no adapter, there is no retained wgpu frame to compare.
            println!("capability={reason}");
            return Ok(());
        }
        Err(reason) => return Err(reason),
    };
    let layout = layout(&renderer, &window);
    check(layout.rows >= 20, &format!("the pane holds at least 20 rows: {}", layout.rows))?;
    settle_fallback(&mut renderer, &layout)?;
    let mut failures = Vec::new();
    // Recovery rebinds the device and degrading switches presenters, so those run last.
    let cases: [(&str, Case); 12] = [
        ("pixel parity", pixel_parity),
        ("emission and upload", emission_and_upload_shrink),
        ("overhanging records", overhanging_records),
        ("post-assembly fallback", post_assembly_fallback),
        ("acquisition failure", acquisition_failure),
        ("atlas change", atlas_change),
        ("error exits discard staged glyph keys", error_exits_discard_staged_keys),
        ("warm glyph rows match a cold renderer", warm_rows_match_a_cold_renderer),
        ("GDI warm and cold frames", gdi_warm_and_cold_frames_match),
        ("row ink grows and shrinks", row_ink_grows_and_shrinks),
        ("submission failure", submission_failure),
        ("degraded is never partial", degraded_is_never_partial),
    ];
    for (name, case) in cases {
        if let Err(error) = case(&mut renderer, &layout, active) {
            failures.push(format!("{name}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// A 640 x 480 single-pane layout at `cell_h` pitch from grid origin `grid_y`, with the ink pad
/// production derives at line height 1, `ceil(cell_h)`.
fn fixture_layout(cell_h: f32, grid_y: f32) -> Layout {
    Layout {
        pane: PixelRect { x: 0, y: 0, w: 640, h: 480 },
        grid_x: 0.0,
        grid_y,
        cell_w: 9.6,
        cell_h,
        ink_pad: cell_h.ceil(),
        cols: 66,
        rows: 24,
    }
}

/// Each row's record as dense text leaves it: its cell box rounded outward, across the grid.
fn cell_box_records(layout: &Layout) -> impl Fn(u16) -> Option<PixelRect> + '_ {
    move |row| {
        Some(outward((
            layout.grid_x,
            layout.row_top(row),
            f32::from(layout.cols) * layout.cell_w,
            layout.cell_h,
        )))
    }
}

/// The strips round outward as the planner does at a fractional pitch: at 19.2 px with a 20 px
/// pad, row 10's strip is [172, 232), row 8's starts at 133 and row 5's ends at 136; at 20 px,
/// row 10's strip is [180, 240).
#[test]
fn strips_round_outward_at_fractional_and_integral_pitch() {
    let fractional = fixture_layout(19.2, 0.0);
    let edit = fractional.strip_rect(10).unwrap();
    assert_eq!((edit.y, edit.bottom()), (172, 232));
    assert_eq!(fractional.strip_rect(8).unwrap().y, 133);
    assert_eq!(fractional.strip_rect(5).unwrap().bottom(), 136);
    let integral = fixture_layout(20.0, 0.0).strip_rect(10).unwrap();
    assert_eq!((integral.y, integral.bottom()), (180, 240));
}

/// A one-row edit emits the rows whose rounded strips or records meet its strip: rows 7 to 13 at
/// 19.2 px (from origin 0 or 7.5), rows 8 to 12 at 20 px, and a row whose record reaches into the
/// strip from far above as well.
#[test]
fn edit_rows_follow_the_rounded_strips_and_records() {
    for (cell_h, grid_y, expected) in
        [(19.2, 0.0, 7..=13), (19.2, 7.5, 7..=13), (20.0, 0.0, 8..=12)]
    {
        let layout = fixture_layout(cell_h, grid_y);
        let rows = edit_rows(&layout, 10, cell_box_records(&layout));
        assert_eq!(rows, expected.collect::<Vec<u16>>(), "cell_h {cell_h} from {grid_y}");
    }
    // Row 3's tall ink reaches y 175, inside row 10's strip [172, 232) at 19.2 px.
    let layout = fixture_layout(19.2, 0.0);
    let boxes = cell_box_records(&layout);
    let tall_row_three = |row: u16| {
        if row == 3 {
            Some(PixelRect { x: 0, y: 57, w: 640, h: 118 })
        } else {
            boxes(row)
        }
    };
    assert_eq!(edit_rows(&layout, 10, tall_row_three), [3, 7, 8, 9, 10, 11, 12, 13]);
}

/// Showing the cursor at row 8 over a glyph four and a half rows tall covers 1/4.5 of it, above
/// the recolorer's 20%, and the nearest row whose strip and record miss the cursor's strip while
/// its record meets the glyph is row 4 at 19.2 px (from origin 0 or 7.5) and row 5 at 20 px.
#[test]
fn the_skipped_reached_row_is_computed_from_strips_and_records() {
    for (cell_h, grid_y, expected) in [(19.2, 0.0, 4), (19.2, 7.5, 4), (20.0, 0.0, 5)] {
        let layout = fixture_layout(cell_h, grid_y);
        let glyph = layout.cursor_glyph(8, 4, 4.5);
        let coverage = layout.cursor_coverage(8, 4, glyph);
        assert!(coverage >= 0.20, "cell_h {cell_h}: the cursor covers {coverage}");
        let reached = skipped_reached_row(&layout, 8, outward(glyph), cell_box_records(&layout));
        assert_eq!(reached, Some(expected), "cell_h {cell_h} from {grid_y}");
    }
}

/// A row qualifies only when its record meets the glyph and misses the cursor's strip: at 19.2 px
/// a glyph two rows tall (from y 134) reaches no row whose strip misses the cursor's strip [133,
/// 193), and when row 4's record runs down to y 140, inside that strip, row 4 no longer qualifies
/// and no row above it meets the four-and-a-half-row glyph.
#[test]
fn a_skipped_row_must_meet_the_glyph_and_miss_the_cursor_strip_with_its_record() {
    let layout = fixture_layout(19.2, 0.0);
    let short = layout.cursor_glyph(8, 4, 2.0);
    assert_eq!(skipped_reached_row(&layout, 8, outward(short), cell_box_records(&layout)), None);
    let boxes = cell_box_records(&layout);
    let reaching_row_four = |row: u16| {
        if row == 4 {
            Some(PixelRect { x: 0, y: 76, w: 640, h: 64 })
        } else {
            boxes(row)
        }
    };
    let tall = outward(layout.cursor_glyph(8, 4, 4.5));
    assert_eq!(skipped_reached_row(&layout, 8, tall, reaching_row_four), None);
}

/// The fallback watch settles only after the generation holds still: one second without tofu,
/// restarted by a change, and five seconds while tofu remains.
#[test]
fn fallback_quiet_waits_for_the_generation_to_hold_still() {
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut quiet = FallbackQuiet::new(Duration::from_secs(1));
    assert!(!quiet.observe(0, false, at(0)));
    assert!(!quiet.observe(0, false, at(500)));
    // A publication restarts the wait.
    assert!(!quiet.observe(1, false, at(1_200)));
    assert!(!quiet.observe(1, false, at(2_000)));
    assert!(quiet.observe(1, false, at(2_200)));
    let mut searching = FallbackQuiet::new(Duration::from_secs(1));
    assert!(!searching.observe(0, true, at(0)));
    assert!(!searching.observe(0, true, at(2_000)));
    assert!(searching.observe(0, true, at(5_000)));
}

/// Differing pixels are counted and bounded outside the excluded rectangle only, or everywhere
/// without one; identical frames differ nowhere.
#[test]
fn differing_pixels_are_counted_and_bounded_outside_the_exclusion() {
    let width_px = 4;
    let first = vec![0u8; 4 * 4 * 3];
    let mut second = first.clone();
    for (column, row) in [(1usize, 1usize), (3, 2)] {
        second[(row * 4 + column) * 4] = 9;
    }
    let edit_row = PixelRect { x: 0, y: 1, w: 2, h: 1 };
    assert_eq!(
        differing_pixels(&first, &second, width_px, Some(edit_row)),
        (1, Some(PixelRect { x: 3, y: 2, w: 1, h: 1 }))
    );
    assert_eq!(
        differing_pixels(&first, &second, width_px, None),
        (2, Some(PixelRect { x: 1, y: 1, w: 3, h: 2 }))
    );
    assert_eq!(differing_pixels(&first, &first, width_px, None), (0, None));
}

/// A fallback applied between the baseline and the narrow frame makes production repaint the narrow
/// frame in full; the check names the generation change, not the whole-surface damage. Matching
/// generations with narrow damage pass, and a whole-surface frame at one generation still fails.
#[test]
fn a_generation_change_before_the_narrow_frame_fails_by_name() {
    let surface = PixelRect { x: 0, y: 0, w: 640, h: 480 };
    let repainted = PresentedDamage { first_frame: true, damage: surface, surface };
    let error = check_narrow_frame("edit", Some(0), Some(1), &repainted).unwrap_err();
    assert!(
        error.contains("font fallback changed between baseline Some(0) and narrow Some(1)"),
        "{error}"
    );
    let narrow = PresentedDamage {
        first_frame: false,
        damage: PixelRect { x: 0, y: 191, w: 640, h: 60 },
        surface,
    };
    assert_eq!(check_narrow_frame("edit", Some(0), Some(0), &narrow), Ok(()));
    let error = check_narrow_frame("edit", Some(0), Some(0), &repainted).unwrap_err();
    assert!(error.contains("damages less than the surface"), "{error}");
}

/// Partial assembly on a real wgpu renderer: narrowed frames equal full repaints, failures after
/// a partial plan commit nothing and retry Full, ink records grow and shrink with the pane, and a
/// degraded frame is never partial.
#[cfg(target_os = "windows")]
#[test]
fn windows_partial_assembly_on_a_real_renderer() {
    use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("partial assembly event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
