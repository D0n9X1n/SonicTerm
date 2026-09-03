use super::*;
use sonicterm_text::glyph_atlas::{AtlasPixelKind, ATLAS_DIM};

fn coverage_rect(x: u32, y: u32, w: u32, h: u32) -> DirtyRect {
    DirtyRect { x, y, w, h, kind: AtlasPixelKind::Coverage }
}

fn color_rect(x: u32, y: u32, w: u32, h: u32) -> DirtyRect {
    DirtyRect { x, y, w, h, kind: AtlasPixelKind::Color }
}

fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    #[cfg(target_os = "windows")]
    let (descriptor, force_fallback_adapter) = (
        wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        },
        true,
    );
    #[cfg(not(target_os = "windows"))]
    let (descriptor, force_fallback_adapter) =
        (wgpu::InstanceDescriptor::new_without_display_handle(), false);
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter,
        apply_limit_buckets: false,
    }))
    .expect("headless test adapter");
    let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    pollster::block_on(adapter.request_device(&crate::core::device_descriptor_for(software)))
        .expect("headless test device")
}

/// Render one image-atlas strip through the real sRGB view and return its BGRA readback.
fn render_image_readback(source_width: u32, pixels: &[u8], target_width: u32) -> Vec<u8> {
    render_image_readback_with_neighbor(source_width, pixels, target_width, None)
}

