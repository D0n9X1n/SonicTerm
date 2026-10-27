#![cfg(test)]
//! Shared test-only parity fixture state and native custody.

use super::*;

/// Absolute rows the cache tests seed: the slot-5 row of the scrolled-back view (102), the
/// live row 5 it is edited at (105), live row 8 (108), and live row 22 below the view (122).
pub(in crate::core) const SEEDED_ABS_ROWS: [u64; 4] = [102, 105, 108, 122];

/// Plan one frame of a 24-row primary pane with 100 history rows scrolled back three rows,
/// after a baseline frame, with `dirty_live_rows` as the grid's dirty rows.
pub(in crate::core) fn scrolled_back_cache_plan(dirty_live_rows: Vec<usize>) -> FramePlan {
    let frame_facts = || FrameFacts {
        window: WindowIdentity { width: 240, height: 600, ..Default::default() },
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
    };
    let metadata = |revision, dirty_rows| PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 100, h: 484 },
        cols: 8,
        rows: 24,
        scrollback_len: 100,
        viewport_top_abs: Some(97),
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink: Vec::new(),
    };
    let baseline = FramePlan::build(frame_facts(), [metadata(1, Vec::new())], None);
    FramePlan::build(frame_facts(), [metadata(2, dirty_live_rows)], Some(&baseline.key))
}

/// Seed the background-quad cache with every row in `SEEDED_ABS_ROWS`, run the planned pane
/// through the quad invalidation `render_frame` makes, and report which seeded absolute rows it
/// still holds. The glyph cache is keyed by content and drops nothing for dirt.
pub(in crate::core) fn surviving_cached_rows(plan: &FramePlan) -> Vec<u64> {
    let planned = &plan.panes[0];
    let mut quads = crate::row_quad_cache::LineQuadCache::new();
    quads.resize(planned.row_count);
    for row_abs in SEEDED_ABS_ROWS {
        quads.insert(planned.id, row_abs, 1, crate::row_quad_cache::CachedRowQuads::default());
    }
    invalidate_planned_quad_rows(&mut quads, planned);
    SEEDED_ABS_ROWS.into_iter().filter(|&row| quads.get(planned.id, row, 1).is_some()).collect()
}

/// The damage rectangle of one viewport slot of the scrolled-back cache plan.
pub(in crate::core) fn cache_plan_slot_rect(plan: &FramePlan, slot: usize) -> PixelRect {
    let planned = &plan.panes[0];
    dirty_rows_damage_rect_with_ink_pad(
        [slot],
        planned.full_rect,
        planned.layout.x,
        planned.layout.y,
        planned.cols,
        10.0,
        20.0,
        0.0,
        240,
        600,
    )
    .expect("the slot has pixels")
}

/// The Rec Mono St.Helens test fixture stack, built through the production font-directory constructor.
pub(in crate::core) fn rec_mono_font_stack() -> sonicterm_engine::FontStack {
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sonicterm-font/test-fonts/rec-mono");
    sonicterm_engine::FontStack::try_new_full_with_weight_and_font_dirs(
        "Rec Mono St.Helens",
        14.0,
        72,
        1.0,
        &[fonts],
    )
    .expect("the tracked Rec Mono fixture build a stack")
}

/// A `cols`x4 grid with 8 rows of history: per-row foreground and background colours,
/// underlined cells, and non-ASCII text, so glyph position, colour and decoration all vary.
pub(in crate::core) fn parity_grid(cols: u16) -> Grid {
    let mut grid = Grid::new(cols, 4);
    let text = ['a', 'B', '3', 'é', '→', 'x', 'Q', '7'];
    for row in 0..12u8 {
        if row > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for col in 0..cols as u8 {
            let foreground = Color::Rgb(250 - row * 10, 40 + col * 20, 120);
            let background = Color::Rgb(row * 20, col * 25, 90);
            let flags =
                if (row + col) % 3 == 0 { CellFlags::UNDERLINE } else { CellFlags::empty() };
            let character = text[(row as usize + col as usize) % text.len()];
            grid.put_char(character, foreground, background, flags);
        }
    }
    grid.clear_dirty();
    grid
}

