#![cfg(test)]
//! Shared test-only raster fixture state and native custody.

use super::*;

/// A style run and the row it shapes into, for driving the record builder directly.
pub(in crate::core) struct ShapeRunFixture<'run> {
    pub(in crate::core) atlas: &'run mut GlyphAtlas,
    pub(in crate::core) row: u16,
    pub(in crate::core) style: RunStyle,
    pub(in crate::core) cells: &'run [(u16, Cell)],
    pub(in crate::core) theme: &'run Theme,
    pub(in crate::core) fg_default: ChromeColor,
    pub(in crate::core) cell_size: (f32, f32),
    pub(in crate::core) origin: (f32, f32),
    pub(in crate::core) surface: (f32, f32),
    pub(in crate::core) baseline_y_in_cell: f32,
    pub(in crate::core) snapped_cell_x: &'run [f32],
    pub(in crate::core) font_stack: Option<&'run sonicterm_engine::FontStack>,
    pub(in crate::core) wt_raster: Option<&'run mut sonicterm_engine::FontStack>,
    pub(in crate::core) hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pub(in crate::core) hovered_url_accent: [f32; 4],
    pub(in crate::core) software_presenter: bool,
}

/// Build one style run's records and project them at `fixture.row`, as a missed row is drawn,
/// appending glyphs, tofu and missing characters; returns the run's completeness.
pub(in crate::core) fn shape_run_for_test(
    fixture: ShapeRunFixture<'_>,
    glyphs: &mut Vec<GlyphInstance>,
    tofu: &mut Vec<(f32, f32, f32, f32, ChromeColor)>,
    missing: &mut Vec<char>,
) -> bool {
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    // The builder takes cells borrowed from a grid; the fixture owns its cells.
    let borrowed: Vec<(u16, &Cell)> =
        fixture.cells.iter().map(|(col, cell)| (*col, cell)).collect();
    let complete = GpuRenderer::build_shape_run(
        fixture.atlas,
        &mut records,
        fixture.row,
        fixture.style,
        &borrowed,
        fixture.theme,
        fixture.fg_default,
        fixture.cell_size.0,
        fixture.cell_size.1,
        fixture.origin.1,
        fixture.snapped_cell_x,
        fixture.font_stack,
        fixture.wt_raster,
        fixture.hovered_url_cells,
        fixture.hovered_url_accent,
        fixture.software_presenter,
    );
    let at = RowPlacement {
        slot: fixture.row,
        origin: fixture.origin,
        cols: (fixture.snapped_cell_x.len().saturating_sub(1)) as u16,
        snapped_cell_x: fixture.snapped_cell_x,
        cell_size: fixture.cell_size,
        baseline_y_in_cell: fixture.baseline_y_in_cell,
        surface: fixture.surface,
    };
    let (mut underlines, mut spans) = (Vec::new(), Vec::new());
    project_cached_row(
        &records,
        &at,
        fixture.software_presenter,
        GlyphFrame {
            glyph_instances: glyphs,
            underlines: &mut underlines,
            missing_tofu: tofu,
            missing_chars_this_frame: missing,
            row_spans: &mut spans,
        },
    );
    complete
}

#[derive(Clone, Default)]
pub(in crate::core) struct GlyphLogCapture(
    pub(in crate::core) std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
);

impl std::io::Write for GlyphLogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for GlyphLogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

// --- tab title colour hover (Issue: custom tab colour did not highlight) ---

/// Distinct sentinel colours so each branch is unambiguous in asserts.
pub(in crate::core) fn active_fg() -> ChromeColor {
    ChromeColor::rgb(0xEB, 0xDB, 0xB2) // gruvbox fg0
}
pub(in crate::core) fn inactive_fg() -> ChromeColor {
    ChromeColor::rgb(0x92, 0x83, 0x74) // gruvbox gray
}

pub(in crate::core) struct OnePixelAtlasGlyph;

impl sonicterm_text::glyph_atlas::Rasterizer for OnePixelAtlasGlyph {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255],
            is_color: false,
            is_subpixel: false,
        })
    }
}

#[cfg(target_os = "windows")]
pub(in crate::core) struct SolidTallAtlasGlyph;

#[cfg(target_os = "windows")]
impl sonicterm_text::glyph_atlas::Rasterizer for SolidTallAtlasGlyph {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 30,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255; 30],
            is_color: false,
            is_subpixel: false,
        })
    }
}

pub(in crate::core) struct RetainedPixelFixture {
    pub(in crate::core) device: wgpu::Device,
    pub(in crate::core) queue: wgpu::Queue,
    pub(in crate::core) pipeline: WeztermPipeline,
    pub(in crate::core) image_upload: crate::atlas_upload::AtlasUpload,
    pub(in crate::core) glyph_upload: crate::atlas_upload::AtlasUpload,
    pub(in crate::core) target: wgpu::Texture,
    pub(in crate::core) view: wgpu::TextureView,
    pub(in crate::core) dual_source: bool,
}