fn render_image_readback_with_neighbor(
    source_width: u32,
    pixels: &[u8],
    target_width: u32,
    neighbor: Option<[u8; 4]>,
) -> Vec<u8> {
    const PADDED_BYTES_PER_ROW: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let (device, queue) = headless_device();
    let mut pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1,
    );
    let atlas_width = source_width + u32::from(neighbor.is_some());
    let mut atlas = GlyphAtlas::new(atlas_width, 1);
    let info = atlas
        .get_or_insert_lazy_without_eviction(
            sonicterm_types::GlyphKey::new('\u{fffc}', false, false),
            source_width,
            1,
            || sonicterm_text::glyph_atlas::RasterTile {
                width: source_width,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: source_width as f32,
                coverage: pixels.to_vec(),
                is_color: true,
                is_subpixel: false,
            },
        )
        .expect("image test tile inserts");
    if let Some(neighbor) = neighbor {
        atlas
            .get_or_insert_lazy_without_eviction(
                sonicterm_types::GlyphKey::new('N', false, false),
                1,
                1,
                || sonicterm_text::glyph_atlas::RasterTile {
                    width: 1,
                    height: 1,
                    offset_x: 0,
                    offset_y: 0,
                    advance: 1.0,
                    coverage: neighbor.to_vec(),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .expect("neighbor test tile inserts");
    }
    let cpu_pixels = atlas.pixels_bgra().to_vec();
    let mut image_upload = AtlasUpload::new(
        &device,
        &atlas,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    let glyph_upload = AtlasUpload::new(
        &device,
        &atlas,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );
    image_upload.sync(&queue, &mut atlas);
    assert_eq!(atlas.pixels_bgra(), cpu_pixels, "GPU upload must not rewrite CPU atlas bytes");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atlas color readback target"),
        size: wgpu::Extent3d { width: target_width, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atlas color readback"),
        size: u64::from(PADDED_BYTES_PER_ROW),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let image = sonicterm_text::GlyphInstance {
        rect: crate::quad::px_to_ndc(0.0, 0.0, target_width as f32, 1.0, target_width as f32, 1.0),
        uv: info.uv,
        color: [1.0; 4],
        flags: [1.0, 0.0, 1.0, 0.0],
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atlas color readback pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
            target_width as f32,
            1.0,
            &[],
            &[image],
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
                bytes_per_row: Some(PADDED_BYTES_PER_ROW),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d { width: target_width, height: 1, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll atlas color readback");
    let mapped = slice.get_mapped_range().expect("mapped atlas color readback");
    let result = mapped[..target_width as usize * BYTES_PER_PIXEL as usize].to_vec();
    drop(mapped);
    readback.unmap();
    result
}

/// Render adjacent synthetic glyph tiles through the real unified pipeline and return BGRA pixels.
fn render_glyph_readback(tiles: &[sonicterm_text::glyph_atlas::RasterTile]) -> Vec<u8> {
    const PADDED_BYTES_PER_ROW: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let (device, queue) = headless_device();
    let mut pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        tiles.len() as u64,
    );
    let mut atlas = GlyphAtlas::new(tiles.len() as u32, 1);
    let mut glyphs = Vec::with_capacity(tiles.len());
    for (index, tile) in tiles.iter().enumerate() {
        let info = atlas
            .get_or_insert(
                sonicterm_types::GlyphKey::new(
                    char::from_u32('A' as u32 + index as u32).expect("test glyph key"),
                    false,
                    false,
                ),
                &mut TestTileRasterizer(tile.clone()),
            )
            .expect("glyph test tile inserts");
        glyphs.push(sonicterm_text::GlyphInstance {
            rect: crate::quad::px_to_ndc(index as f32, 0.0, 1.0, 1.0, tiles.len() as f32, 1.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [f32::from(info.is_color), f32::from(info.is_subpixel), 0.0, 0.0],
        });
    }
    let cpu_pixels = atlas.pixels_bgra().to_vec();
    let image_upload = AtlasUpload::new(
        &device,
        &atlas,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    let mut glyph_upload = AtlasUpload::new(
        &device,
        &atlas,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );
    glyph_upload.sync(&queue, &mut atlas);
    assert_eq!(atlas.pixels_bgra(), cpu_pixels, "GPU upload must preserve CPU glyph bytes");

    let width = tiles.len() as u32;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mixed glyph readback target"),
        size: wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mixed glyph readback"),
        size: u64::from(PADDED_BYTES_PER_ROW),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mixed glyph readback pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
            width as f32,
            1.0,
            &[],
            &[],
            &glyphs,
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
                bytes_per_row: Some(PADDED_BYTES_PER_ROW),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll mixed glyph readback");
    let mapped = slice.get_mapped_range().expect("mapped mixed glyph readback");
    let result = mapped[..width as usize * BYTES_PER_PIXEL as usize].to_vec();
    drop(mapped);
    readback.unmap();
    result
}

struct TestTileRasterizer(sonicterm_text::glyph_atlas::RasterTile);

impl sonicterm_text::glyph_atlas::Rasterizer for TestTileRasterizer {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(self.0.clone())
    }
}

/// Adjacent color and grayscale glyphs must select sRGB color and unorm coverage views respectively.
#[test]
fn gpu_mixed_glyphs_select_distinct_texture_views() {
    let output = render_glyph_readback(&[
        sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![128, 128, 128, 255],
            is_color: true,
            is_subpixel: false,
        },
        sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![128],
            is_color: false,
            is_subpixel: false,
        },
    ]);

    for &actual in &output[0..3] {
        assert!(actual.abs_diff(128) <= 1, "color glyph must stay encoded gray 128: {output:?}");
    }
    assert_eq!(output[3], 255);
    for &actual in &output[4..7] {
        assert!(actual.abs_diff(188) <= 2, "coverage glyph must remain linear: {output:?}");
    }
    assert!(output[7].abs_diff(128) <= 1);
}

/// Translucent color glyphs convert encoded premultiplication before sRGB-view sampling.
#[test]
fn gpu_translucent_color_glyph_blends_as_premultiplied_linear() {
    let output = render_glyph_readback(&[sonicterm_text::glyph_atlas::RasterTile {
        width: 1,
        height: 1,
        offset_x: 0,
        offset_y: 0,
        advance: 1.0,
        coverage: vec![64, 64, 64, 128],
        is_color: true,
        is_subpixel: false,
    }]);

    for (actual, expected) in output.iter().zip([92, 92, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
}

#[test]
fn coalesces_touching_rows_and_columns() {
    let input = [
        coverage_rect(0, 0, 2, 1),
        coverage_rect(2, 0, 2, 1),
        coverage_rect(0, 1, 4, 1),
        coverage_rect(7, 7, 1, 1),
    ];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, [coverage_rect(0, 0, 4, 2), coverage_rect(7, 7, 1, 1)]);
}

#[test]
fn separate_dirty_regions_remain_separate() {
    let input = [coverage_rect(0, 0, 1, 1), coverage_rect(2, 0, 1, 1)];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, input);
}

/// Touching writes coalesce within one interpretation but never across coverage and color.
#[test]
fn coalescing_keeps_touching_pixel_kinds_separate() {
    let input = [coverage_rect(0, 0, 1, 1), coverage_rect(1, 0, 1, 1), color_rect(2, 0, 1, 1)];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, [coverage_rect(0, 0, 2, 1), color_rect(2, 0, 1, 1)]);
}

/// Coverage uploads preserve every CPU atlas byte while reusing retained staging.
#[test]
fn coverage_copies_tightly_packed_subrect_and_reuses_capacity() {
    let pixels: Vec<u8> = (0..48).collect();
    let rect = coverage_rect(1, 0, 2, 2);
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, 4, rect, &mut scratch);
    let capacity = scratch.capacity();

    assert_eq!(scratch, [4, 5, 6, 7, 8, 9, 10, 11, 20, 21, 22, 23, 24, 25, 26, 27]);

    copy_rect_into_scratch(&pixels, 4, coverage_rect(0, 0, 1, 1), &mut scratch);
    assert_eq!(scratch, [0, 1, 2, 3]);
    assert_eq!(scratch.capacity(), capacity);
}

/// DirectWrite subpixel coverage remains byte-exact through kind-directed GPU staging.
#[test]
fn subpixel_coverage_staging_preserves_bgra_channels() {
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&[10, 20, 30, 40], 1, coverage_rect(0, 0, 1, 1), &mut scratch);

    assert_eq!(scratch, [10, 20, 30, 40]);
}

