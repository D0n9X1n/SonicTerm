#![cfg(test)]
//! Shared test-only row fixture state and native custody.

use super::*;

/// Facts for the fallback orchestration tests: a 4-row pane on a 240x160 surface, no tab bar.
pub(in crate::core) fn fallback_facts() -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 160, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Never,
        degraded: false,
        tab_bar_top: None,
        status_band: None,
        scale: 1.0,
        previous_recolor: crate::cursor::RecolorRecord::default(),
    }
}

/// The 4-row pane of `fallback_facts` at `revision`, with `dirty_rows` and per-slot `row_ink`.
pub(in crate::core) fn fallback_pane(
    revision: u64,
    dirty_rows: Vec<usize>,
    row_ink: Vec<Option<PixelRect>>,
) -> PaneMetadata {
    PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 100, h: 84 },
        cols: 8,
        rows: 4,
        scrollback_len: 0,
        viewport_top_abs: None,
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink,
    }
}

// ---------------------------------------------------------------------------
// Content-keyed row glyph cache: production seams driven with real fonts and plans.
// ---------------------------------------------------------------------------

/// One emitted row: whether it replayed, its key, its projected glyphs, and its decorations.
pub(in crate::core) struct EmittedRow {
    pub(in crate::core) replayed: bool,
    pub(in crate::core) key: u64,
    pub(in crate::core) glyphs: Vec<GlyphInstance>,
    pub(in crate::core) decorations: String,
    pub(in crate::core) missing: Vec<char>,
}

impl EmittedRow {
    /// The row's projected glyph bytes, for bit-exact comparison.
    pub(in crate::core) fn glyph_bytes(&self) -> Vec<u8> {
        bytemuck::cast_slice(&self.glyphs).to_vec()
    }
}

/// A renderer's glyph state for driving `emit_row_glyphs` and `assemble_pane_glyph_rows`
/// directly: the atlas, the content-keyed cache, the fonts and the row geometry.
pub(in crate::core) struct GlyphRig {
    pub(in crate::core) atlas: GlyphAtlas,
    pub(in crate::core) cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    pub(in crate::core) stack: Option<sonicterm_engine::FontStack>,
    pub(in crate::core) raster: Option<sonicterm_engine::FontStack>,
    pub(in crate::core) theme: Theme,
    pub(in crate::core) cell_size: (f32, f32),
    pub(in crate::core) baseline_y_in_cell: f32,
    pub(in crate::core) origin: (f32, f32),
    pub(in crate::core) surface: (f32, f32),
    pub(in crate::core) software_presenter: bool,
}

impl GlyphRig {
    /// A rig with the Rec Mono fixture fonts, 10x20 cells, baseline 16 and a 400x200 surface.
    pub(in crate::core) fn new(software_presenter: bool) -> Self {
        Self::with_stack(rec_mono_font_stack(), software_presenter)
    }

    /// A rig shaping and rasterizing with `stack`, otherwise as [`GlyphRig::new`] builds one.
    pub(in crate::core) fn with_stack(
        stack: sonicterm_engine::FontStack,
        software_presenter: bool,
    ) -> Self {
        Self {
            atlas: GlyphAtlas::new(1024, 1024),
            cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            raster: Some(stack.clone()),
            stack: Some(stack),
            theme: Theme::default(),
            cell_size: (10.0, 20.0),
            baseline_y_in_cell: 16.0,
            origin: (0.0, 0.0),
            surface: (400.0, 200.0),
            software_presenter,
        }
    }

