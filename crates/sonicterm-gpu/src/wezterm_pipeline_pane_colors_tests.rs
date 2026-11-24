//! A split where one pane's program set its colors: the software compositor and the wgpu pipeline
//! both draw that pane's blank area, inverse cell, palette cell, cursor, selection highlight,
//! selected glyphs and cursor text in the program colors, while the sibling pane keeps the theme.

use super::wezterm_pipeline_tests::headless_device_and_queue;
use super::*;
use crate::core::{
    build_snapped_cell_x, emit_cell_bg_quads_for_row, field_selection_colors, push_selection_quads,
    recolor_copy_mode_glyphs, recolor_selected_glyphs, GpuRenderer, PaneColors, SelectionGeometry,
};
use crate::cursor::{recolor_cursor_glyphs_in, RowGlyphSpan};
use crate::quad::px_to_ndc;
use sonicterm_render_model::boundary::cfg::theme::{Hex, Theme};
use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
use sonicterm_render_model::boundary::ui::copy_mode::CopyModeState;
use sonicterm_render_model::boundary::ui::selection::Selection;
use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile};
use sonicterm_types::{ColorOverrides, DynamicColor, GlyphKey};

/// Surface width: two 40 px panes of four 10 px cells each.
const WIDTH_PX: u32 = 80;
/// Surface height: one 20 px row.
const HEIGHT_PX: u32 = 20;
/// Row stride of the wgpu readback, padded to the copy alignment.
const READBACK_STRIDE: u32 = 512;
/// An sRGB color, 8 bits per channel.
type Srgb = (u8, u8, u8);

/// The program colors of the left pane.
fn program_colors() -> ColorOverrides {
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::Foreground, Some((1, 2, 3)));
    overrides.set_dynamic(DynamicColor::Background, Some((40, 50, 60)));
    overrides.set_dynamic(DynamicColor::Cursor, Some((7, 8, 9)));
    overrides.set_palette(1, Some((100, 110, 120)));
    overrides
}

/// One row: an inverse default cell, a blank cell, a cell on palette background 1, and the cell
/// the cursor sits on.
fn split_row() -> Grid {
    let mut grid = Grid::new(4, 1);
    grid.put_char(' ', Color::Default, Color::Default, CellFlags::INVERSE);
    grid.put_char(' ', Color::Default, Color::Default, CellFlags::empty());
    grid.put_char(' ', Color::Default, Color::Indexed(1), CellFlags::empty());
    grid
}

/// The quads both panes contribute, through the production pane fill, cell background and
/// cursor resolvers: the left pane with `overrides`, the right pane with the theme alone.
fn split_quads(theme: &Theme, overrides: &ColorOverrides, grid: &Grid) -> Vec<QuadInstance> {
    let theme_cursor = crate::color::hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0);
    let (surface_w, surface_h) = (WIDTH_PX as f32, HEIGHT_PX as f32);
    let mut quads = Vec::new();
    for (pad, colors) in
        [(0.0, PaneColors::new(theme, Some(overrides))), (40.0, PaneColors::new(theme, None))]
    {
        if let Some(fill) = colors.background_fill(1.0) {
            quads.push(QuadInstance::sharp(
                px_to_ndc(pad, 0.0, 40.0, 20.0, surface_w, surface_h),
                fill,
            ));
        }
        let snapped = build_snapped_cell_x(pad, 10.0, 4);
        emit_cell_bg_quads_for_row(
            grid, 0, colors, pad, 0.0, 10.0, 20.0, surface_w, surface_h, 4, 0, &mut quads, &snapped,
        );
        quads.push(QuadInstance::sharp(
            px_to_ndc(pad + 30.0, 0.0, 10.0, 20.0, surface_w, surface_h),
            colors.cursor_rgba(theme_cursor),
        ));
    }
    quads
}

/// The sRGB color of a theme hex value.
fn theme_rgb(hex: &Hex) -> Srgb {
    hex.rgb().expect("default theme colors are hex")
}

/// The sRGB color expected at the centre of each of the eight cells, left pane first.
fn expected_cells(theme: &Theme) -> [Srgb; 8] {
    [
        (1, 2, 3),
        (40, 50, 60),
        (100, 110, 120),
        (7, 8, 9),
        theme_rgb(&theme.colors.foreground),
        theme_rgb(&theme.colors.background),
        theme_rgb(&theme.colors.ansi.red),
        theme_rgb(&theme.colors.cursor),
    ]
}