/// One atlas allocation supplies unorm coverage and sRGB color views without retaining a second payload.
#[test]
fn one_texture_exposes_coverage_and_color_bind_groups() {
    let (device, _queue) = headless_device();
    let pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1,
    );
    let upload = AtlasUpload::new_sized(
        &device,
        2,
        3,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );

    assert_eq!(upload.texture.format(), wgpu::TextureFormat::Bgra8Unorm);
    assert_eq!(upload.texture.size().width, 2);
    assert_eq!(upload.texture.size().height, 3);
    assert_eq!(upload.coverage_view().texture(), upload.color_view().texture());
    assert_eq!(upload.payload_bytes(), 2 * 3 * u64::from(BYTES_PER_PIXEL));
    let _glyph = upload.glyph_bind_group();
}

/// View encoding and sampler policy stay orthogonal for image and color-glyph consumers.
#[test]
fn image_color_filtering_does_not_define_the_srgb_view_policy() {
    let coverage = atlas_sampler_descriptor();
    let image = image_atlas_sampler_descriptor();

    assert_eq!(coverage.mag_filter, wgpu::FilterMode::Nearest);
    assert_eq!(coverage.min_filter, wgpu::FilterMode::Nearest);
    assert_eq!(image.mag_filter, wgpu::FilterMode::Linear);
    assert_eq!(image.min_filter, wgpu::FilterMode::Linear);
}

/// The completed glyph group exposes both views with nearest filtering from one texture.
#[test]
fn glyph_bind_group_combines_coverage_and_color_views() {
    let (device, _queue) = headless_device();
    let pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1,
    );
    let upload = AtlasUpload::new_sized(
        &device,
        1,
        1,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );

    assert_eq!(upload.coverage_view().texture(), upload.color_view().texture());
    let _nearest = upload.nearest_sampler();
    let _glyph = upload.glyph_bind_group();
}

/// An opaque encoded midtone survives upload, sRGB sampling, blending, and sRGB output unchanged.
#[test]
fn gpu_srgb_view_preserves_opaque_midtone() {
    let output = render_image_readback(1, &[128, 128, 128, 255], 1);

    for (actual, expected) in output.iter().zip([128, 128, 128, 255]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected={expected}");
    }
}