    /// The rig's shaping inputs, borrowed for one call.
    pub(in crate::core) fn shaping(&mut self) -> GlyphShaping<'_> {
        GlyphShaping {
            atlas: &mut self.atlas,
            row_cache: &mut self.cache,
            font_stack: self.stack.as_ref(),
            wt_raster: self.raster.as_mut(),
            style_rev: 0,
            theme: &self.theme,
            fg_default: ChromeColor::rgb(230, 230, 230),
            raster_px: 14.0,
            cell_size: self.cell_size,
            surface: self.surface,
            baseline_y_in_cell: self.baseline_y_in_cell,
            hovered_url_accent: [0.0; 4],
            software_presenter: self.software_presenter,
        }
    }

    /// Start one cache pass drawing pane 7 at `grid`'s size.
    pub(in crate::core) fn begin(&mut self, grid: &Grid) {
        self.cache.begin_frame(&[(7, grid.rows, grid.cols)]);
    }

    /// Emit the row at `slot` of a view whose top is `view_top_abs`, pinning its key with the
    /// committed slots first and staging it after, as the pane pass does for one row.
    pub(in crate::core) fn emit(
        &mut self,
        grid: &Grid,
        view_top_abs: u64,
        slot: u16,
    ) -> EmittedRow {
        let snapped = build_snapped_cell_x(self.origin.0, self.cell_size.0, grid.cols);
        let origin = self.origin;
        let mut shaping = self.shaping();
        let key = emitted_row_key(&shaping, grid, view_top_abs, slot, None);
        shaping.row_cache.pin(7, &[key]);
        let (mut glyphs, mut underlines, mut tofu) = (Vec::new(), Vec::new(), Vec::new());
        let (mut missing, mut spans) = (Vec::new(), Vec::new());
        let replayed = emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id: 7,
                grid,
                view_top_abs,
                slot,
                origin,
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
                key,
            },
            GlyphFrame {
                glyph_instances: &mut glyphs,
                underlines: &mut underlines,
                missing_tofu: &mut tofu,
                missing_chars_this_frame: &mut missing,
                row_spans: &mut spans,
            },
        );
        shaping.row_cache.stage_slot(7, slot, key);
        EmittedRow {
            replayed,
            key,
            glyphs,
            decorations: format!("{underlines:?} {tofu:?}"),
            missing,
        }
    }
}

/// Run `work` inside a counting scope and return its output with the counters it moved.
pub(in crate::core) fn counted<Output>(
    work: impl FnOnce() -> Output,
) -> (Output, crate::frame_stats::FrameStats) {
    let sink = crate::frame_stats::FrameStatsSink::default();
    let output = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        work()
    };
    (output, sink.snapshot())
}

/// Overwrite visible row `row` from column 0 with `text` in default colours.
pub(in crate::core) fn write_row(grid: &mut Grid, row: u16, text: &str) {
    grid.goto(row, 0);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A grid of `cols` columns holding one visible row per entry of `lines`, with no dirt.
pub(in crate::core) fn text_grid(cols: u16, lines: &[&str]) -> Grid {
    let mut grid = Grid::new(cols, lines.len() as u16);
    for (row, line) in lines.iter().enumerate() {
        write_row(&mut grid, row as u16, line);
    }
    grid.clear_dirty();
    grid
}

/// The inputs one frozen oracle instance reads.
pub(in crate::core) struct OracleCell {
    pub(in crate::core) snapped: Vec<f32>,
    pub(in crate::core) cell_size: (f32, f32),
    pub(in crate::core) top_inset: f32,
    pub(in crate::core) row: u16,
    pub(in crate::core) baseline_y_in_cell: f32,
    pub(in crate::core) surface: (f32, f32),
}

/// Frozen, test-local copy of the pre-record status-marker predicate and fit arithmetic. It is
/// deliberately independent of production, so a change to either the predicate or the fit is
/// caught by `record_projection_matches_the_frozen_emission_geometry`.
pub(in crate::core) fn frozen_fit_single_cell_status_marker(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    if !matches!(ch, '\u{23fa}' | '\u{25ef}' | '\u{25cf}')
        || cluster_cells != 1
        || is_wide
        || has_extras
    {
        return natural;
    }
    let (_, _, glyph_w, glyph_h) = natural;
    let (cell_x, cell_y, cell_w, cell_h) = cell;
    if glyph_w <= 0.0 || glyph_h <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return natural;
    }
    let scale = (cell_w / glyph_w).min(cell_h / glyph_h);
    let fitted_w = glyph_w * scale;
    let fitted_h = glyph_h * scale;
    (cell_x + (cell_w - fitted_w) * 0.5, cell_y + (cell_h - fitted_h) * 0.5, fitted_w, fitted_h)
}

/// Frozen copy of the pre-record HarfBuzz placement: offsets move the tile, never resize it.
pub(in crate::core) fn frozen_positioned_shaped_glyph_rect(
    natural: (f32, f32, f32, f32),
    x_offset: f32,
    y_offset: f32,
) -> (f32, f32, f32, f32) {
    (natural.0 + x_offset, natural.1 + y_offset, natural.2, natural.3)
}