/// Assert each cell's centre pixel, read as BGRA by `pixel_at`, within `tolerance` per channel.
fn assert_cells(
    path: &str,
    expected: [Srgb; 8],
    tolerance: u8,
    pixel_at: impl Fn(u32, u32) -> [u8; 4],
) {
    for (cell, (red, green, blue)) in expected.into_iter().enumerate() {
        let [blue_seen, green_seen, red_seen, _] = pixel_at(cell as u32 * 10 + 5, HEIGHT_PX / 2);
        for (seen, wanted) in [(red_seen, red), (green_seen, green), (blue_seen, blue)] {
            assert!(
                seen.abs_diff(wanted) <= tolerance,
                "{path}: cell {cell} is {:?}, expected {:?}",
                (red_seen, green_seen, blue_seen),
                (red, green, blue)
            );
        }
    }
}

/// Composite `quads` and then `glyphs` from `atlas` over `clear` on the software (GDI) compositor
/// and return its BGRA pixels, row-major.
fn software_pixels(
    clear: [f32; 4],
    atlas: &GlyphAtlas,
    quads: &[QuadInstance],
    glyphs: &[GlyphInstance],
) -> Vec<[u8; 4]> {
    software_pixels_with_overlay(clear, atlas, (quads, glyphs), (&[], &[]))
}

/// [`software_pixels`] with an overlay layer (a field's quads and glyphs) drawn above the
/// terminal layer, as palette and search fields are.
fn software_pixels_with_overlay(
    clear: [f32; 4],
    atlas: &GlyphAtlas,
    (quads, glyphs): (&[QuadInstance], &[GlyphInstance]),
    (overlay_quads, overlay_glyphs): (&[QuadInstance], &[GlyphInstance]),
) -> Vec<[u8; 4]> {
    let mut frame =
        crate::software_frame::SoftwareFrame::new(WIDTH_PX, HEIGHT_PX, clear).expect("frame");
    frame.draw_layers_with_subpixel_aa(
        atlas,
        atlas,
        SubpixelAaMode::Off,
        quads,
        &[],
        glyphs,
        overlay_quads,
        overlay_glyphs,
    );
    (0..HEIGHT_PX)
        .flat_map(|y| (0..WIDTH_PX).map(move |x| (x, y)))
        .map(|(x, y)| frame.pixel_bgra_at(x, y).expect("in bounds"))
        .collect()
}

/// Draw `quads` and then `glyphs` from `atlas` over `clear` through the wgpu pipeline, on an sRGB
/// target as the surface is, and return its BGRA pixels, row-major.
fn wgpu_pixels(
    clear: [f32; 4],
    atlas: &mut GlyphAtlas,
    quads: &[QuadInstance],
    glyphs: &[GlyphInstance],
) -> Vec<[u8; 4]> {
    wgpu_pixels_with_overlay(clear, atlas, (quads, glyphs), (&[], &[]))
}

/// [`wgpu_pixels`] with an overlay layer drawn above the terminal layer.
fn wgpu_pixels_with_overlay(
    clear: [f32; 4],
    atlas: &mut GlyphAtlas,
    (quads, glyphs): (&[QuadInstance], &[GlyphInstance]),
    (overlay_quads, overlay_glyphs): (&[QuadInstance], &[GlyphInstance]),
) -> Vec<[u8; 4]> {
    let (device, queue) = headless_device_and_queue();
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline =
        WeztermPipeline::new(&device, format, (quads.len() + overlay_quads.len()).max(1) as u64);
    let image_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        atlas,
        pipeline.image_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Image,
    );
    let mut glyph_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        atlas,
        pipeline.glyph_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Glyph,
    );
    glyph_upload.sync(&queue, atlas);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pane colors split target"),
        size: wgpu::Extent3d { width: WIDTH_PX, height: HEIGHT_PX, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pane colors split readback"),
        size: u64::from(READBACK_STRIDE) * u64::from(HEIGHT_PX),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pane colors split pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(clear[0]),
                        g: f64::from(clear[1]),
                        b: f64::from(clear[2]),
                        a: f64::from(clear[3]),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipeline.draw_frame(
            &device,
            &queue,
            &mut pass,
            image_upload.image_bind_group(),
            glyph_upload.glyph_bind_group(),
            WIDTH_PX as f32,
            HEIGHT_PX as f32,
            SubpixelAaMode::Off,
            None,
            &[],
            quads,
            &[],
            glyphs,
            overlay_quads,
            overlay_glyphs,
        );
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(READBACK_STRIDE),
                rows_per_image: Some(HEIGHT_PX),
            },
        },
        wgpu::Extent3d { width: WIDTH_PX, height: HEIGHT_PX, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll split readback");
    let mapped = slice.get_mapped_range().expect("mapped split readback");
    (0..HEIGHT_PX)
        .flat_map(|y| (0..WIDTH_PX).map(move |x| (x, y)))
        .map(|(x, y)| {
            let offset = (y * READBACK_STRIDE + x * 4) as usize;
            [mapped[offset], mapped[offset + 1], mapped[offset + 2], mapped[offset + 3]]
        })
        .collect()
}