/// One pointer-visible frame state: viewport top, pane origin, selection, focus, and the scale
/// the cell geometry is drawn at.
#[derive(Clone, Copy)]
pub(in crate::core) struct ParityFrame {
    pub(in crate::core) view_top_abs: u64,
    pub(in crate::core) origin_x: f32,
    pub(in crate::core) selection: Option<(u64, u64)>,
    pub(in crate::core) focused: bool,
    pub(in crate::core) scale: f32,
}

/// One frame's drawn output: quads, glyph instances, and the decorations kept as records.
#[derive(Debug, PartialEq)]
pub(in crate::core) struct RenderedFrame {
    pub(in crate::core) quads: Vec<u8>,
    pub(in crate::core) glyphs: Vec<u8>,
    pub(in crate::core) decorations: String,
}

/// The caches a renderer keeps between frames.
pub(in crate::core) struct ParityCaches {
    pub(in crate::core) glyph_rows: sonicterm_text::row_glyph_cache::RowGlyphCache,
    pub(in crate::core) background_rows: crate::row_quad_cache::LineQuadCache,
}

impl ParityCaches {
    pub(in crate::core) fn new() -> Self {
        Self {
            glyph_rows: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            background_rows: crate::row_quad_cache::LineQuadCache::new(),
        }
    }
}

