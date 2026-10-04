//! Partial assembly on a real wgpu renderer, read back from its retained frame.
//!
//! Pixel parity: each narrowed case presents a `Partial` frame that emits fewer rows than the pane
//! has, and its retained pixels equal a full repaint of the same state, over a translucent
//! background. Failures after a partial plan (an acquisition retry, an atlas change during
//! assembly, a submission that fails after the retained draw) commit nothing, keep the dirt, clear
//! the key and retry as a `Full` frame. The row-ink table grows with a 2,000-row pane and releases
//! its records when the pane shrinks or closes, and a degraded frame is never partial.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::{path::PathBuf, sync::Arc};

use sonicterm_gpu::{
    core::{
        unpad_readback_rows, GlyphAtlasStart, GpuRenderer, InjectedRowGlyph, PresentOutcome,
        PresentedDamage, RendererSettings, SurfaceAppearance, SurfaceRetryReason,
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
    CursorStyle, InlineImage, PaneRender, PixelRect,
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

    /// The top and bottom of row `row`'s ink-padded damage strip, rounded outward as the planner
    /// rounds them.
    fn strip(&self, row: u16) -> (f32, f32) {
        (
            (self.row_top(row) - self.ink_pad).floor(),
            (self.row_top(row) + self.cell_h + self.ink_pad).ceil(),
        )
    }

    /// Every cell of one pane of this layout.
    fn cells(&self) -> u64 {
        u64::from(self.cols) * u64::from(self.rows)
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
            software_render_mode: SoftwareRenderMode::Off,
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
    check(!renderer.is_software_render_degraded(), "the renderer presents through wgpu")?;
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
/// the cell height is the font's integer raster height, and the ink pad is that height rounded up.
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

/// Draw `scene` once through the compatibility path, which applies its receipts to the grids.
fn frame(renderer: &mut GpuRenderer, scene: &mut Scene) -> PresentOutcome {
    let theme = Theme::default();
    let fonts = renderer.begin_frame_fonts();
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
    renderer.render_with_outcome(
        &fonts,
        &mut panes,
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
    )
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

/// The first pixel `(x, y)` where two equally sized frames differ, if any.
fn first_difference(first: &[u8], second: &[u8], width_px: u32) -> Option<(usize, usize)> {
    let width = width_px as usize;
    first
        .chunks(4)
        .zip(second.chunks(4))
        .position(|(left, right)| left != right)
        .map(|index| (index % width, index / width))
}

/// How a narrowed frame is expected to be assembled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// A presented `Partial` frame that hashes fewer cells than the scene has.
    Partial,
    /// A `Partial` plan reassembled `Full` in the same frame by the post-assembly check.
    Fallback,
}

/// Present `scene` with narrow damage, check how it was assembled, then repaint the same state in
/// full and require the two retained frames equal byte for byte over a translucent background.
/// Returns the narrow frame's damage and its counter deltas.
fn narrow_matches_full(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    case: &str,
    expect: Expect,
) -> Result<(PresentedDamage, Counts), String> {
    let before = counts(renderer);
    let narrow = present(renderer, scene)?;
    let moved = delta(before, counts(renderer));
    check(
        !narrow.first_frame && narrow.is_narrow(),
        &format!("{case}: the frame damages less than the surface: {narrow:?}"),
    )?;
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
    renderer.invalidate_retained_frame();
    let full = present(renderer, scene)?;
    check(full.first_frame, &format!("{case}: the comparison frame repaints in full"))?;
    let (full_pixels, _) = retained_pixels(renderer)?;
    check(
        narrow_pixels.len() == full_pixels.len(),
        &format!("{case}: both frames read back at one size"),
    )?;
    let difference = first_difference(&narrow_pixels, &full_pixels, width_px);
    check(
        difference.is_none(),
        &format!(
            "{case}: narrow damage {:?} equals a full repaint; first differing pixel {difference:?}",
            narrow.damage
        ),
    )?;
    // The background is translucent, so the cleared ground keeps an alpha below opaque.
    let corner = &full_pixels[..4];
    check(corner[3] < u8::MAX, &format!("{case}: the background is translucent: {corner:?}"))?;
    Ok((narrow, moved))
}

/// Test 6: every acceptance case presents `Partial`, emitting fewer rows than the pane has, and
/// equals a full repaint: a one-row edit, a tall glyph overhanging from the row above and from the
/// row below, combining marks, wide CJK and emoji at the pane's right edge, every underline style,
/// an inline image crossing the damage edge, a selection change and a cursor toggle, each over the
/// translucent background.
fn pixel_parity(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let mut edit = single(layout);
    baseline(renderer, &mut edit)?;
    write(edit.grid(), EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut edit, "one-row edit", Expect::Partial)?;

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
        let mut overhang = single(layout);
        baseline(renderer, &mut overhang)?;
        write(overhang.grid(), EDIT_ROW, 0, "xyz");
        narrow_matches_full(renderer, &mut overhang, case, Expect::Partial)?;
    }
    renderer.__inject_row_glyph(None);

    let mut marks = single(layout);
    baseline(renderer, &mut marks)?;
    write(marks.grid(), EDIT_ROW, 0, "e\u{301}a\u{308}o\u{302}");
    narrow_matches_full(renderer, &mut marks, "combining marks", Expect::Partial)?;

    let mut wide = single(layout);
    baseline(renderer, &mut wide)?;
    write(wide.grid(), EDIT_ROW, layout.cols - 4, "中😀");
    narrow_matches_full(renderer, &mut wide, "wide CJK and emoji at the edge", Expect::Partial)?;

    // Every underline style on the edit row, under an unchanged underlined neighbour row.
    let styles = [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    let mut underlined = single(layout);
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
    narrow_matches_full(renderer, &mut underlined, "every underline style", Expect::Partial)?;

    // An unchanged image anchored on the row above, two rows tall, so it crosses the damage edge.
    let mut image = single(layout);
    let (width, height) = ((3.0 * layout.cell_w) as u32, (2.0 * layout.cell_h) as u32);
    image.panes[0].images.push(InlineImage {
        id: 1,
        row: EDIT_ROW - 1,
        col: 6,
        width,
        height,
        bgra: Arc::from(vec![200u8; (width * height * 4) as usize]),
    });
    baseline(renderer, &mut image)?;
    write(image.grid(), EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut image, "inline image crossing the damage", Expect::Partial)?;

    let mut select = single(layout);
    let selected_row = select.grid().scrollback_len() as u64 + u64::from(EDIT_ROW - 1);
    let mut selection = Selection::new(selected_row, 0);
    selection.extend(selected_row, 3);
    select.selection = Some(selection);
    baseline(renderer, &mut select)?;
    if let Some(selection) = select.selection.as_mut() {
        selection.extend(selected_row, 9);
    }
    narrow_matches_full(renderer, &mut select, "selection change", Expect::Partial)?;

    let mut cursor = single(layout);
    cursor.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut cursor)?;
    for visible in [true, false] {
        cursor.cursor_visible = visible;
        narrow_matches_full(renderer, &mut cursor, "cursor toggle", Expect::Partial)?;
    }
    Ok(())
}

/// Test 7: a one-row edit on a pane of at least 20 rows presents one partial frame, no full frame,
/// hashes a whole number of rows (the edited row and the rows its one-row ink pad reaches, about
/// five), and uploads fewer bytes than the same state planned `Full`.
fn emission_and_upload_shrink(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    write(scene.grid(), EDIT_ROW, 0, "edit");
    let before = counts(renderer);
    present(renderer, &mut scene)?;
    let partial = delta(before, counts(renderer));
    let cols = u64::from(layout.cols);
    check(
        partial.partial_frames == 1
            && partial.full_frames == 0
            && partial.row_cells_hashed.is_multiple_of(cols)
            && (3 * cols..=7 * cols).contains(&partial.row_cells_hashed),
        &format!("one partial frame hashing about five rows of {cols} cells: {partial:?}"),
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
    let (damage, _) =
        narrow_matches_full(renderer, &mut vertical, "tall glyph five rows up", Expect::Partial)?;
    let record = renderer.__test_row_ink(PANE_ID, tall_slot).ok_or("the tall row's record")?;
    check(
        record.bottom() > damage.damage.y,
        &format!("the tall row's record {record:?} reaches the damage {:?}", damage.damage),
    )?;
    renderer.__inject_row_glyph(None);
    Ok(())
}

/// Test 19, pixel part: a glyph four and a half rows tall under the hidden cursor. Showing the
/// cursor recolors it; widening the damage by its bounds reaches a row the partial plan did not
/// emit, so the frame is reassembled `Full` in the same frame, counted as one fallback and one
/// full frame, and equals a full repaint. Hiding it again emits the rows the previous recolor
/// reached, so no fallback is needed.
///
/// Before presenting, the geometry is checked: the cursor cell covers at least the recolorer's
/// 20% of the glyph, and the glyph reaches a dense row whose padded strip misses the cursor rows'
/// strips and whose record misses the cursor cell, so only the post-assembly check can catch it.
fn post_assembly_fallback(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let cursor_left = layout.col_left(CURSOR_COL);
    let cursor_top = layout.row_top(CURSOR_ROW);
    let cursor_bottom = cursor_top + layout.cell_h;
    let tall_h = 4.5 * layout.cell_h;
    let glyph_top = cursor_bottom - tall_h;
    let covered = (layout.cell_w * layout.cell_h) / (layout.cell_w * tall_h);
    check(covered >= 0.20, &format!("the cursor covers {covered} of the glyph, at least 20%"))?;
    // The cursor toggle damages the cursor row's padded strip; the row three above it is the
    // nearest whose strip ends at or above that strip's top while its cell meets the glyph.
    let (damage_top, _) = layout.strip(CURSOR_ROW);
    let reached = CURSOR_ROW - 3;
    let (_, reached_strip_bottom) = layout.strip(reached);
    check(
        reached_strip_bottom <= damage_top
            && glyph_top < layout.row_top(reached) + layout.cell_h
            && reached != EDIT_ROW,
        &format!(
            "row {reached} ({}..{reached_strip_bottom}) misses the cursor strip from {damage_top} \
             and meets the glyph from {glyph_top}",
            layout.strip(reached).0
        ),
    )?;
    renderer.__inject_test_glyph(Some((
        (cursor_left, glyph_top, layout.cell_w, tall_h),
        INJECTED_COLOR,
    )));
    let mut scene = single(layout);
    scene.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut scene)?;
    let record = renderer.__test_row_ink(PANE_ID, reached).ok_or("the reached row's record")?;
    check(
        (record.y as f32) < cursor_top
            && record.bottom() as f32 > glyph_top
            && record.bottom() as f32 <= damage_top,
        &format!(
            "row {reached}'s record {record:?} meets the glyph from {glyph_top} and misses the \
             cursor strip from {damage_top}"
        ),
    )?;
    scene.cursor_visible = true;
    narrow_matches_full(renderer, &mut scene, "recolor reaching a skipped row", Expect::Fallback)?;
    scene.cursor_visible = false;
    narrow_matches_full(renderer, &mut scene, "previous recolor rows emitted", Expect::Partial)?;
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

/// One case: a renderer and its layout, and the event loop recovery runs on.
type Case = fn(&mut GpuRenderer, &Layout, &ActiveEventLoop) -> Result<(), String>;

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
    let mut failures = Vec::new();
    // Recovery rebinds the device and degrading switches presenters, so those run last.
    let cases: [(&str, Case); 9] = [
        ("pixel parity", pixel_parity),
        ("emission and upload", emission_and_upload_shrink),
        ("overhanging records", overhanging_records),
        ("post-assembly fallback", post_assembly_fallback),
        ("acquisition failure", acquisition_failure),
        ("atlas change", atlas_change),
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