/// The pixel at `(x, y)` of row-major `pixels`.
fn pixel(pixels: &[[u8; 4]]) -> impl Fn(u32, u32) -> [u8; 4] + '_ {
    move |x, y| pixels[(y * WIDTH_PX + x) as usize]
}

/// The software (GDI) compositor draws the program-colored pane in its program colors and its
/// sibling in the theme.
#[test]
fn software_split_keeps_each_pane_in_its_own_colors() {
    let theme = Theme::default();
    let overrides = program_colors();
    let grid = split_row();
    let quads = split_quads(&theme, &overrides, &grid);
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let atlas = GlyphAtlas::new(1, 1);
    let pixels = software_pixels(clear, &atlas, &quads, &[]);
    assert_cells("software", expected_cells(&theme), 1, pixel(&pixels));
}

/// The wgpu pipeline, on an sRGB target as the surface is, draws the same split the same way.
#[test]
fn wgpu_split_keeps_each_pane_in_its_own_colors() {
    let theme = Theme::default();
    let overrides = program_colors();
    let grid = split_row();
    let quads = split_quads(&theme, &overrides, &grid);
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let mut atlas = GlyphAtlas::new(1, 1);
    let pixels = wgpu_pixels(clear, &mut atlas, &quads, &[]);
    assert_cells("wgpu", expected_cells(&theme), 1, pixel(&pixels));
}

/// Which pane is active, and so holds the selection and the block cursor.
#[derive(Clone, Copy, PartialEq)]
enum ActivePane {
    Left,
    Right,
}

/// One selection frame: the quads and glyphs of two panes whose row holds a text glyph, a blank
/// cell, a color glyph and a text glyph under the block cursor. The active pane's selection runs
/// from its first cell past its last column, so only the clip keeps it out of the sibling; the
/// left pane carries `left_overrides`. Built with the production selection and recolor helpers in
/// the order assembly runs them.
fn selection_frame(
    theme: &Theme,
    left_overrides: Option<&ColorOverrides>,
    active: ActivePane,
) -> (GlyphAtlas, Vec<QuadInstance>, Vec<GlyphInstance>) {
    let (surface_w, surface_h) = (WIDTH_PX as f32, HEIGHT_PX as f32);
    let mut atlas = GlyphAtlas::new(64, 64);
    let tile = |is_color: bool, bytes_per_pixel: usize| RasterTile {
        width: 10,
        height: 20,
        offset_x: 0,
        offset_y: 0,
        advance: 10.0,
        coverage: vec![255; 10 * 20 * bytes_per_pixel],
        is_color,
        is_subpixel: false,
    };
    let text = atlas
        .get_or_insert_lazy_without_eviction(GlyphKey::new('M', false, false), 10, 20, || {
            tile(false, 1)
        })
        .expect("text tile");
    let emoji = atlas
        .get_or_insert_lazy_without_eviction(GlyphKey::new('E', false, false), 10, 20, || {
            tile(true, 4)
        })
        .expect("color tile");
    let foreground =
        crate::color::hex_to_premultiplied_rgba(theme.colors.foreground.0.as_str(), 1.0);
    let mut glyphs = Vec::new();
    for pad in [0.0, 40.0] {
        for (column, info, is_color) in [(0.0, text, false), (2.0, emoji, true), (3.0, text, false)]
        {
            glyphs.push(GlyphInstance {
                rect: px_to_ndc(pad + column * 10.0, 0.0, 10.0, 20.0, surface_w, surface_h),
                uv: info.uv,
                color: foreground,
                flags: [f32::from(u8::from(is_color)), 0.0, 0.0, 0.0],
            });
        }
    }
    // Each pane emitted one row of three glyphs; the selection recolor reads only the active
    // pane's row, and the cursor recolor reads every row.
    let mut row_spans = [
        RowGlyphSpan::new(&glyphs, 0..3, surface_w, surface_h),
        RowGlyphSpan::new(&glyphs, 3..6, surface_w, surface_h),
    ];
    let (origin, overrides, pane) = match active {
        ActivePane::Left => (0.0, left_overrides, 0..1),
        ActivePane::Right => (40.0, None, 1..2),
    };
    let colors = PaneColors::new(theme, overrides);
    let theme_highlight =
        crate::color::hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
    let theme_cursor_text =
        crate::color::hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0);
    let mut selection = Selection::new(0, 0);
    selection.extend(0, 7);
    let geometry = SelectionGeometry {
        view_top_abs: 0,
        grid_size: (1, 4),
        origin: (origin, 0.0),
        cell_size: (10.0, 20.0),
        clip: (origin, 0.0, 40.0, 20.0),
        surface: (surface_w, surface_h),
    };
    let mut quads = Vec::new();
    push_selection_quads(
        &mut quads,
        &selection,
        &geometry,
        &[],
        colors.selection_rgba(theme_highlight),
    );
    if let Some(selected) = colors.selection_foreground_rgba() {
        recolor_selected_glyphs(
            &mut glyphs,
            &mut row_spans,
            pane,
            &selection,
            &geometry,
            &[],
            selected,
        );
    }
    let _ = recolor_cursor_glyphs_in(
        &mut glyphs,
        &row_spans,
        origin + 30.0,
        0.0,
        10.0,
        20.0,
        surface_w,
        surface_h,
        colors.cursor_text_rgba(theme_cursor_text),
    );
    (atlas, quads, glyphs)
}

