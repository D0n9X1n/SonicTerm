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
    let cases: [(&str, Case); 14] = [
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
        ("moved strip routes until presented", moved_strip_routes_until_presented),
        ("strip rasterizes anew after size changes", strip_rasterizes_anew_after_size_changes),
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

#[path = "support/windows_partial_assembly_renderer.rs"]
mod renderer;
use renderer::renderer_in_mode;
use renderer::wgpu_renderer;
#[path = "support/windows_partial_assembly_parity.rs"]
mod parity;
use parity::emission_and_upload_shrink;
use parity::overhanging_records;
use parity::parity_cases;
use parity::post_assembly_fallback;
#[path = "support/windows_partial_assembly_failures.rs"]
mod failures;
use failures::acquisition_failure;
use failures::atlas_change;
use failures::failing_edit;
use failures::submission_failure;
#[path = "support/windows_partial_assembly_strip.rs"]
mod strip;
use strip::moved_strip_routes_until_presented;
use strip::strip_rasterizes_anew_after_size_changes;
#[path = "support/windows_partial_assembly_rows.rs"]
mod rows;
use rows::degraded_is_never_partial;
use rows::error_exits_discard_staged_keys;
use rows::gdi_warm_and_cold_frames_match;
use rows::row_ink_grows_and_shrinks;
use rows::warm_rows_match_a_cold_renderer;

#[path = "support/windows_partial_assembly_geometry.rs"]
mod geometry;
use geometry::{edit_rows, outward, skipped_reached_row};