impl RetainedPixelFixture {
    pub(in crate::core) fn new() -> Self {
        #[cfg(target_os = "windows")]
        let (backends, fallback) = (wgpu::Backends::DX12, true);
        #[cfg(not(target_os = "windows"))]
        let (backends, fallback) = (wgpu::Backends::all(), false);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: fallback,
            apply_limit_buckets: false,
        }))
        .expect("retained-pixel adapter");
        let features = selected_optional_device_features(adapter.features(), cfg!(windows));
        let dual_source = features.contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor_for(
            adapter.get_info().device_type == wgpu::DeviceType::Cpu,
            features,
        )))
        .expect("retained-pixel device");
        let pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 3);
        let atlas = GlyphAtlas::new(1, 1);
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
            label: Some("retained reset pixel fixture"),
            size: wgpu::Extent3d { width: 4, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        Self { device, queue, pipeline, image_upload, glyph_upload, target, view, dual_source }
    }

    pub(in crate::core) fn draw(
        &mut self,
        first: bool,
        damage: PixelRect,
        background: wgpu::Color,
        mode: SubpixelAaMode,
        quads: &[QuadInstance],
    ) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        draw_retained_frame(
            &mut self.pipeline,
            &self.device,
            &self.queue,
            &mut encoder,
            &self.view,
            self.image_upload.image_bind_group(),
            self.glyph_upload.glyph_bind_group(),
            4.0,
            2.0,
            first,
            damage,
            background,
            mode,
            quads,
            &[],
            &[],
            &[],
            &[],
        );
        self.queue.submit([encoder.finish()]);
    }

    pub(in crate::core) fn pixels(&self) -> Vec<[u8; 4]> {
        const STRIDE: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retained reset readback"),
            size: u64::from(STRIDE) * 2,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(STRIDE),
                    rows_per_image: Some(2),
                },
            },
            wgpu::Extent3d { width: 4, height: 2, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("retained readback poll");
        let mapped = slice.get_mapped_range().expect("retained mapped pixels");
        let mut pixels = Vec::new();
        for y in 0..2 {
            for x in 0..4 {
                let offset = y * STRIDE as usize + x * 4;
                pixels.push(mapped[offset..offset + 4].try_into().unwrap());
            }
        }
        drop(mapped);
        readback.unmap();
        pixels
    }
}

pub(in crate::core) fn reset_background(opacity: f32) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(0.125 * opacity),
        g: f64::from(0.25 * opacity),
        b: f64::from(0.5 * opacity),
        a: f64::from(opacity),
    }
}

pub(in crate::core) fn clear_bgra(background: wgpu::Color) -> [u8; 4] {
    [
        crate::color::linear_channel_to_srgb_u8(background.b as f32),
        crate::color::linear_channel_to_srgb_u8(background.g as f32),
        crate::color::linear_channel_to_srgb_u8(background.r as f32),
        (background.a * 255.0).round() as u8,
    ]
}

pub(in crate::core) fn pixels_close(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual.into_iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1)
}

pub(in crate::core) fn striped_inline_image(
    width: u32,
    height: u32,
) -> sonicterm_render_model::InlineImage {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[((x * 13) % 256) as u8, ((y * 71) % 256) as u8, 180, 255]);
        }
    }
    sonicterm_render_model::InlineImage {
        id: 71,
        row: 0,
        col: 0,
        width,
        height,
        bgra: std::sync::Arc::from(pixels),
        kitty: None,
    }
}

pub(in crate::core) fn packed_image_fixture(
    image: &sonicterm_render_model::InlineImage,
    origin: [f32; 2],
    content_clip: PaneRect,
) -> (GlyphAtlas, Vec<ImageInstance>) {
    let mut atlas = GlyphAtlas::new(image.width + 2, image.height + 2);
    let mut pack_neighbor = |ch, width, height| {
        atlas
            .get_or_insert_lazy_without_eviction(
                sonicterm_types::GlyphKey::new(ch, false, false),
                width,
                height,
                || sonicterm_text::glyph_atlas::RasterTile {
                    width,
                    height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage: [0, 0, 255, 255].repeat((width * height) as usize),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .unwrap();
    };
    pack_neighbor('T', image.width + 2, 1);
    pack_neighbor('L', 1, image.height);
    let placement = InlineImagePlacement {
        image,
        origin_x: origin[0],
        origin_y: origin[1],
        content_clip,
        painter_order: 0,
        slice: None,
    };
    let mut instances = Vec::new();
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 16.0, 12.0),
        0
    );
    for (ch, width, height) in [('R', 1, image.height), ('B', image.width + 2, 1)] {
        atlas
            .get_or_insert_lazy_without_eviction(
                sonicterm_types::GlyphKey::new(ch, false, false),
                width,
                height,
                || sonicterm_text::glyph_atlas::RasterTile {
                    width,
                    height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage: [0, 0, 255, 255].repeat((width * height) as usize),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .unwrap();
    }
    (atlas, instances)
}