/// `selection_rgb` drawn at the theme highlight's opacity of one half over `background_rgb`,
/// blended in linear light as both compositors do.
fn highlight_over(selection_rgb: Srgb, background_rgb: Srgb) -> Srgb {
    let lut = crate::color::srgb_u8_to_linear_lut();
    let blend = |selection: u8, background: u8| {
        crate::software_frame::linear_channel_to_srgb_u8(
            0.5 * lut[usize::from(selection)] + 0.5 * lut[usize::from(background)],
        )
    };
    (
        blend(selection_rgb.0, background_rgb.0),
        blend(selection_rgb.1, background_rgb.1),
        blend(selection_rgb.2, background_rgb.2),
    )
}

/// The three selection cases and the eight cells each must draw: with every selection and
/// cursor-text color set on the active left pane; with none set, so existing themes look
/// unchanged; with only the selection background set, so selected glyphs keep their own color;
/// and with the theme-colored right pane active beside the overridden left pane.
fn selection_cases(theme: &Theme) -> Vec<(&'static str, ColorOverrides, ActivePane, [Srgb; 8])> {
    let foreground = theme_rgb(&theme.colors.foreground);
    let background = theme_rgb(&theme.colors.background);
    let theme_highlight = highlight_over(theme_rgb(&theme.colors.selection_bg), background);
    let cursor_text = theme_rgb(&theme.colors.cursor_text);
    let white = (255, 255, 255);
    let mut every = ColorOverrides::default();
    every.set_dynamic(DynamicColor::SelectionBackground, Some((200, 0, 0)));
    every.set_dynamic(DynamicColor::SelectionForeground, Some((0, 200, 0)));
    every.set_dynamic(DynamicColor::CursorText, Some((0, 0, 200)));
    let mut background_only = ColorOverrides::default();
    background_only.set_dynamic(DynamicColor::SelectionBackground, Some((200, 0, 0)));
    let program_highlight = highlight_over((200, 0, 0), background);
    let sibling = [foreground, background, white, foreground];
    let join = |left: [Srgb; 4], right: [Srgb; 4]| {
        [left[0], left[1], left[2], left[3], right[0], right[1], right[2], right[3]]
    };
    vec![
        (
            "every color set",
            every.clone(),
            ActivePane::Left,
            join([(0, 200, 0), program_highlight, white, (0, 0, 200)], sibling),
        ),
        (
            "no color set",
            ColorOverrides::default(),
            ActivePane::Left,
            join([foreground, theme_highlight, white, cursor_text], sibling),
        ),
        (
            "selection background only",
            background_only,
            ActivePane::Left,
            join([foreground, program_highlight, white, cursor_text], sibling),
        ),
        (
            "theme pane active",
            every,
            ActivePane::Right,
            join(sibling, [foreground, theme_highlight, white, cursor_text]),
        ),
    ]
}

