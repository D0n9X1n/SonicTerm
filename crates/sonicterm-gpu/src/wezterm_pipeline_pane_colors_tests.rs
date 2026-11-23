//! A split where one pane's program set its colors: the software compositor and the wgpu pipeline
//! both draw that pane's blank area, inverse cell, palette cell and cursor in the program colors,
//! while the sibling pane keeps the theme.

use super::wezterm_pipeline_tests::headless_device_and_queue;
use super::*;
use crate::core::{build_snapped_cell_x, emit_cell_bg_quads_for_row, PaneColors};
use crate::quad::px_to_ndc;
use sonicterm_render_model::boundary::cfg::theme::Theme;
use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
use sonicterm_types::{ColorOverrides, DynamicColor};

/// Surface width: two 40 px panes of four 10 px cells each.
const WIDTH_PX: u32 = 80;
/// Surface height: one 20 px row.
const HEIGHT_PX: u32 = 20;
/// Row stride of the wgpu readback, padded to the copy alignment.
const READBACK_STRIDE: u32 = 512;

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

/// The sRGB color expected at the centre of each of the eight cells, left pane first.
fn expected_cells(theme: &Theme) -> [(u8, u8, u8); 8] {
    let theme_rgb = |hex: &sonicterm_render_model::boundary::cfg::theme::Hex| {
        hex.rgb().expect("default theme colors are hex")
    };
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

/// Assert each cell's centre pixel, read as BGRA by `pixel_at`, within one step per channel.
fn assert_cells(path: &str, theme: &Theme, pixel_at: impl Fn(u32, u32) -> [u8; 4]) {
    for (cell, (red, green, blue)) in expected_cells(theme).into_iter().enumerate() {
        let [blue_seen, green_seen, red_seen, _] = pixel_at(cell as u32 * 10 + 5, HEIGHT_PX / 2);
        for (seen, wanted) in [(red_seen, red), (green_seen, green), (blue_seen, blue)] {
            assert!(
                seen.abs_diff(wanted) <= 1,
                "{path}: cell {cell} is {:?}, expected {:?}",
                (red_seen, green_seen, blue_seen),
                (red, green, blue)
            );
        }
    }
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
    let atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let mut frame =
        crate::software_frame::SoftwareFrame::new(WIDTH_PX, HEIGHT_PX, clear).expect("frame");
    frame.draw_layers_with_subpixel_aa(
        &atlas,
        &atlas,
        SubpixelAaMode::Off,
        &quads,
        &[],
        &[],
        &[],
        &[],
    );
    assert_cells("software", &theme, |x, y| frame.pixel_bgra_at(x, y).expect("in bounds"));
}

/// The wgpu pipeline, on an sRGB target as the surface is, draws the same split the same way.
#[test]
fn wgpu_split_keeps_each_pane_in_its_own_colors() {
    let theme = Theme::default();
    let overrides = program_colors();
    let grid = split_row();
    let quads = split_quads(&theme, &overrides, &grid);
    let clear = crate::color::hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
    let (device, queue) = headless_device_and_queue();
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut pipeline = WeztermPipeline::new(&device, format, quads.len() as u64);
    let atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let image_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.image_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Image,
    );
    let glyph_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.glyph_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Glyph,
    );
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
            &quads,
            &[],
            &[],
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
    assert_cells("wgpu", &theme, |x, y| {
        let offset = (y * READBACK_STRIDE + x * 4) as usize;
        [mapped[offset], mapped[offset + 1], mapped[offset + 2], mapped[offset + 3]]
    });
}
