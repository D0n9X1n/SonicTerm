//! A split where one pane's program set its colors: the software compositor and the wgpu pipeline
//! both draw that pane's blank area, inverse cell, palette cell, cursor, selection highlight,
//! selected glyphs and cursor text in the program colors, while the sibling pane keeps the theme.

use super::wezterm_pipeline_tests::headless_device_and_queue;
use super::*;
use crate::core::{
    build_snapped_cell_x, emit_cell_bg_quads_for_row, push_selection_quads,
    recolor_selected_glyphs, PaneColors, SelectionGeometry,
};
use crate::cursor::{recolor_cursor_glyphs_in, RowGlyphSpan};
use crate::quad::px_to_ndc;
use sonicterm_render_model::boundary::cfg::theme::{Hex, Theme};
use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
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
    let mut frame =
        crate::software_frame::SoftwareFrame::new(WIDTH_PX, HEIGHT_PX, clear).expect("frame");
    frame.draw_layers_with_subpixel_aa(
        atlas,
        atlas,
        SubpixelAaMode::Off,
        quads,
        &[],
        glyphs,
        &[],
        &[],
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
    let (device, queue) = headless_device_and_queue();
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline = WeztermPipeline::new(&device, format, quads.len().max(1) as u64);
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
            &[],
            &[],
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
#[derive(Clone, Copy)]
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
    let row_spans = [RowGlyphSpan::new(&glyphs, 0..glyphs.len(), surface_w, surface_h)];
    let (origin, overrides) = match active {
        ActivePane::Left => (0.0, left_overrides),
        ActivePane::Right => (40.0, None),
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
        recolor_selected_glyphs(&mut glyphs, &row_spans, &selection, &geometry, &[], selected);
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