/// The software compositor draws the active pane's selection in its program colors, clipped to
/// that pane: the highlight at the theme opacity, selected glyphs in the selection foreground
/// with color glyphs unchanged, the block cursor's glyph in the cursor text over the selection
/// foreground, and theme colors everywhere when nothing is set or another pane is active.
#[test]
fn software_selection_follows_the_active_pane_colors() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    for (case, overrides, active, expected) in selection_cases(&theme) {
        let (atlas, quads, glyphs) = selection_frame(&theme, Some(&overrides), active);
        let pixels = software_pixels(clear, &atlas, &quads, &glyphs);
        assert_cells(&format!("software, {case}"), expected, 2, pixel(&pixels));
    }
}

/// The wgpu pipeline draws every selection case as the software compositor does.
#[test]
fn wgpu_selection_follows_the_active_pane_colors() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    for (case, overrides, active, expected) in selection_cases(&theme) {
        let (mut atlas, quads, glyphs) = selection_frame(&theme, Some(&overrides), active);
        let pixels = wgpu_pixels(clear, &mut atlas, &quads, &glyphs);
        assert_cells(&format!("wgpu, {case}"), expected, 2, pixel(&pixels));
    }
}

/// One glyph of a cluster scene: its left edge and width in surface pixels, its pane, its atlas
/// key, and whether its tile is striped (every third column uncovered) so a split that loses its
/// atlas coordinates moves the stripes.
struct ClusterGlyph {
    left_px: f32,
    width_px: u32,
    pane: ActivePane,
    key: char,
    striped: bool,
}

/// The program selection background and foreground of the active pane in every cluster scene.
const CLUSTER_SELECTION_BG: Srgb = (200, 0, 0);
/// The selected glyph color of every cluster scene.
const CLUSTER_SELECTION_FG: Srgb = (0, 200, 0);

/// One cluster scene: `glyphs`, left pane first, and a selection of `selected_cols` on row 0 of the
/// `active` pane, whose program set [`CLUSTER_SELECTION_BG`] and [`CLUSTER_SELECTION_FG`]. The
/// recolor sees only the active pane's glyph rows, as assembly passes them.
fn cluster_frame(
    theme: &Theme,
    glyphs: &[ClusterGlyph],
    active: ActivePane,
    selected_cols: (u16, u16),
) -> (GlyphAtlas, Vec<QuadInstance>, Vec<GlyphInstance>) {
    let (surface_w, surface_h) = (WIDTH_PX as f32, HEIGHT_PX as f32);
    let mut atlas = GlyphAtlas::new(128, 64);
    let foreground =
        crate::color::hex_to_premultiplied_rgba(theme.colors.foreground.0.as_str(), 1.0);
    let mut instances = Vec::new();
    for glyph in glyphs {
        let (width, striped) = (glyph.width_px, glyph.striped);
        let info = atlas
            .get_or_insert_lazy_without_eviction(
                GlyphKey::new(glyph.key, false, false),
                width,
                20,
                || RasterTile {
                    width,
                    height: 20,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage: (0..20 * width)
                        .map(
                            |index| {
                                if striped && (index % width).is_multiple_of(3) {
                                    0
                                } else {
                                    255
                                }
                            },
                        )
                        .collect(),
                    is_color: false,
                    is_subpixel: false,
                },
            )
            .expect("cluster tile");
        instances.push(GlyphInstance {
            rect: px_to_ndc(glyph.left_px, 0.0, width as f32, 20.0, surface_w, surface_h),
            uv: info.uv,
            color: foreground,
            flags: [0.0; 4],
        });
    }
    let left_count = glyphs.iter().filter(|glyph| glyph.pane == ActivePane::Left).count();
    let (origin, pane_range) = match active {
        ActivePane::Left => (0.0, 0..left_count),
        ActivePane::Right => (40.0, left_count..instances.len()),
    };
    let mut pane_rows = [RowGlyphSpan::new(&instances, pane_range, surface_w, surface_h)];
    let mut overrides = ColorOverrides::default();
    let (red, green, blue) = CLUSTER_SELECTION_BG;
    overrides.set_dynamic(DynamicColor::SelectionBackground, Some((red, green, blue)));
    let (red, green, blue) = CLUSTER_SELECTION_FG;
    overrides.set_dynamic(DynamicColor::SelectionForeground, Some((red, green, blue)));
    let colors = PaneColors::new(theme, Some(&overrides));
    let theme_highlight =
        crate::color::hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
    let mut selection = Selection::new(0, selected_cols.0);
    selection.extend(0, selected_cols.1);
    let geometry = SelectionGeometry {
        view_top_abs: 0,
        grid_size: (1, 4),
        origin: (origin, 0.0),
        cell_size: (10.0, 20.0),
        clip: (origin, 0.0, 40.0, 20.0),
        surface: (surface_w, surface_h),
    };
    let mut quads = Vec::new();
    push_selection_quads(
        &mut quads,
        &selection,
        &geometry,
        &[],
        colors.selection_rgba(theme_highlight),
    );
    let selected = colors.selection_foreground_rgba().expect("the program set one");
    recolor_selected_glyphs(
        &mut instances,
        &mut pane_rows,
        0..1,
        &selection,
        &geometry,
        &[],
        selected,
    );
    (atlas, quads, instances)
}