/// Frozen copy of the pre-record software block target: integer edges, at least one pixel.
pub(in crate::core) fn frozen_software_block_glyph_target_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> (f32, f32, f32, f32) {
    let left = (left - 0.5).ceil();
    let top = (top - 0.5).ceil();
    let right = (right - 0.5).ceil().max(left + 1.0);
    let bottom = (bottom - 0.5).ceil().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

/// Frozen copy of the pre-record instance flags: colour in x, subpixel coverage in y.
pub(in crate::core) fn frozen_glyph_flags(is_color: bool, is_subpixel: bool) -> [f32; 4] {
    [if is_color { 1.0 } else { 0.0 }, if is_subpixel { 1.0 } else { 0.0 }, 0.0, 0.0]
}

/// The whole instance the pre-record renderer pushed for `rect` from `info` in `rgba`.
pub(in crate::core) fn frozen_instance(
    cell: &OracleCell,
    rect: (f32, f32, f32, f32),
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let (glyph_x, glyph_y, glyph_w, glyph_h) = rect;
    GlyphInstance {
        rect: px_to_ndc(glyph_x, glyph_y, glyph_w, glyph_h, cell.surface.0, cell.surface.1),
        uv: info.uv,
        color: rgba,
        flags: frozen_glyph_flags(info.is_color, info.is_subpixel),
    }
}

/// Frozen copy of the pre-record ASCII emission.
pub(in crate::core) fn oracle_natural(
    cell: &OracleCell,
    col: u16,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell.cell_size.1;
    let inv_s = 1.0_f32;
    let glyph_x = cell_left_px + info.px_offset[0] as f32 * inv_s;
    let glyph_y = cell_top_px + cell.baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
    let glyph_w = info.px_size[0] as f32 * inv_s;
    let glyph_h = info.px_size[1] as f32 * inv_s;
    let rect = sonicterm_render_model::geometry::snap_to_device_pixels(
        (glyph_x, glyph_y, glyph_w, glyph_h),
        1.0,
    );
    frozen_instance(cell, rect, info, rgba)
}

/// Frozen copy of the pre-record fallback and shaped emission.
pub(in crate::core) fn oracle_shaped(
    cell: &OracleCell,
    lead: (u16, char, usize, bool, bool),
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    shape_offset: (f32, f32),
    rgba: [f32; 4],
) -> GlyphInstance {
    let (col, character, cluster_cells, is_wide, has_extras) = lead;
    let (cell_w, cell_h) = cell.cell_size;
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell_h;
    let inv_s = 1.0_f32;
    let glyph_x = cell_left_px + info.px_offset[0] as f32 * inv_s;
    let glyph_y = cell_top_px + cell.baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
    let glyph_w = info.px_size[0] as f32 * inv_s;
    let glyph_h = info.px_size[1] as f32 * inv_s;
    let positioned = frozen_positioned_shaped_glyph_rect(
        (glyph_x, glyph_y, glyph_w, glyph_h),
        shape_offset.0,
        shape_offset.1,
    );
    let cell_right = cell.snapped.get(col as usize + 1).copied().unwrap_or(cell_left_px + cell_w);
    let fitted = frozen_fit_single_cell_status_marker(
        character,
        cluster_cells,
        is_wide,
        has_extras,
        positioned,
        (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h),
    );
    let rect = sonicterm_render_model::geometry::snap_to_device_pixels(fitted, 1.0);
    frozen_instance(cell, rect, info, rgba)
}

/// Frozen copy of the pre-record block emission.
pub(in crate::core) fn oracle_block(
    cell: &OracleCell,
    col: u16,
    span: usize,
    software_presenter: bool,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let cell_h = cell.cell_size.1;
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell_h;
    let end_col = (col as usize + span).min(cell.snapped.len() - 1);
    let cell_right = cell.snapped[end_col];
    let rect = if software_presenter {
        let cell_bottom = cell.top_inset + (f32::from(cell.row) + 1.0) * cell_h;
        frozen_software_block_glyph_target_rect(cell_left_px, cell_top_px, cell_right, cell_bottom)
    } else {
        (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h)
    };
    frozen_instance(cell, rect, info, rgba)
}

/// One instance's bytes, so comparisons are bit-exact rather than f32 equality.
pub(in crate::core) fn instance_bytes(instance: &GlyphInstance) -> Vec<u8> {
    bytemuck::bytes_of(instance).to_vec()
}

/// The glyph atlas and the cold and warm glyph lists of one scrolled frame on the software
/// presenter at a fractional pitch: the warm list replays rows cached at other slots, and both
/// lists are returned with the frame's surface size, for compositor parity tests.
pub(crate) fn warm_and_cold_row_glyphs(
) -> (GlyphAtlas, Vec<GlyphInstance>, Vec<GlyphInstance>, (u32, u32)) {
    let mut grid = Grid::new(8, 4);
    let lines = ["ab=>c", "│─┼█", "→ é x", "plain", "▌▐ AB", "q->r", "last", "zz"];
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let mut rig = GlyphRig::new(true);
    rig.atlas = GlyphAtlas::new(512, 512);
    rig.cell_size = (10.5, 20.3);
    rig.baseline_y_in_cell = 16.2;
    rig.origin = (0.5, 0.1);
    rig.surface = (85.0, 82.0);
    let live = grid.scrollback_len() as u64;
    let scrolled = live - 2;
    let frame = |rig: &mut GlyphRig, top: u64| -> Vec<EmittedRow> {
        rig.begin(&grid);
        let rows = (0..4).map(|slot| rig.emit(&grid, top, slot)).collect();
        rig.cache.commit_slots();
        rows
    };
    frame(&mut rig, scrolled);
    let warm = frame(&mut rig, live);
    assert!(warm.iter().any(|row| row.replayed), "the warm frame replays moved rows");
    rig.cache.invalidate_all();
    let cold = frame(&mut rig, live);
    assert!(cold.iter().all(|row| !row.replayed), "the cold frame shapes every row");
    let flatten = |rows: Vec<EmittedRow>| rows.into_iter().flat_map(|row| row.glyphs).collect();
    (rig.atlas, flatten(cold), flatten(warm), (85, 82))
}

/// The plan fixture of the cache policy tests: one primary pane of `rows` rows by 8 columns at
/// layout and origin y 0, padding 0, 10x20 cells and `vertical_ink_pad`, cursor hidden, no
/// overlay, not degraded.
pub(in crate::core) fn policy_plan(
    rows: u16,
    vertical_ink_pad: f32,
    revision: u64,
    dirty_rows: Vec<usize>,
    row_ink: Vec<Option<PixelRect>>,
    previous: Option<&FrameKey>,
) -> FramePlan {
    let facts = FrameFacts {
        window: WindowIdentity { width: 240, height: 200, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [0.0; 4],
        vertical_ink_pad,
        scrollbar_mode: ScrollbarMode::Never,
        degraded: false,
        tab_bar_top: None,
        status_band: None,
        scale: 1.0,
        previous_recolor: crate::cursor::RecolorRecord::default(),
    };
    let pane = PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 80, h: u32::from(rows) * 20 },
        cols: 8,
        rows,
        scrollback_len: 0,
        viewport_top_abs: None,
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink,
    };
    FramePlan::build(facts, [pane], previous)
}