/// Render one frame through the renderer's own steps: per-row backgrounds and glyphs (replayed
/// or shaped), the focus flash, the selection highlight, and the cursor recolour, in the
/// renderer's order. Returns the output and how many glyph and background rows replayed.
pub(in crate::core) fn render_parity_frame(
    stack: &sonicterm_engine::FontStack,
    atlas: &mut GlyphAtlas,
    caches: &mut ParityCaches,
    grid: &Grid,
    frame: ParityFrame,
) -> (RenderedFrame, usize, usize) {
    let theme = Theme::default();
    let (cell_w, cell_h) = (10.0 * frame.scale, 20.0 * frame.scale);
    let baseline_y_in_cell = 16.0 * frame.scale;
    let surface = (400.0, 200.0);
    let pane_size = (f32::from(grid.cols) * cell_w, f32::from(grid.rows) * cell_h);
    let snapped = build_snapped_cell_x(frame.origin_x, cell_w, grid.cols);
    let selection = frame.selection.map(|(start, end)| selection_for_rows(start, end));
    let sel_bbox = selection.map(|sel| {
        let (lo, hi) = sel.normalized();
        (lo.0, lo.1, hi.0, hi.1)
    });
    caches.glyph_rows.begin_frame(&[(7, grid.rows, grid.cols)]);
    caches.background_rows.resize(grid.rows);
    let mut raster = stack.clone();
    let (mut quads, mut glyph_instances, mut underlines) = (Vec::new(), Vec::new(), Vec::new());
    let (mut missing_tofu, mut missing_chars, mut row_spans) = (Vec::new(), Vec::new(), Vec::new());
    let (mut glyph_replays, mut background_replays) = (0, 0);
    let background_geometry = RowBackgroundGeometry {
        origin: (frame.origin_x, 0.0),
        pane_size,
        cell_size: (cell_w, cell_h),
        surface,
        max_cols: grid.cols,
    };
    let mut shaping = GlyphShaping {
        atlas: &mut *atlas,
        row_cache: &mut caches.glyph_rows,
        font_stack: Some(stack),
        wt_raster: Some(&mut raster),
        style_rev: 0,
        theme: &theme,
        fg_default: ChromeColor::rgb(230, 230, 230),
        raster_px: 14.0,
        cell_size: (cell_w, cell_h),
        surface,
        baseline_y_in_cell,
        hovered_url_accent: [0.0; 4],
        software_presenter: false,
    };
    // Pin phase, as the production pass runs it: every row's key before any admission.
    let keys: Vec<u64> = (0..grid.rows)
        .map(|slot| emitted_row_key(&shaping, grid, frame.view_top_abs, slot, None))
        .collect();
    shaping.row_cache.pin(7, &keys);
    for slot in 0..grid.rows {
        let row = grid.row_at_abs(frame.view_top_abs + u64::from(slot)).expect("retained row");
        background_replays += usize::from(emit_row_background(
            &mut caches.background_rows,
            RowBackgroundRow { pane_id: 7, grid, view_top_abs: frame.view_top_abs, slot },
            row.iter(),
            (0, &theme, sel_bbox),
            &background_geometry,
            &snapped,
            &mut quads,
        ));
        glyph_replays += usize::from(emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id: 7,
                grid,
                view_top_abs: frame.view_top_abs,
                slot,
                origin: (frame.origin_x, 0.0),
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
                key: keys[usize::from(slot)],
            },
            GlyphFrame {
                glyph_instances: &mut glyph_instances,
                underlines: &mut underlines,
                missing_tofu: &mut missing_tofu,
                missing_chars_this_frame: &mut missing_chars,
                row_spans: &mut row_spans,
            },
        ));
        shaping.row_cache.stage_slot(7, slot, keys[usize::from(slot)]);
    }
    // Every parity frame is presented, so its slot keys commit.
    caches.glyph_rows.commit_slots();
    if frame.focused {
        // Focus chrome: the pane flash.
        quads.push(focus_flash_quad(
            [0.1, 0.1, 0.1, 1.0],
            (frame.origin_x, 0.0, pane_size.0, pane_size.1),
            0.5,
            surface,
        ));
    }
    if let Some(sel) = selection.as_ref() {
        push_selection_quads(
            &mut quads,
            sel,
            &SelectionGeometry {
                view_top_abs: frame.view_top_abs,
                grid_size: (grid.rows, grid.cols),
                origin: (frame.origin_x, 0.0),
                cell_size: (cell_w, cell_h),
                clip: (frame.origin_x, 0.0, pane_size.0, pane_size.1),
                surface,
            },
            &snapped,
            [0.2, 0.4, 0.8, 0.5],
        );
    }
    if frame.focused {
        // Focus chrome: the cursor recolours the glyph under it, after the rows were cached.
        let _ = recolor_cursor_glyphs_in(
            &mut glyph_instances,
            &row_spans,
            snapped[2],
            cell_h,
            cell_w,
            cell_h,
            surface.0,
            surface.1,
            [0.0, 0.0, 0.0, 1.0],
        );
    }
    let decorations = format!("{underlines:?} {missing_tofu:?} {missing_chars:?}");
    let rendered = RenderedFrame {
        quads: bytemuck::cast_slice(&quads).to_vec(),
        glyphs: bytemuck::cast_slice(&glyph_instances).to_vec(),
        decorations,
    };
    (rendered, glyph_replays, background_replays)
}