/// One cluster scene: its name, glyphs, active pane, selected columns, and the `(pixel column,
/// color)` pairs of row 10 it must draw.
type ClusterCase = (&'static str, Vec<ClusterGlyph>, ActivePane, (u16, u16), Vec<(u32, Srgb)>);

/// The cluster scenes and, per scene, the pixel columns of row 10 to check with the color each
/// must draw:
/// - a two-cell glyph with a bearing (a ligature) whose second cell alone is selected, beside a
///   wide glyph whose lead cell alone is selected: only the selected cells' ink turns green, and
///   the ligature's stripes stay where its atlas coordinates put them on both sides of the cut;
/// - a neighbouring pane's glyph overhanging into the active pane's selected cells: its ink there
///   keeps the foreground, while the active pane's own glyph in that cell turns green.
fn cluster_cases(theme: &Theme) -> Vec<ClusterCase> {
    let foreground = theme_rgb(&theme.colors.foreground);
    let background = theme_rgb(&theme.colors.background);
    let highlight = highlight_over(CLUSTER_SELECTION_BG, background);
    // The ligature spans pixels 1..19, striped from its own left edge; cells 1 and 2 (pixels
    // 10..30) are selected.
    let mut ligature_pixels: Vec<(u32, Srgb)> = (1..19)
        .map(|pixel_x| {
            let inked = (pixel_x - 1) % 3 != 0;
            let selected = pixel_x >= 10;
            let color = match (inked, selected) {
                (true, true) => CLUSTER_SELECTION_FG,
                (true, false) => foreground,
                (false, true) => highlight,
                (false, false) => background,
            };
            (pixel_x, color)
        })
        .collect();
    ligature_pixels.extend([(25, CLUSTER_SELECTION_FG), (35, foreground)]);
    let glyph = |left_px, width_px, pane, key, striped| ClusterGlyph {
        left_px,
        width_px,
        pane,
        key,
        striped,
    };
    vec![
        (
            "ligature and wide glyph half selected",
            vec![
                glyph(1.0, 18, ActivePane::Left, 'L', true),
                glyph(20.0, 20, ActivePane::Left, 'W', false),
            ],
            ActivePane::Left,
            (1, 2),
            ligature_pixels,
        ),
        (
            "neighbouring pane's overhang",
            vec![
                glyph(30.0, 16, ActivePane::Left, 'O', false),
                glyph(47.0, 3, ActivePane::Right, 'N', false),
            ],
            ActivePane::Right,
            (0, 1),
            vec![(35, foreground), (43, foreground), (48, CLUSTER_SELECTION_FG), (55, highlight)],
        ),
    ]
}

/// Assert each `(column, color)` of row 10 of `pixels` within `tolerance` per channel.
fn assert_columns(path: &str, expected: &[(u32, Srgb)], tolerance: u8, pixels: &[[u8; 4]]) {
    for &(pixel_x, (red, green, blue)) in expected {
        let [blue_seen, green_seen, red_seen, _] = pixels[(10 * WIDTH_PX + pixel_x) as usize];
        for (seen, wanted) in [(red_seen, red), (green_seen, green), (blue_seen, blue)] {
            assert!(
                seen.abs_diff(wanted) <= tolerance,
                "{path}: pixel {pixel_x} is {:?}, expected {:?}",
                (red_seen, green_seen, blue_seen),
                (red, green, blue)
            );
        }
    }
}

/// The selection foreground recolors only the selected cells of the owning pane on the software
/// compositor: a glyph crossing the selection's edge is cut there with its atlas coordinates kept,
/// so its unselected part keeps its color, and a neighbour's overhang is never recolored.
#[test]
fn software_selection_foreground_follows_the_selected_cells() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    for (case, glyphs, active, cols, expected) in cluster_cases(&theme) {
        let (atlas, quads, instances) = cluster_frame(&theme, &glyphs, active, cols);
        let pixels = software_pixels(clear, &atlas, &quads, &instances);
        assert_columns(&format!("software, {case}"), &expected, 2, &pixels);
    }
}