/// Each slot's committed ink set to its own strip `[20 s, 20 s + 20)`.
pub(in crate::core) fn strip_records(rows: u16) -> Vec<Option<PixelRect>> {
    (0..i32::from(rows)).map(|slot| Some(PixelRect { x: 0, y: 20 * slot, w: 80, h: 20 })).collect()
}

/// Assemble one pass of `plan`'s pane through the production pane seam, after the pass's
/// `begin_frame`, returning the glyphs and the counters the pass moved.
pub(in crate::core) fn assemble_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats) {
    begin_pass(rig, ink, grid, plan);
    emit_pass(rig, ink, grid, plan)
}

/// Start one assembly pass through `begin_glyph_pass`, the helper `assemble_frame` starts every
/// pass with, so the fixture exercises production's pass start rather than a copy of it.
pub(in crate::core) fn begin_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) {
    begin_glyph_pass(&mut rig.cache, ink, plan.panes.iter().map(|planned| (planned, grid.cols)));
}

/// Emit `plan`'s pane rows through the production pane seam into a pass already begun,
/// returning the glyphs and the counters the emission moved.
pub(in crate::core) fn emit_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats) {
    let (glyphs, stats, _slots) = emit_pass_recording(rig, ink, grid, plan);
    (glyphs, stats)
}