/// A translucent encoded-premultiplied image pixel is linearized before GPU source-over.
#[test]
fn gpu_srgb_view_converts_translucent_encoded_premultiplication() {
    let output = render_image_readback(1, &[64, 64, 64, 128], 1);

    for (actual, expected) in output.iter().zip([92, 92, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
}

/// Linear filtering must not pull pixels from the next packed image tile.
#[test]
fn gpu_scaled_image_clamps_to_its_own_atlas_tile() {
    let output =
        render_image_readback_with_neighbor(1, &[255, 255, 255, 255], 3, Some([0, 0, 255, 255]));

    for pixel in output.as_chunks::<4>().0 {
        assert_eq!(*pixel, [255, 255, 255, 255]);
    }
}

/// Scaled images interpolate decoded linear samples, not their encoded sRGB byte values.
#[test]
fn gpu_scaled_image_filters_in_linear_light() {
    let output = render_image_readback(2, &[0, 0, 0, 255, 255, 255, 255, 255], 3);
    let middle = &output[4..8];

    for &actual in &middle[..3] {
        assert!(actual.abs_diff(188) <= 2, "linear midpoint should encode near 188: {middle:?}");
    }
    assert_eq!(middle[3], 255);
}

/// Color uploads convert encoded premultiplication to linear-light premultiplication for an sRGB view.
#[test]
fn premultiplied_srgb_converts_representative_pixels() {
    let pixels = [128, 128, 128, 255, 64, 64, 64, 128, 16, 32, 64, 128, 77, 88, 99, 0];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, 4, color_rect(0, 0, 4, 1), &mut scratch);

    assert_eq!(&scratch[0..4], &[128, 128, 128, 255]);
    for (actual, expected) in scratch[4..8].iter().zip([92, 92, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
    for (actual, expected) in scratch[8..12].iter().zip([20, 44, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
    assert_eq!(&scratch[12..16], &[0, 0, 0, 0]);
}

/// Both encoding modes reuse one bounded staging allocation rather than retaining converted atlases.
#[test]
fn staging_capacity_is_reused_and_bounded_in_both_modes() {
    let width = 64u32;
    let height = 8u32;
    let pixels = vec![128u8; width as usize * height as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, width, color_rect(0, 0, width, height), &mut scratch);
    let capacity = scratch.capacity();
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, height), &mut scratch);

    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    assert_eq!(scratch.capacity(), capacity);
    assert!(scratch.capacity() <= whole_atlas);
}

/// Growing staging to one full atlas never retains geometric spare capacity above that bound.
#[test]
fn full_atlas_staging_growth_stays_at_the_atlas_bound() {
    let width = ATLAS_DIM;
    let pixels = vec![0u8; width as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(
        &pixels,
        width,
        coverage_rect(0, 0, width, ATLAS_DIM * 3 / 4),
        &mut scratch,
    );
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, ATLAS_DIM), &mut scratch);

    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    assert_eq!(scratch.capacity(), whole_atlas);
}

/// What the retained staging buffer can hold, measured rather than assumed.
///
/// The test above establishes that capacity survives the call — deliberately,
/// because reallocating per frame on this path would cost more than the memory
/// does. What it does not say is how much memory that is, and the class is
/// recorded `UnchargedRetention` on exactly that figure, so it is worth having
/// under a test rather than only in a comment.
///
/// Derived from the atlas constants rather than typed, because a dirty rect
/// cannot exceed the atlas it comes from.
#[test]
fn retained_staging_is_bounded_by_one_whole_atlas() {
    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;

    assert_eq!(
        whole_atlas,
        16 * 1024 * 1024,
        "a full-atlas dirty rect stages this much, and it is retained after the copy"
    );

    // Measured on the real function rather than argued from the constants: a
    // copy of a given size leaves at least that much capacity behind.
    let width = 512u32;
    let height = 64u32;
    let pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, height), &mut scratch);

    let copied = width as usize * height as usize * BYTES_PER_PIXEL as usize;
    assert!(
        scratch.capacity() >= copied,
        "a {copied}-byte copy left {} bytes of capacity; the retained figure scales with \
         the rect, up to {whole_atlas} for a whole atlas",
        scratch.capacity()
    );
}

/// The recorded figure must be the atlas arithmetic, not a number beside it.
///
/// `sonicterm-types` is a contract crate and cannot depend on
/// `sonicterm-text`, so the figure in the coverage table is typed rather than
/// derived — the same constraint that makes the pane-charge list a hand-kept
/// mirror. A typed figure drifts silently when `ATLAS_DIM` changes, and every
/// accounting defect this milestone found had that shape.
///
/// This crate sees both, so the check belongs here: it recomputes the figure
/// from the constants and compares it to what the table records. Change the
/// atlas and this fails until the row is updated.
#[test]
fn the_recorded_class_figure_matches_the_atlas_constants() {
    use sonicterm_types::{ClassCoverage, ResourceClass};

    let per_upload = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    // One for glyphs, one for images; `GpuRenderer` holds both for its life.
    let uploads_per_renderer = 2;
    let derived = per_upload * uploads_per_renderer;

    let ClassCoverage::UnchargedRetention { per_owner_bytes } =
        ResourceClass::UploadStaging.coverage()
    else {
        panic!(
            "`UploadStaging` retains its staging buffer between copies and nothing charges \
             it, so it must record `UnchargedRetention` with what it holds"
        );
    };

    assert_eq!(
        per_owner_bytes, derived,
        "the coverage table records {per_owner_bytes} bytes for `UploadStaging` but the \
         atlas constants give {derived} ({uploads_per_renderer} x {per_upload}); the typed \
         figure has drifted from the atlas it describes"
    );
}