/// The wgpu pipeline draws every cluster scene as the software compositor does.
#[test]
fn wgpu_selection_foreground_follows_the_selected_cells() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    for (case, glyphs, active, cols, expected) in cluster_cases(&theme) {
        let (mut atlas, quads, instances) = cluster_frame(&theme, &glyphs, active, cols);
        let pixels = wgpu_pixels(clear, &mut atlas, &quads, &instances);
        assert_columns(&format!("wgpu, {case}"), &expected, 2, &pixels);
    }
}

/// One copy-mode frame of the left pane, whose program set every selection and cursor-text color
/// (`overrides`): four text glyphs, copy mode's range over cells 0-2 with its cursor on cell 2,
/// built with the production copy-mode quads and recolors in the order assembly runs them.
fn copy_mode_frame(
    theme: &Theme,
    overrides: &ColorOverrides,
) -> (GlyphAtlas, Vec<QuadInstance>, Vec<GlyphInstance>) {
    let (surface_w, surface_h) = (WIDTH_PX as f32, HEIGHT_PX as f32);
    let mut atlas = GlyphAtlas::new(64, 64);
    let text = atlas
        .get_or_insert_lazy_without_eviction(GlyphKey::new('M', false, false), 10, 20, || {
            RasterTile {
                width: 10,
                height: 20,
                offset_x: 0,
                offset_y: 0,
                advance: 10.0,
                coverage: vec![255; 10 * 20],
                is_color: false,
                is_subpixel: false,
            }
        })
        .expect("text tile");
    let foreground =
        crate::color::hex_to_premultiplied_rgba(theme.colors.foreground.0.as_str(), 1.0);
    let mut glyphs: Vec<GlyphInstance> = (0..4)
        .map(|column| GlyphInstance {
            rect: px_to_ndc(column as f32 * 10.0, 0.0, 10.0, 20.0, surface_w, surface_h),
            uv: text.uv,
            color: foreground,
            flags: [0.0; 4],
        })
        .collect();
    let mut rows = [RowGlyphSpan::new(&glyphs, 0..glyphs.len(), surface_w, surface_h)];
    let colors = PaneColors::new(theme, Some(overrides));
    let theme_highlight =
        crate::color::hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
    let theme_cursor = crate::color::hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0);
    let theme_cursor_text =
        crate::color::hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0);
    let grid = Grid::new(4, 1);
    let mut copy_mode = CopyModeState::new_at((0, 0));
    copy_mode.anchor = Some((0, 0));
    copy_mode.cursor = (2, 0);
    let geometry = SelectionGeometry {
        view_top_abs: 0,
        grid_size: (1, 4),
        origin: (0.0, 0.0),
        cell_size: (10.0, 20.0),
        clip: (0.0, 0.0, 40.0, 20.0),
        surface: (surface_w, surface_h),
    };
    let mut quads = Vec::new();
    let cursor = GpuRenderer::emit_copy_mode_quads(
        &copy_mode,
        &grid,
        0,
        0.0,
        0.0,
        10.0,
        20.0,
        surface_w,
        surface_h,
        colors.selection_rgba(theme_highlight),
        theme_cursor,
        &mut quads,
        &[],
    );
    if let Some(selected) = colors.selection_foreground_rgba() {
        recolor_copy_mode_glyphs(
            &mut glyphs,
            &mut rows,
            0..1,
            &copy_mode,
            &grid,
            &geometry,
            &[],
            selected,
        );
    }
    let (cursor_x, cursor_y) = cursor.expect("copy mode draws its cursor");
    let _ = recolor_cursor_glyphs_in(
        &mut glyphs,
        &rows,
        cursor_x,
        cursor_y,
        10.0,
        20.0,
        surface_w,
        surface_h,
        colors.cursor_text_rgba(theme_cursor_text),
    );
    (atlas, quads, glyphs)
}

