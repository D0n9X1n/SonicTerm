use super::*;
pub(super) use sonicterm_text::glyph_atlas::{AtlasPixelKind, ATLAS_DIM};

pub(super) fn coverage_rect(x: u32, y: u32, w: u32, h: u32) -> DirtyRect {
    DirtyRect { x, y, w, h, kind: AtlasPixelKind::Coverage }
}

pub(super) fn color_rect(x: u32, y: u32, w: u32, h: u32) -> DirtyRect {
    DirtyRect { x, y, w, h, kind: AtlasPixelKind::Color }
}

pub(super) fn headless_device() -> (wgpu::Device, wgpu::Queue) {
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
    pollster::block_on(
        adapter
            .request_device(&crate::core::device_descriptor_for(software, wgpu::Features::empty())),
    )
    .expect("headless test device")
}

/// Render one image-atlas strip through the real sRGB view and return its BGRA readback.
pub(super) fn render_image_readback(
    source_width: u32,
    pixels: &[u8],
    target_width: u32,
) -> Vec<u8> {
    render_image_readback_with_neighbor(source_width, pixels, target_width, None)
}

pub(super) fn render_image_readback_with_neighbor(
    source_width: u32,
    pixels: &[u8],
    target_width: u32,
    neighbor: Option<[u8; 4]>,
) -> Vec<u8> {
    render_image_readback_on(source_width, pixels, target_width, neighbor, [0.0; 4])
}

/// Render an image strip over one linear clear color for cross-presenter tests.
pub(crate) fn render_image_readback_on(
    source_width: u32,
    pixels: &[u8],
    target_width: u32,
    neighbor: Option<[u8; 4]>,
    clear: [f32; 4],
) -> Vec<u8> {
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
    let image = crate::wezterm_pipeline::ImageInstance {
        rect_px: [0.0, 0.0, target_width as f32, 1.0],
        uv: info.uv,
        sample_uv: info.uv,
    };
    render_image_instances_readback(&mut atlas, &[image], target_width, 1, clear)
}

/// Read real image-atlas pixels for arbitrary destination geometry without altering CPU atlas storage.
pub(crate) fn render_image_instances_readback(
    atlas: &mut GlyphAtlas,
    images: &[crate::wezterm_pipeline::ImageInstance],
    width: u32,
    height: u32,
    clear: [f32; 4],
) -> Vec<u8> {
    render_instances_readback(atlas, images, &[], (width, height), clear)
}

/// Draw `images` and `glyphs` from `atlas` through the real unified pipeline over `clear` and
/// return the BGRA readback. Uploading drains the atlas's dirty rects, so the atlas binding
/// the instances use is the one synchronized: the glyph binding when any glyph is drawn.
pub(super) fn render_instances_readback(
    atlas: &mut GlyphAtlas,
    images: &[crate::wezterm_pipeline::ImageInstance],
    glyphs: &[sonicterm_text::GlyphInstance],
    (width, height): (u32, u32),
    clear: [f32; 4],
) -> Vec<u8> {
    let stride = (width * BYTES_PER_PIXEL).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let (device, queue) = headless_device();
    let mut pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        (images.len() + glyphs.len()).max(1) as u64,
    );
    let cpu_pixels = atlas.pixels_bgra().to_vec();
    let mut image_upload = AtlasUpload::new(
        &device,
        atlas,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    let mut glyph_upload = AtlasUpload::new(
        &device,
        atlas,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );
    if glyphs.is_empty() {
        image_upload.sync(&queue, atlas);
    } else {
        // When: glyphs are drawn, the glyph binding receives the atlas's pending tiles.
        glyph_upload.sync(&queue, atlas);
    }
    assert_eq!(atlas.pixels_bgra(), cpu_pixels, "GPU upload must not rewrite CPU atlas bytes");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atlas color readback target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
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
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atlas color readback pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0] as f64,
                        g: clear[1] as f64,
                        b: clear[2] as f64,
                        a: clear[3] as f64,
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
            width as f32,
            height as f32,
            sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::Off,
            None,
            &[],
            images,
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
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll atlas color readback");
    let mapped = slice.get_mapped_range().expect("mapped atlas color readback");
    let mut result = Vec::with_capacity((width * height * BYTES_PER_PIXEL) as usize);
    for y in 0..height as usize {
        let start = y * stride as usize;
        result.extend_from_slice(&mapped[start..start + (width * BYTES_PER_PIXEL) as usize]);
    }
    drop(mapped);
    readback.unmap();
    result
}