/// The frame-texture inventory over the given sources: `create_frame_texture(` appears only in
/// `build_frame_texture`, and construction, resize, recovery prepare and the degrade switch all use it.
pub(in crate::core) fn check_frame_texture_inventory(
    core: &str,
    rebind: &str,
    present: &str,
    atlas_lifecycle: &str,
) {
    // A CRLF checkout is read as LF, so the function-end delimiters match either way.
    let [core, rebind, present, atlas_lifecycle] =
        [core, rebind, present, atlas_lifecycle].map(|source| source.replace("\r\n", "\n"));
    let (core, rebind, present, atlas_lifecycle) =
        (core.as_str(), rebind.as_str(), present.as_str(), atlas_lifecycle.as_str());
    let sources = [
        ("core.rs", core),
        ("rebind.rs", rebind),
        ("present.rs", present),
        ("atlas_lifecycle.rs", atlas_lifecycle),
    ];
    let calls: usize =
        sources.iter().map(|(_, text)| text.matches("create_frame_texture(").count()).sum();
    // The definition and the one call inside `build_frame_texture`.
    assert_eq!(calls, 2, "create_frame_texture( outside build_frame_texture");
    let helper = core.split_once("fn build_frame_texture(").expect("helper").1;
    let helper = helper.split_once("\n}\n").unwrap().0;
    assert!(helper.contains("create_frame_texture("));
    assert!(helper.contains("frame_texture_extent("));
    // rustfmt may wrap these calls, so the checks compare text with whitespace removed.
    let squeeze = |text: &str| text.split_whitespace().collect::<String>();
    let construction = core.split_once("InitTiming::begin(\"frame_texture\")").unwrap().1;
    let construction = construction.split_once("InitTiming::finish").unwrap().0;
    assert!(squeeze(construction).contains("build_frame_texture(&device,software_presenter"));
    let resize = core.split_once("enter_gpu_work(\"try_resize\")").unwrap().1;
    assert!(resize
        .split_once("self.last_frame_key = None")
        .unwrap()
        .0
        .contains("self.rebuild_frame_texture()"));
    let degrade = core.split_once("pub fn set_software_render_degrade(").unwrap().1;
    let degrade = degrade.split_once("fn uses_windows_software_presenter(").unwrap().0;
    let branch =
        degrade.split_once("if used_software_presenter != uses_software_presenter {").unwrap().1;
    assert!(
        branch.contains("self.rebuild_frame_texture()"),
        "the degrade switch resizes the texture"
    );
    let rebuild = core.split_once("fn rebuild_frame_texture(").expect("rebuild helper").1;
    let rebuild = rebuild.split_once("\n    }\n").unwrap().0;
    assert!(
        rebuild.find("enter_gpu_work(").unwrap() < rebuild.find("build_frame_texture(").unwrap(),
        "a stopped device refuses the rebuild"
    );
    let prepare = rebind.split_once("fn prepare_rebind").unwrap().1;
    let software = prepare.find("let software_presenter =").unwrap();
    let build = prepare.find("build_frame_texture(").unwrap();
    assert!(software < build, "recovery decides the presenter before sizing the texture");
    assert!(squeeze(&prepare[build..])
        .starts_with("build_frame_texture(&context.device,software_presenter"));
    let wgpu = present.split_once("fn present_wgpu_frame(").unwrap().1;
    assert!(
        squeeze(wgpu)
            .contains("debug_assert_eq!(self.frame_texture_extent(),frame_texture_extent("),
        "the GPU presenter checks the extent"
    );
}

/// A rasterizer that resolves nothing, so the atlas caches a missing sentinel.
pub(in crate::core) struct NoGlyphs;

impl sonicterm_text::glyph_atlas::Rasterizer for NoGlyphs {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        None
    }
}

/// A source that owns its grids and records when it is lent and when it is dropped.
pub(in crate::core) struct OwningSource {
    pub(in crate::core) grids: Vec<Grid>,
    pub(in crate::core) log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
}

impl sonicterm_render_model::FrameSource for OwningSource {
    fn lend<R>(
        mut self,
        assemble: impl for<'slice, 'grid> FnOnce(
            &'slice mut [sonicterm_render_model::PaneRender<'grid>],
        ) -> R,
    ) -> R {
        self.log.borrow_mut().push("lend");
        let mut panes: Vec<_> = self
            .grids
            .iter_mut()
            .enumerate()
            .map(|(index, grid)| sonicterm_render_model::PaneRender {
                id: index as u64 + 1,
                rect_px: PixelRect { x: 0, y: 0, w: 80, h: 40 },
                grid,
                viewport_top_abs: None,
                is_active: index == 0,
                cursor_style: sonicterm_render_model::CursorStyle::default(),
                is_broadcast_participant: false,
                scrollbar_alpha: 0.0,
                inline_images: Vec::new(),
            })
            .collect();
        assemble(&mut panes)
    }
}

impl Drop for OwningSource {
    // Lifecycle: dropping the source releases what it owns; the log records when.
    fn drop(&mut self) {
        self.log.borrow_mut().push("drop");
    }
}