/// [`emit_pass`], also returning the slots the pane seam emitted, in emission order, as its
/// recording sink saw them at each `emit_row_glyphs` call.
pub(in crate::core) fn emit_pass_recording(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats, Vec<u16>) {
    let planned = &plan.panes[0];
    let mut emitted_slots = Vec::new();
    let snapped = build_snapped_cell_x(planned.layout.x, rig.cell_size.0, grid.cols);
    let (mut glyphs, mut underlines, mut tofu) = (Vec::new(), Vec::new(), Vec::new());
    let (mut missing, mut spans, mut owners) = (Vec::new(), Vec::new(), Vec::new());
    let ((), stats) = counted(|| {
        assemble_pane_glyph_rows(
            rig.shaping(),
            PaneGlyphRows {
                pane_id: planned.id,
                grid,
                planned,
                origin: (planned.layout.x, planned.layout.y),
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
            },
            GlyphFrame {
                glyph_instances: &mut glyphs,
                underlines: &mut underlines,
                missing_tofu: &mut tofu,
                missing_chars_this_frame: &mut missing,
                row_spans: &mut spans,
            },
            PaneGlyphSinks {
                row_ink: ink,
                ink_surface: plan.surface,
                underline_owners: &mut owners,
                injected_row_glyph: None,
                emitted_slots: Some(&mut emitted_slots),
                row_keys: &mut Vec::new(),
                missing_rows: None,
                fonts: None,
            },
        )
    });
    (glyphs, stats, emitted_slots)
}

/// Settle a pass as presented: commit its staged glyph slot keys and ink records.
pub(in crate::core) fn present_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    plan: &FramePlan,
) {
    rig.cache.commit_slots();
    ink.commit(&plan.drawn_row_counts());
}

/// How one fallback pass changes the glyph atlas while it assembles, as eviction, a reset or a
/// growth can during a real assembly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::core) enum AtlasChange {
    /// The atlas is left alone.
    Unchanged,
    /// The atlas is reset in place, giving it a new content identity.
    Reset,
    /// The atlas doubles, recomputing every UV.
    Growth,
}

/// Which of the last presented frame's ink reaches slot 0, the row a partial plan does not emit:
/// the cursor recolor or the tab-title ink. Either widens the plan's damage onto that row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::core) enum Widening {
    /// The last frame recolored glyphs over slot 0.
    Recolor,
    /// The last frame drew tab-title ink over slot 0.
    TabInk,
}

/// A six-row pane presented in full, then edited at slot 2, for the fallback orchestration tests:
/// the rig (with a growable atlas), its ink table, the edited grid, the presented plan, slot 2's
/// committed key before the edit, and the stage each pass found at its start.
pub(in crate::core) struct FallbackFixture {
    pub(in crate::core) rig: GlyphRig,
    pub(in crate::core) ink: crate::row_ink::RowInkTable,
    pub(in crate::core) grid: Grid,
    pub(in crate::core) warm: FramePlan,
    pub(in crate::core) committed_before: Vec<u64>,
    pub(in crate::core) stages_at_pass_start: Vec<Vec<u64>>,
    pub(in crate::core) committed_after_pass: Vec<Vec<u64>>,
}

impl FallbackFixture {
    pub(in crate::core) fn new() -> Self {
        let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
        let mut rig = GlyphRig::new(false);
        rig.atlas = GlyphAtlas::growable(256, 2048);
        let mut ink = crate::row_ink::RowInkTable::default();
        let warm = policy_plan(6, 0.0, grid.revision(), Vec::new(), Vec::new(), None);
        assemble_pass(&mut rig, &mut ink, &grid, &warm);
        present_pass(&mut rig, &mut ink, &warm);
        write_row(&mut grid, 2, "EDIT");
        let committed_before =
            (0..6).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
        Self {
            rig,
            ink,
            grid,
            warm,
            committed_before,
            stages_at_pass_start: Vec::new(),
            committed_after_pass: Vec::new(),
        }
    }