/// Copy mode's selected range follows the mouse selection's rule on both compositors: with a
/// program selection foreground set, its selected glyphs take that color, the copy cursor's glyph
/// takes the cursor text over it, and the unselected cell keeps the foreground.
#[test]
fn copy_mode_selection_takes_the_program_selection_foreground() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::SelectionBackground, Some(CLUSTER_SELECTION_BG));
    overrides.set_dynamic(DynamicColor::SelectionForeground, Some(CLUSTER_SELECTION_FG));
    overrides.set_dynamic(DynamicColor::CursorText, Some((0, 0, 200)));
    let foreground = theme_rgb(&theme.colors.foreground);
    let expected = [
        (5, CLUSTER_SELECTION_FG),
        (15, CLUSTER_SELECTION_FG),
        (25, (0, 0, 200)),
        (35, foreground),
    ];
    let (mut atlas, quads, glyphs) = copy_mode_frame(&theme, &overrides);
    let software = software_pixels(clear, &atlas, &quads, &glyphs);
    assert_columns("software, copy mode", &expected, 2, &software);
    let wgpu = wgpu_pixels(clear, &mut atlas, &quads, &glyphs);
    assert_columns("wgpu, copy mode", &expected, 2, &wgpu);
}

/// A palette or search field keeps the theme's selection colors with every selection and
/// cursor-text color a program can set on the active pane: the field (an overlay over the left
/// pane's selected cells) draws its selected glyph in the theme's `selection_fg` on the theme's
/// `selection_bg` and its other glyph in the theme foreground, through the production field marks,
/// while the terminal's own selected glyph beside it takes the program's selection foreground.
#[test]
fn ui_fields_keep_the_theme_selection_colors_beside_a_program_selection() {
    let theme = Theme::default();
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let mut every = ColorOverrides::default();
    every.set_dynamic(DynamicColor::SelectionBackground, Some(CLUSTER_SELECTION_BG));
    every.set_dynamic(DynamicColor::SelectionForeground, Some(CLUSTER_SELECTION_FG));
    every.set_dynamic(DynamicColor::CursorText, Some((0, 0, 200)));
    let (mut atlas, quads, glyphs) = selection_frame(&theme, Some(&every), ActivePane::Left);
    let (surface_w, surface_h) = (WIDTH_PX as f32, HEIGHT_PX as f32);
    let text = atlas
        .get_or_insert_lazy_without_eviction(GlyphKey::new('M', false, false), 10, 20, || {
            unreachable!("the selection frame already holds the text tile")
        })
        .expect("text tile");
    let background =
        crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let foreground =
        crate::color::hex_to_premultiplied_rgba(theme.colors.foreground.0.as_str(), 1.0);
    // The field covers cells 1 and 2 of the selected left pane with an opaque backdrop and two glyphs.
    let mut overlay_quads = vec![QuadInstance::sharp(
        px_to_ndc(10.0, 0.0, 20.0, 20.0, surface_w, surface_h),
        background,
    )];
    let mut overlay_glyphs: Vec<GlyphInstance> = [10.0, 20.0]
        .into_iter()
        .map(|left_px| GlyphInstance {
            rect: px_to_ndc(left_px, 0.0, 10.0, 20.0, surface_w, surface_h),
            uv: text.uv,
            color: foreground,
            flags: [0.0; 4],
        })
        .collect();
    let (field_selection_bg, field_selection_fg) = field_selection_colors(&theme);
    crate::cursor::paint_field_marks(
        &mut overlay_quads,
        &mut overlay_glyphs,
        Vec::new(),
        &[crate::cursor::FieldMark {
            rect: (10.0, 0.0, 10.0, 20.0),
            background: field_selection_bg,
            foreground: field_selection_fg,
        }],
        surface_w,
        surface_h,
    );
    let expected = [
        (5, CLUSTER_SELECTION_FG),
        (15, theme_rgb(&theme.colors.selection_fg)),
        (25, theme_rgb(&theme.colors.foreground)),
    ];
    let software = software_pixels_with_overlay(
        clear,
        &atlas,
        (&quads, &glyphs),
        (&overlay_quads, &overlay_glyphs),
    );
    assert_columns("software, field", &expected, 2, &software);
    let wgpu = wgpu_pixels_with_overlay(
        clear,
        &mut atlas,
        (&quads, &glyphs),
        (&overlay_quads, &overlay_glyphs),
    );
    assert_columns("wgpu, field", &expected, 2, &wgpu);
}