/// Render adjacent synthetic glyph tiles through the real unified pipeline and return BGRA pixels.
pub(super) fn render_glyph_readback(tiles: &[sonicterm_text::glyph_atlas::RasterTile]) -> Vec<u8> {
    render_glyph_readback_on(tiles, [0.0; 4])
}

/// Render color or coverage glyph tiles over one linear clear color for cross-presenter tests.
pub(crate) fn render_glyph_readback_on(
    tiles: &[sonicterm_text::glyph_atlas::RasterTile],
    clear: [f32; 4],
) -> Vec<u8> {
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
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0] as f64,
                        g: clear[1] as f64,
                        b: clear[2] as f64,
                        a: clear[3] as f64,
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
            width as f32,
            1.0,
            sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::Off,
            None,
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

pub(super) struct TestTileRasterizer(pub(super) sonicterm_text::glyph_atlas::RasterTile);

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
pub(super) fn gpu_mixed_glyphs_select_distinct_texture_views() {
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
pub(super) fn gpu_translucent_color_glyph_blends_as_premultiplied_linear() {
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

/// One atlas allocation supplies unorm coverage and sRGB color views without retaining a second payload.
#[test]
pub(super) fn one_texture_exposes_coverage_and_color_bind_groups() {
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
pub(super) fn image_color_filtering_does_not_define_the_srgb_view_policy() {
    let coverage = atlas_sampler_descriptor();
    let image = image_atlas_sampler_descriptor();

    assert_eq!(coverage.mag_filter, wgpu::FilterMode::Nearest);
    assert_eq!(coverage.min_filter, wgpu::FilterMode::Nearest);
    assert_eq!(image.mag_filter, wgpu::FilterMode::Linear);
    assert_eq!(image.min_filter, wgpu::FilterMode::Linear);
}

/// The completed glyph group exposes both views with nearest filtering from one texture.
#[test]
pub(super) fn glyph_bind_group_combines_coverage_and_color_views() {
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
pub(super) fn gpu_srgb_view_preserves_opaque_midtone() {
    let output = render_image_readback(1, &[128, 128, 128, 255], 1);

    for (actual, expected) in output.iter().zip([128, 128, 128, 255]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected={expected}");
    }
}

/// A translucent encoded-premultiplied image pixel is linearized before GPU source-over.
#[test]
pub(super) fn gpu_srgb_view_converts_translucent_encoded_premultiplication() {
    let output = render_image_readback(1, &[64, 64, 64, 128], 1);

    for (actual, expected) in output.iter().zip([92, 92, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
}

/// Linear filtering must not pull pixels from the next packed image tile.
#[test]
pub(super) fn gpu_scaled_image_clamps_to_its_own_atlas_tile() {
    let output =
        render_image_readback_with_neighbor(1, &[255, 255, 255, 255], 3, Some([0, 0, 255, 255]));

    for pixel in output.as_chunks::<4>().0 {
        assert_eq!(*pixel, [255, 255, 255, 255]);
    }
}

/// Scaled images interpolate decoded linear samples, not their encoded sRGB byte values.
#[test]
pub(super) fn gpu_scaled_image_filters_in_linear_light() {
    let output = render_image_readback(2, &[0, 0, 0, 255, 255, 255, 255, 255], 3);
    let middle = &output[4..8];

    for &actual in &middle[..3] {
        assert!(actual.abs_diff(188) <= 2, "linear midpoint should encode near 188: {middle:?}");
    }
    assert_eq!(middle[3], 255);
}

/// A `side × side` tile filled with `value`: one coverage byte per pixel, or BGRA when `is_color`.
pub(super) fn list_tile(
    side: u32,
    is_color: bool,
    value: [u8; 4],
) -> sonicterm_text::glyph_atlas::RasterTile {
    let pixels = (side * side) as usize;
    let coverage = if is_color { value.repeat(pixels) } else { vec![value[3]; pixels] };
    sonicterm_text::glyph_atlas::RasterTile {
        width: side,
        height: side,
        offset_x: 0,
        offset_y: 0,
        advance: side as f32,
        coverage,
        is_color,
        is_subpixel: false,
    }
}

/// A distinct key per `index`, from the CJK block so thousands stay valid chars.
pub(super) fn list_key(index: u32) -> sonicterm_types::GlyphKey {
    sonicterm_types::GlyphKey::new(char::from_u32(0x4E00 + index).unwrap(), false, false)
}

/// Every staged write of one sync through fresh lists, in order.
pub(super) fn staged_writes(atlas: &mut GlyphAtlas) -> Vec<(DirtyRect, Vec<u8>)> {
    let mut writes = Vec::new();
    let _stats = UploadLists::default()
        .sync_with(atlas, |rect, staged| writes.push((rect, staged.to_vec())));
    writes
}

/// The envelope `class` restates for one owner.
pub(super) fn envelope_bytes(class: sonicterm_types::ResourceClass) -> usize {
    match class.coverage() {
        sonicterm_types::ClassCoverage::UnchargedRetention { per_owner_bytes } => per_owner_bytes,
        other => panic!("{class:?} is not an uncharged retention: {other:?}"),
    }
}

/// Bytes `upload` keeps between syncs: both rect lists and the staging buffer each write is
/// copied through.
pub(super) fn upload_retained_bytes(upload: &AtlasUpload) -> usize {
    (upload.lists.dirty_rects.capacity() + upload.lists.coalesced_rects.capacity())
        * std::mem::size_of::<DirtyRect>()
        + upload.lists.scratch.capacity()
}

/// One megabyte, the shared scratch rule's strict floor.
pub(super) const MIB: usize = 1024 * 1024;
/// A 768x768 coverage tile staged as BGRA: 2,359,296 bytes, 2.25 MiB.
pub(super) const LARGE_TILE_BYTES: usize = 768 * 768 * 4;
/// A 64x64 coverage tile staged as BGRA: 16,384 bytes.
pub(super) const SMALL_TILE_BYTES: usize = 64 * 64 * 4;

/// Sync `atlas` through `lists`, recording every staged slice's length in order.
pub(super) fn recorded_sync(lists: &mut UploadLists, atlas: &mut GlyphAtlas) -> Vec<usize> {
    let mut lengths = Vec::new();
    let _stats = lists.sync_with(atlas, |_, staged| lengths.push(staged.len()));
    lengths
}

/// Insert one coverage tile `side` pixels square under the distinct key `index`.
pub(super) fn insert_coverage(atlas: &mut GlyphAtlas, index: u32, side: u32) {
    let tile = list_tile(side, false, [0, 0, 0, 200]);
    assert!(atlas.get_or_insert(list_key(index), &mut TestTileRasterizer(tile)).is_some());
}

/// A growable atlas holding one 768x768 tile, and lists that have synced it once: the largest
/// slice is the whole tile and the staging buffer is over 1 MiB.
pub(super) fn after_large_sync() -> (UploadLists, GlyphAtlas) {
    let mut atlas = GlyphAtlas::growable(1024, ATLAS_DIM);
    insert_coverage(&mut atlas, 0, 768);
    let mut lists = UploadLists::default();
    let lengths = recorded_sync(&mut lists, &mut atlas);
    assert_eq!(lengths.iter().max(), Some(&LARGE_TILE_BYTES), "precondition: the large write");
    assert!(lengths.iter().max().is_some_and(|largest| *largest > MIB));
    assert!(lists.scratch.capacity() > MIB, "precondition: staging over 1 MiB");
    (lists, atlas)
}