    /// The edit's plan at the grid's current revision, `Partial` unless `force_full`.
    pub(in crate::core) fn plan(&self, force_full: bool) -> FramePlan {
        let mut plan = policy_plan(
            6,
            0.0,
            self.grid.revision(),
            vec![2],
            strip_records(6),
            Some(&self.warm.key),
        );
        if force_full {
            plan.force_full();
        }
        plan
    }

    /// Every slot's committed glyph key.
    pub(in crate::core) fn committed(&self) -> Vec<u64> {
        (0..6).map(|slot| self.rig.cache.committed_slot(7, slot).unwrap()).collect()
    }

    /// Every slot's staged glyph key.
    pub(in crate::core) fn staged(&self) -> Vec<u64> {
        (0..6).map(|slot| self.rig.cache.staged_slot(7, slot).unwrap()).collect()
    }

    /// One assembly pass through the production pass helpers: `begin_glyph_pass` starts it, the
    /// pane seam emits its rows, `change` is applied to the atlas as assembly could change it,
    /// and `GpuRenderer::finish_assembly_pass` takes the atlas check, the damage widening by the
    /// last frame's `widening` ink over slot 0, the fallback decision and the receipts, exactly
    /// as `assemble_frame` does.
    pub(in crate::core) fn assemble(
        &mut self,
        force_full: bool,
        change: AtlasChange,
        widening: Widening,
    ) -> Result<Assembled> {
        let mut plan = self.plan(force_full);
        let atlas_stamp_at_start = GlyphContentStamp::capture(1, 1, &self.rig.atlas);
        let atlas_evictions_at_start = self.rig.atlas.evictions();
        begin_pass(&mut self.rig, &mut self.ink, &self.grid, &plan);
        self.stages_at_pass_start.push(self.staged());
        let (glyphs, _) = emit_pass(&mut self.rig, &mut self.ink, &self.grid, &plan);
        self.committed_after_pass.push(self.committed());
        match change {
            AtlasChange::Unchanged => {}
            AtlasChange::Reset => self.rig.atlas.reset_in_place(),
            AtlasChange::Growth => {
                let doubled = self.rig.atlas.width() * 2;
                assert!(self.rig.atlas.grow_to(doubled), "the atlas doubles");
            }
        }
        let slot_zero = crate::cursor::RecolorBounds::Rect(strip_records(6)[0].expect("strip"));
        let (previous_recolor, previous_tab_ink) = match widening {
            Widening::Recolor => (
                crate::cursor::RecolorRecord { bounds: slot_zero, hash: 1 },
                crate::cursor::RecolorBounds::Empty,
            ),
            Widening::TabInk => (crate::cursor::RecolorRecord::default(), slot_zero),
        };
        let pass_end = PassEnd {
            atlas_stamp_at_start,
            atlas_stamp_now: GlyphContentStamp::capture(1, 1, &self.rig.atlas),
            atlas_evictions_at_start,
            previous_recolor,
            current_recolor: crate::cursor::RecolorRecord::default(),
            previous_tab_ink,
            current_tab_ink: crate::cursor::RecolorBounds::Empty,
        };
        let panes = [sonicterm_render_model::PaneRender {
            id: 7,
            rect_px: PixelRect { x: 0, y: 0, w: 80, h: 120 },
            grid: &mut self.grid,
            viewport_top_abs: None,
            is_active: true,
            cursor_style: sonicterm_render_model::CursorStyle::default(),
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: Vec::new(),
        }];
        let ended = GpuRenderer::finish_assembly_pass(&mut plan, &panes, pass_end);
        drop(panes);
        let receipts = match ended {
            Ok(receipts) => receipts,
            Err(early_exit) => return Ok(early_exit),
        };
        Ok(Assembled::Layers(Box::new(AssembledLayers {
            surface_width: 240.0,
            surface_height: 200.0,
            subpixel_aa: SubpixelAaMode::Off,
            scratch: {
                // The fixture leases a scratch of its own and fills it as assembly would.
                let home = frame_scratch::ScratchHome::new();
                let mut lease = home.lease();
                lease.get().glyphs = glyphs;
                lease.complete();
                lease
            },
            field_candidates: PresentedFields::default(),
            missing_chars: Vec::new(),
            missing_chrome_chars: Vec::new(),
            gpu_timing: None,
            plan,
            receipts,
            recolor: crate::cursor::RecolorRecord::default(),
            tab_ink: crate::cursor::RecolorBounds::Empty,
        })))
    }
}
