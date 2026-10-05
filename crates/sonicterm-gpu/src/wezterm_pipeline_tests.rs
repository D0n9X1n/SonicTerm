use super::*;

/// Line geometry must not overwrite the color transform carried by each vertex.
#[test]
fn line_vertices_keep_geometry_out_of_hsv() {
    let color = [0.0, 1.0, 1.0, 1.0];
    let line_a = [-5.0, -2.0];
    let line_b = [7.0, 3.0];
    let thickness = 2.5;
    let line = QuadInstance::line(
        crate::quad::px_to_ndc(2.0, 3.0, 16.0, 10.0, 32.0, 24.0),
        color,
        [16.0, 10.0],
        line_a,
        line_b,
        thickness,
    );
    let mut vertices = Vec::new();

    push_quad_instances(&mut vertices, &[line], 32.0, 24.0);

    assert_eq!(vertices.len(), VERTICES_PER_QUAD);
    for vertex in vertices {
        assert_eq!(vertex.has_color, IS_LINE);
        assert_eq!(vertex.alt_color, [line_a[0], line_a[1], line_b[0], line_b[1]]);
        assert_eq!(vertex.mix_value, thickness);
        assert_eq!(vertex.hsv, [1.0, 1.0, 1.0]);
        assert_eq!(vertex.fg_color, color);
    }
}

/// Moving line geometry out of HSV must leave the ordinary glyph transform contract intact.
#[test]
fn glyph_vertices_retain_foreground_hsv_contract() {
    let color = [0.2, 0.3, 0.4, 1.0];
    let glyph = GlyphInstance {
        rect: crate::quad::px_to_ndc(1.0, 2.0, 4.0, 6.0, 16.0, 12.0),
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
        flags: [0.0; 4],
    };
    let mut vertices = Vec::new();

    push_glyph_instances(
        &mut vertices,
        &[glyph],
        16.0,
        12.0,
        sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::Off,
    );

    assert_eq!(vertices.len(), VERTICES_PER_QUAD);
    for vertex in vertices {
        assert_eq!(vertex.has_color, IS_GLYPH);
        assert_eq!(vertex.hsv, [1.0, 1.0, 1.0]);
        assert_eq!(vertex.fg_color, color);
    }
    assert!(SHADER.contains("hsv *= uniforms.foreground_text_hsb;"));
}

/// LCD policy reclassifies only ordinary subpixel glyphs and preserves higher-priority kinds.
#[test]
fn subpixel_vertex_kind_respects_mode_and_primitive_precedence() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::{Bgr, Off, Rgb};

    let instance = |flags| GlyphInstance {
        rect: crate::quad::px_to_ndc(1.0, 2.0, 4.0, 6.0, 16.0, 12.0),
        uv: [0.0, 0.0, 1.0, 1.0],
        color: [0.2, 0.3, 0.4, 1.0],
        flags,
    };
    let kind = |glyph: GlyphInstance, mode| {
        let mut vertices = Vec::new();
        push_glyph_instances(&mut vertices, &[glyph], 16.0, 12.0, mode);
        vertices[0].has_color
    };

    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Off), IS_GLYPH);
    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Rgb), IS_SUBPIXEL_RGB);
    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Bgr), IS_SUBPIXEL_BGR);
    assert_eq!(kind(instance([1.0, 1.0, 0.0, 0.0]), Rgb), IS_COLOR_EMOJI);
    let image = ImageInstance {
        rect_px: [1.0, 2.0, 4.0, 6.0],
        uv: [0.25, 0.25, 0.5, 0.75],
        sample_uv: [0.0, 0.0, 1.0, 1.0],
    };
    let mut vertices = Vec::new();
    push_image_instances(&mut vertices, &[image], 16.0, 12.0);
    assert!(vertices.iter().all(|vertex| vertex.has_color == IS_IMAGE));
    assert!(vertices.iter().all(|vertex| vertex.alt_color == image.sample_uv));
    assert_eq!(vertices[0].tex, [0.25, 0.25]);
    assert_eq!(vertices[3].tex, [0.5, 0.75]);
}

/// The dual-source shader carries independent source color and destination attenuation factors.
#[test]
fn dual_source_shader_declares_two_blend_sources_and_subpixel_weights() {
    let shader = shader_source(true);

    assert!(shader.contains("enable dual_source_blending;"));
    assert!(shader.contains("@location(0) @blend_src(0) color"));
    assert!(shader.contains("@location(0) @blend_src(1) factor"));
    assert!(shader.contains("coverage * transformed_foreground.a"));
    assert!(shader.contains("transformed_foreground.rgb * coverage"));
}

/// Primitive kind is categorical state and must never be perspective-interpolated.
#[test]
fn primitive_kind_uses_flat_interpolation() {
    assert!(SHADER.contains("@location(4) @interpolate(flat) has_color: f32,"));
}

/// Line antialiasing must keep smoothstep edges ordered when derivatives are zero.
#[test]
fn line_antialiasing_has_a_positive_derivative_floor() {
    assert!(SHADER.contains("let w = max(fwidth(d), 1.0e-4);"));
}

/// Render one synthetic atlas glyph through either standard or dual-source presentation.
#[cfg(target_os = "windows")]
fn render_warp_glyph(
    coverage: [u8; 4],
    is_subpixel: bool,
    mode: SubpixelAaMode,
    foreground: [f32; 4],
    background: wgpu::Color,
) -> Option<[u8; 4]> {
    const BYTES_PER_ROW: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .ok()?;
    let optional = crate::core::selected_optional_device_features(adapter.features(), true);
    if mode != SubpixelAaMode::Off && !optional.contains(wgpu::Features::DUAL_SOURCE_BLENDING) {
        return None;
    }
    let (device, queue) = pollster::block_on(
        adapter.request_device(&crate::core::device_descriptor_for(true, optional)),
    )
    .ok()?;
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 1);
    let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let info = atlas.get_or_insert(
        sonicterm_types::GlyphKey::new('L', false, false),
        &mut CapabilityGlyph {
            tile: sonicterm_text::glyph_atlas::RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: coverage.to_vec(),
                is_color: false,
                is_subpixel,
            },
        },
    )?;
    let image_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.image_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Image,
    );
    let mut glyph_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.glyph_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Glyph,
    );
    glyph_upload.sync(&queue, &mut atlas);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("LCD capability target"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("LCD capability readback"),
        size: u64::from(BYTES_PER_ROW),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let glyph = GlyphInstance {
        rect: crate::quad::px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
        uv: info.uv,
        color: foreground,
        flags: [0.0, f32::from(is_subpixel), 0.0, 0.0],
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("LCD capability pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(background),
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
            1.0,
            1.0,
            mode,
            None,
            &[],
            &[],
            &[glyph],
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
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    let mapped = slice.get_mapped_range().ok()?;
    let pixel = [mapped[0], mapped[1], mapped[2], mapped[3]];
    drop(mapped);
    readback.unmap();
    Some(pixel)
}

/// Render one synthetic atlas glyph through the Windows software presenter.
#[cfg(target_os = "windows")]
fn render_software_glyph(
    coverage: [u8; 4],
    is_subpixel: bool,
    mode: SubpixelAaMode,
    foreground: [f32; 4],
    background: [f32; 4],
) -> [u8; 4] {
    let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let info = atlas
        .get_or_insert(
            sonicterm_types::GlyphKey::new('L', false, false),
            &mut CapabilityGlyph {
                tile: sonicterm_text::glyph_atlas::RasterTile {
                    width: 1,
                    height: 1,
                    offset_x: 0,
                    offset_y: 0,
                    advance: 1.0,
                    coverage: coverage.to_vec(),
                    is_color: false,
                    is_subpixel,
                },
            },
        )
        .expect("software capability glyph");
    let glyph = GlyphInstance {
        rect: crate::quad::px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
        uv: info.uv,
        color: foreground,
        flags: [0.0, f32::from(is_subpixel), 0.0, 0.0],
    };
    let mut frame = crate::software_frame::SoftwareFrame::new(1, 1, background)
        .expect("software capability frame");
    frame.draw_layers_with_subpixel_aa(&atlas, &atlas, mode, &[], &[], &[glyph], &[], &[]);
    frame.pixel_bgra_at(0, 0).expect("software capability pixel")
}

#[cfg(target_os = "windows")]
struct CapabilityGlyph {
    tile: sonicterm_text::glyph_atlas::RasterTile,
}

#[cfg(target_os = "windows")]
impl sonicterm_text::glyph_atlas::Rasterizer for CapabilityGlyph {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(self.tile.clone())
    }
}

/// WARP exercises RGB/BGR dual-source blending when advertised or proves grayscale fallback.
#[cfg(target_os = "windows")]
#[test]
fn warp_subpixel_capability_is_explicit_and_ordinary_output_is_stable() {
    let foreground = [1.0, 1.0, 1.0, 1.0];
    let black = wgpu::Color::BLACK;
    let grayscale =
        render_warp_glyph([0, 128, 255, 255], true, SubpixelAaMode::Off, foreground, black)
            .expect("WARP grayscale baseline");
    let rgb = render_warp_glyph([0, 128, 255, 255], true, SubpixelAaMode::Rgb, foreground, black);
    let Some(rgb) = rgb else {
        assert_eq!(grayscale, [255, 255, 255, 255]);
        println!("capability=HOST_INCAPABLE fallback=grayscale");
        return;
    };
    let bgr = render_warp_glyph([0, 128, 255, 255], true, SubpixelAaMode::Bgr, foreground, black)
        .expect("BGR uses the same dual-source capability");
    let ordinary_standard =
        render_warp_glyph([128; 4], false, SubpixelAaMode::Off, foreground, black)
            .expect("ordinary standard output");
    let ordinary_dual = render_warp_glyph([128; 4], false, SubpixelAaMode::Rgb, foreground, black)
        .expect("ordinary dual-source output");
    let colored_background = crate::color::hex_to_wgpu("#204080");
    let translucent_foreground = crate::color::hex_to_premultiplied_rgba("#e0a040", 0.5);
    let translucent = render_warp_glyph(
        [0, 128, 255, 255],
        true,
        SubpixelAaMode::Rgb,
        translucent_foreground,
        colored_background,
    )
    .expect("translucent LCD output");

    assert_eq!(rgb, [0, 188, 255, 255]);
    assert_eq!(bgr, [255, 188, 0, 255]);
    assert_eq!(translucent, [128, 100, 166, 255]);
    assert_eq!(ordinary_dual, ordinary_standard);

    let black_linear = [0.0, 0.0, 0.0, 1.0];
    assert_eq!(
        render_software_glyph(
            [0, 128, 255, 255],
            true,
            SubpixelAaMode::Off,
            foreground,
            black_linear,
        ),
        grayscale,
    );
    assert_eq!(
        render_software_glyph(
            [0, 128, 255, 255],
            true,
            SubpixelAaMode::Rgb,
            foreground,
            black_linear,
        ),
        rgb,
    );
    assert_eq!(
        render_software_glyph(
            [0, 128, 255, 255],
            true,
            SubpixelAaMode::Bgr,
            foreground,
            black_linear,
        ),
        bgr,
    );
    assert_eq!(
        render_software_glyph(
            [0, 128, 255, 255],
            true,
            SubpixelAaMode::Rgb,
            translucent_foreground,
            [
                colored_background.r as f32,
                colored_background.g as f32,
                colored_background.b as f32,
                colored_background.a as f32,
            ],
        ),
        translucent,
    );
    println!("capability=EXERCISED rgb={rgb:?} bgr={bgr:?} translucent={translucent:?}");
}

/// Reconstruct the same padded local geometry used by curly-underline line segments.
#[cfg(target_os = "windows")]
fn line_segment(
    surface: [f32; 2],
    start: [f32; 2],
    end: [f32; 2],
    thickness: f32,
    color: [f32; 4],
) -> QuadInstance {
    let pad = thickness * 0.5 + 1.0;
    let x0 = start[0].min(end[0]) - pad;
    let y0 = start[1].min(end[1]) - pad;
    let x1 = start[0].max(end[0]) + pad;
    let y1 = start[1].max(end[1]) + pad;
    let size = [(x1 - x0).max(1.0), (y1 - y0).max(1.0)];
    let center = [x0 + size[0] * 0.5, y0 + size[1] * 0.5];
    QuadInstance::line(
        crate::quad::px_to_ndc(x0, y0, size[0], size[1], surface[0], surface[1]),
        color,
        size,
        [start[0] - center[0], start[1] - center[1]],
        [end[0] - center[0], end[1] - center[1]],
        thickness,
    )
}

/// GPU and software presentation must agree for horizontal, diagonal, and alternating segments.
#[cfg(target_os = "windows")]
#[test]
fn warp_line_colors_match_software_across_segment_shapes() {
    const WIDTH: u32 = 32;
    const HEIGHT: u32 = 20;
    const BYTES_PER_ROW: u32 = 256;
    let surface = [WIDTH as f32, HEIGHT as f32];
    let cyan = [0.0, 1.0, 1.0, 1.0];
    let magenta = [1.0, 0.0, 1.0, 1.0];
    let orange = [1.0, 0.215_860_5, 0.0, 1.0];
    let base_control = QuadInstance::sharp(
        crate::quad::px_to_ndc(28.0, 0.0, 4.0, 4.0, surface[0], surface[1]),
        [0.0, 1.0, 0.0, 1.0],
    );
    let overlay_control = QuadInstance::sharp(
        crate::quad::px_to_ndc(28.0, 6.0, 4.0, 4.0, surface[0], surface[1]),
        [1.0, 1.0, 0.0, 1.0],
    );
    let lines = [
        line_segment(surface, [2.0, 3.0], [10.0, 3.0], 4.0, cyan),
        line_segment(surface, [2.0, 9.0], [8.0, 15.0], 4.0, magenta),
        line_segment(surface, [14.0, 16.0], [20.0, 10.0], 4.0, orange),
        line_segment(surface, [20.0, 10.0], [26.0, 16.0], 4.0, orange),
    ];
    let base_quads = [base_control, lines[0]];
    let overlay_quads = [lines[1], lines[2], lines[3], overlay_control];
    let samples = [
        ([30_u32, 2_u32], [0, 255, 0, 255]),
        ([30, 8], [0, 255, 255, 255]),
        ([6, 3], [255, 255, 0, 255]),
        ([4, 11], [255, 0, 255, 255]),
        ([17, 12], [0, 128, 255, 255]),
        ([23, 13], [0, 128, 255, 255]),
        ([2, 17], [0, 0, 0, 255]),
    ];

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .expect("Windows WARP fallback adapter");
    let (device, queue) = pollster::block_on(
        adapter.request_device(&crate::core::device_descriptor_for(true, wgpu::Features::empty())),
    )
    .expect("WARP device");
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 4);
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
        label: Some("line color parity target"),
        size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("line color parity readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("line color parity pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
            surface[0],
            surface[1],
            SubpixelAaMode::Off,
            None,
            &base_quads,
            &[],
            &[],
            &overlay_quads,
            &[],
        );
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll WARP line readback");
    let bytes = slice.get_mapped_range().expect("mapped WARP line readback");

    let mut software =
        crate::software_frame::SoftwareFrame::new(WIDTH, HEIGHT, [0.0, 0.0, 0.0, 1.0])
            .expect("valid software frame");
    software.draw_layers(&atlas, &atlas, &base_quads, &[], &[], &overlay_quads, &[]);

    for (point, expected) in samples {
        let offset = point[1] as usize * BYTES_PER_ROW as usize + point[0] as usize * 4;
        let gpu = [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]];
        let cpu = software.pixel_bgra_at(point[0], point[1]).expect("software sample pixel");
        for channel in 0..4 {
            assert!(
                gpu[channel].abs_diff(cpu[channel]) <= 2,
                "sample {point:?} channel {channel} differs: GPU {gpu:?}, software {cpu:?}"
            );
            assert!(
                cpu[channel].abs_diff(expected[channel]) <= 2,
                "sample {point:?} channel {channel} has unexpected color {cpu:?}"
            );
        }
    }
}

/// WARP and software encode every named translucent quad producer identically.
#[cfg(target_os = "windows")]
#[test]
fn warp_named_quad_producers_match_software_linear_blend() {
    const WIDTH: u32 = 32;
    const HEIGHT: u32 = 12;
    const BYTES_PER_ROW: u32 = 256;
    let surface = [WIDTH as f32, HEIGHT as f32];
    let background = crate::color::hex_to_premultiplied_rgba("#123456", 1.0);
    let opaque = crate::color::hex_to_premultiplied_rgba("#e04020", 1.0);
    let selection = crate::color::hex_to_premultiplied_rgba("#e04020", 0.5);
    let quads = [
        QuadInstance::sharp(
            crate::quad::px_to_ndc(0.0, 0.0, 4.0, 4.0, surface[0], surface[1]),
            selection,
        ),
        QuadInstance::rounded(
            crate::quad::px_to_ndc(5.0, 0.0, 6.0, 6.0, surface[0], surface[1]),
            selection,
            [6.0, 6.0],
            2.0,
        ),
        line_segment(surface, [4.0, 9.0], [12.0, 9.0], 4.0, selection),
        QuadInstance::sharp(
            crate::quad::px_to_ndc(13.0, 0.0, 3.0, 4.0, surface[0], surface[1]),
            crate::color::hex_to_premultiplied_rgba("#e04020", 0.9),
        ),
        QuadInstance::sharp(
            crate::quad::px_to_ndc(17.0, 0.0, 3.0, 4.0, surface[0], surface[1]),
            crate::quad::with_premultiplied_alpha(opaque, 0.55),
        ),
        QuadInstance::sharp(
            crate::quad::px_to_ndc(21.0, 0.0, 3.0, 4.0, surface[0], surface[1]),
            crate::quad::with_premultiplied_alpha(opaque, 0.18),
        ),
        QuadInstance::sharp(
            crate::quad::px_to_ndc(25.0, 0.0, 3.0, 4.0, surface[0], surface[1]),
            crate::quad::with_premultiplied_alpha(opaque, 0.5),
        ),
    ];
    let samples = [
        ("selection sharp", [1_u32, 1_u32], [66, 58, 165, 255]),
        ("selection rounded", [8, 3], [66, 58, 165, 255]),
        ("selection line", [8, 9], [66, 58, 165, 255]),
        ("URL hover", [14, 1], [41, 63, 214, 255]),
        ("tofu", [18, 1], [63, 59, 172, 255]),
        ("tab dimming", [22, 1], [79, 54, 104, 255]),
        ("drag-chip body", [26, 1], [66, 58, 165, 255]),
    ];

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .expect("Windows WARP fallback adapter");
    let (device, queue) = pollster::block_on(
        adapter.request_device(&crate::core::device_descriptor_for(true, wgpu::Features::empty())),
    )
    .expect("WARP device");
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 1);
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
        label: Some("named quad producer parity target"),
        size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("named quad producer parity readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("named quad producer parity pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(background[0]),
                        g: f64::from(background[1]),
                        b: f64::from(background[2]),
                        a: f64::from(background[3]),
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
            surface[0],
            surface[1],
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
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll named-producer WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped named-producer WARP readback");
    let mut software = crate::software_frame::SoftwareFrame::new(
        WIDTH,
        HEIGHT,
        [background[0], background[1], background[2], background[3]],
    )
    .expect("valid software frame");
    software.draw_layers(&atlas, &atlas, &quads, &[], &[], &[], &[]);

    for (name, point, expected) in samples {
        let offset = point[1] as usize * BYTES_PER_ROW as usize + point[0] as usize * 4;
        let gpu = [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]];
        let cpu = software.pixel_bgra_at(point[0], point[1]).expect("software sample pixel");
        assert_eq!(cpu, expected, "{name}");
        for channel in 0..4 {
            assert!(
                gpu[channel].abs_diff(cpu[channel]) <= 1,
                "{name} channel {channel} differs: GPU {gpu:?}, software {cpu:?}"
            );
        }
    }
}

/// A headless device and queue on the host's default adapter (WARP on Windows).
fn headless_device_and_queue() -> (wgpu::Device, wgpu::Queue) {
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

fn uniform_test_device() -> wgpu::Device {
    headless_device_and_queue().0
}

/// An invalid uniform-buffer descriptor surfaces as a contained validation
/// error instead of a panic. `create_buffer_init` maps the new buffer and
/// panics through `expect` when that buffer is invalid.
#[test]
fn invalid_uniform_buffer_is_a_contained_error() {
    let device = uniform_test_device();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _buffer = create_uniform_buffer(&device, wgpu::BufferUsages::empty());
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");
}

/// Width of the frame-harness target in pixels; one quad covers one column.
const HARNESS_WIDTH: u32 = 8;
/// Height of the frame-harness target in pixels.
const HARNESS_HEIGHT: u32 = 2;
/// Padded readback row stride for the harness target.
const HARNESS_STRIDE: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
/// Opaque red in linear space, drawn by every harness quad.
const HARNESS_RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

/// One pipeline on a headless device, drawing solid quads into a small cleared target.
struct FrameHarness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: WeztermPipeline,
    image_upload: crate::atlas_upload::AtlasUpload,
    glyph_upload: crate::atlas_upload::AtlasUpload,
}

/// What one harness frame produced: the columns drawn red and the counted buffer writes.
struct HarnessFrame {
    red_columns: Vec<u32>,
    stats: crate::frame_stats::FrameStats,
}

impl FrameHarness {
    /// Build a harness whose pipeline starts with room for `initial_quads` quads.
    fn new(initial_quads: u64) -> Self {
        let (device, queue) = headless_device_and_queue();
        let pipeline =
            WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm, initial_quads);
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
        Self { device, queue, pipeline, image_upload, glyph_upload }
    }

    /// Draw one frame on a black target under a counting scope and read it back.
    fn draw(
        &mut self,
        reset: Option<QuadInstance>,
        quads: &[QuadInstance],
        overlay_quads: &[QuadInstance],
    ) -> HarnessFrame {
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame harness target"),
            size: wgpu::Extent3d {
                width: HARNESS_WIDTH,
                height: HARNESS_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame harness readback"),
            size: u64::from(HARNESS_STRIDE) * u64::from(HARNESS_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let sink = crate::frame_stats::FrameStatsSink::default();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let _counting = crate::frame_stats::CollectGuard::enter(Some(&sink));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame harness pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.pipeline.draw_frame(
                &self.device,
                &self.queue,
                &mut pass,
                self.image_upload.image_bind_group(),
                self.glyph_upload.glyph_bind_group(),
                HARNESS_WIDTH as f32,
                HARNESS_HEIGHT as f32,
                SubpixelAaMode::Off,
                reset,
                quads,
                &[],
                &[],
                overlay_quads,
                &[],
            );
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(HARNESS_STRIDE),
                    rows_per_image: Some(HARNESS_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: HARNESS_WIDTH,
                height: HARNESS_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll harness readback");
        let mapped = slice.get_mapped_range().expect("mapped harness readback");
        // BGRA: a red pixel has its red byte at offset 2 and no green or blue.
        let red_columns = (0..HARNESS_WIDTH)
            .filter(|column| {
                let offset = *column as usize * 4;
                mapped[offset + 2] > 200 && mapped[offset] < 50 && mapped[offset + 1] < 50
            })
            .collect();
        drop(mapped);
        readback.unmap();
        HarnessFrame { red_columns, stats: sink.snapshot() }
    }

    /// Read the whole index buffer back as indices.
    fn index_buffer(&self) -> Vec<u32> {
        let size = self.pipeline.index_capacity * std::mem::size_of::<u32>() as u64;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("index harness readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.pipeline.index_buf, 0, &readback, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll index readback");
        let mapped = slice.get_mapped_range().expect("mapped index readback");
        let indices = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
        drop(mapped);
        readback.unmap();
        indices
    }
}

/// A red quad covering pixel column `column` of the harness target.
fn red_column(column: u32) -> QuadInstance {
    QuadInstance::sharp(
        crate::quad::px_to_ndc(
            column as f32,
            0.0,
            1.0,
            HARNESS_HEIGHT as f32,
            HARNESS_WIDTH as f32,
            HARNESS_HEIGHT as f32,
        ),
        HARNESS_RED,
    )
}

/// A zero-width quad, which emits no vertices.
fn degenerate_quad() -> QuadInstance {
    QuadInstance::sharp(
        crate::quad::px_to_ndc(0.0, 0.0, 0.0, 1.0, HARNESS_WIDTH as f32, HARNESS_HEIGHT as f32),
        HARNESS_RED,
    )
}

/// A black quad over the whole target, drawn through the replace-blend reset pipeline.
fn black_reset() -> QuadInstance {
    QuadInstance::sharp(
        crate::quad::px_to_ndc(
            0.0,
            0.0,
            HARNESS_WIDTH as f32,
            HARNESS_HEIGHT as f32,
            HARNESS_WIDTH as f32,
            HARNESS_HEIGHT as f32,
        ),
        [0.0, 0.0, 0.0, 1.0],
    )
}

/// Bytes of the index pattern for the pipeline's whole index capacity.
fn pattern_bytes(index_capacity: u64) -> u64 {
    index_capacity * std::mem::size_of::<u32>() as u64
}

/// The index pattern is uploaded on the first frame, once and for the whole capacity, and
/// draws the right quads; a second frame at the same capacity writes no index bytes.
#[test]
fn first_frame_uploads_the_index_pattern_once_and_the_next_uploads_nothing() {
    let mut harness = FrameHarness::new(8);

    let first = harness.draw(None, &[red_column(1), red_column(4)], &[]);
    assert_eq!(first.red_columns, vec![1, 4]);
    assert_eq!(first.stats.index_bytes, pattern_bytes(harness.pipeline.index_capacity));
    assert_eq!(
        harness.index_buffer(),
        build_indices((harness.pipeline.index_capacity / INDICES_PER_QUAD as u64) as usize),
        "the buffer holds the pattern for its whole quad capacity"
    );

    let second = harness.draw(None, &[red_column(2), red_column(5), red_column(6)], &[]);
    assert_eq!(second.red_columns, vec![2, 5, 6]);
    assert_eq!(second.stats.index_bytes, 0, "an unchanged capacity writes no index bytes");
}

/// Growth re-uploads the pattern for the new capacity, and a smaller frame afterwards draws
/// from the larger pattern's prefix without another upload.
#[test]
fn growth_uploads_the_larger_pattern_and_smaller_frames_draw_from_its_prefix() {
    let mut harness = FrameHarness::new(1);
    let small = harness.draw(None, &[red_column(0)], &[]);
    assert_eq!(small.red_columns, vec![0]);

    let wide: Vec<QuadInstance> = (0..5).map(red_column).collect();
    let grown = harness.draw(None, &wide, &[]);
    let new_index_capacity = harness.pipeline.index_capacity;
    assert!(new_index_capacity >= 5 * INDICES_PER_QUAD as u64, "the index buffer grew");
    assert_eq!(grown.red_columns, vec![0, 1, 2, 3, 4]);
    assert_eq!(grown.stats.index_bytes, pattern_bytes(new_index_capacity));
    assert_eq!(
        harness.index_buffer(),
        build_indices((new_index_capacity / INDICES_PER_QUAD as u64) as usize)
    );

    let after = harness.draw(None, &[red_column(7), red_column(3)], &[]);
    assert_eq!(after.red_columns, vec![3, 7]);
    assert_eq!(after.stats.index_bytes, 0);
}

/// At an unchanged capacity no frame writes index bytes, with or without a reset and with
/// overlay quads, and every frame still draws exactly its own quads.
#[test]
fn frames_at_unchanged_capacity_write_no_index_bytes() {
    let mut harness = FrameHarness::new(8);
    harness.draw(None, &[red_column(0)], &[]);
    let frames: [(Option<QuadInstance>, Vec<QuadInstance>, Vec<QuadInstance>, Vec<u32>); 3] = [
        (Some(black_reset()), vec![red_column(2)], vec![red_column(6)], vec![2, 6]),
        (None, vec![red_column(1), red_column(3), red_column(5)], vec![], vec![1, 3, 5]),
        (Some(black_reset()), vec![], vec![red_column(7)], vec![7]),
    ];
    for (reset, quads, overlay, expected) in frames {
        let frame = harness.draw(reset, &quads, &overlay);
        assert_eq!(frame.red_columns, expected);
        assert_eq!(frame.stats.index_bytes, 0);
        assert!(frame.stats.vertex_bytes > 0, "vertices are still uploaded every frame");
    }
}

/// Draw ranges come from the vertices actually emitted: a skipped degenerate primitive
/// shortens the range, with or without a reset, and with overlays.
#[test]
fn draw_ranges_follow_emitted_vertices() {
    let surface = (HARNESS_WIDTH as f32, HARNESS_HEIGHT as f32);
    let quads = [red_column(0), degenerate_quad(), red_column(2)];
    let overlay = [degenerate_quad(), red_column(5)];
    let layers = PipelineLayers {
        quads: &quads,
        images: &[],
        glyphs: &[],
        overlay_quads: &overlay,
        overlay_glyphs: &[],
    };
    let cases =
        [(None, 0_u32, 18_u32), (Some(black_reset()), 6, 24), (Some(degenerate_quad()), 0, 18)];
    for (reset, reset_end, main_end) in cases {
        let mut vertices = Vec::new();
        let reset_vertices = assemble_vertices(
            &mut vertices,
            reset.as_ref(),
            &layers,
            surface.0,
            surface.1,
            SubpixelAaMode::Off,
        );
        let (reset_range, main_range) = draw_index_ranges(reset_vertices, vertices.len());
        assert_eq!(reset_range, 0..reset_end);
        assert_eq!(main_range, reset_end..main_end);
    }
}

/// A frame whose degenerate primitives are skipped draws only its own vertices: a range
/// sized from the input slices would reach the previous frame's stale vertices.
#[test]
fn skipped_primitives_never_draw_stale_vertices() {
    let mut harness = FrameHarness::new(8);
    let earlier: Vec<QuadInstance> = (0..4).map(red_column).collect();
    assert_eq!(harness.draw(None, &earlier, &[]).red_columns, vec![0, 1, 2, 3]);

    let frame = harness.draw(None, &[degenerate_quad(), red_column(5), degenerate_quad()], &[]);

    assert_eq!(frame.red_columns, vec![5]);
}

/// An invalid index-buffer descriptor, at creation or at growth, is a contained wgpu
/// validation error rather than a panic.
#[test]
fn invalid_index_buffer_is_a_contained_error() {
    let (device, _queue) = headless_device_and_queue();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _buffer = create_index_buffer(&device, 6, wgpu::BufferUsages::empty());
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");

    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm, 1);
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let oversized = device.limits().max_buffer_size;
    pipeline.ensure_capacity(&device, 4, oversized);
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");
}

/// The vertex scratch is reused across frames without carrying the previous frame's
/// vertices, and grows when a frame needs more room.
#[test]
fn vertex_scratch_is_reused_without_stale_vertices_and_grows() {
    let mut harness = FrameHarness::new(8);
    harness.draw(None, &[red_column(0), red_column(1), red_column(2)], &[]);
    let reused = harness.pipeline.vertex_scratch.as_ptr();
    let capacity = harness.pipeline.vertex_scratch.capacity();
    assert!(capacity >= 3 * VERTICES_PER_QUAD, "the scratch holds the first frame");

    let frame = harness.draw(None, &[red_column(6)], &[]);
    assert_eq!(frame.red_columns, vec![6]);
    assert_eq!(harness.pipeline.vertex_scratch.len(), VERTICES_PER_QUAD, "no stale vertices");
    assert_eq!(harness.pipeline.vertex_scratch.as_ptr(), reused, "the allocation is reused");

    let wide: Vec<QuadInstance> = (0..8).map(red_column).collect();
    harness.draw(None, &wide, &[]);
    assert!(harness.pipeline.vertex_scratch.capacity() >= 8 * VERTICES_PER_QUAD);
}

/// The scratch is shrunk to twice a frame's use once its capacity exceeds four times that
/// use and 1 MiB, and kept otherwise; its capacity is the retained figure.
#[test]
fn vertex_scratch_release_policy_and_retained_figure() {
    let vertex_size = std::mem::size_of::<Vertex>();
    let large = (2 * 1024 * 1024) / vertex_size;
    let mut scratch: Vec<Vertex> = Vec::with_capacity(large);
    scratch.resize(100, Vertex::default());
    release_scratch_excess(&mut scratch, 100);
    assert!(scratch.capacity() >= 200 && scratch.capacity() < large, "oversized scratch shrinks");
    assert_eq!(scratch.len(), 100, "the frame's vertices are untouched");

    let small = (512 * 1024) / vertex_size;
    let mut under_floor: Vec<Vertex> = Vec::with_capacity(small);
    release_scratch_excess(&mut under_floor, 10);
    assert_eq!(under_floor.capacity(), small, "scratch under 1 MiB is kept");

    let mut busy: Vec<Vertex> = Vec::with_capacity(large);
    release_scratch_excess(&mut busy, large / 3);
    assert_eq!(busy.capacity(), large, "scratch within four times the frame's use is kept");

    let mut harness = FrameHarness::new(8);
    assert_eq!(harness.pipeline.vertex_scratch_retained(), ResourceAmount::default());
    harness.draw(None, &[red_column(0), red_column(1)], &[]);
    assert_eq!(
        harness.pipeline.vertex_scratch_retained(),
        ResourceAmount {
            bytes: harness.pipeline.vertex_scratch.capacity() * vertex_size,
            items: 1
        }
    );
}

/// A frame that emits no vertices still applies the scratch release policy: after a frame
/// large enough to leave over 1 MiB of scratch, an empty frame and an all-degenerate frame
/// each release the allocation (twice zero use is zero), and the retained figure that
/// `retained_amounts` reads drops to nothing. Both exits of `draw_frame` are covered.
#[test]
fn zero_vertex_frames_release_a_large_scratch() {
    const LARGE_QUADS: u32 = 5_000;
    let large: Vec<QuadInstance> = (0..LARGE_QUADS).map(|index| red_column(index % 8)).collect();
    let empty: Vec<QuadInstance> = Vec::new();
    let degenerate = vec![degenerate_quad(); 3];
    for (case, zero_use) in [("empty", &empty), ("all-degenerate", &degenerate)] {
        let mut harness = FrameHarness::new(8);
        harness.draw(None, &large, &[]);
        let peak = harness.pipeline.vertex_scratch_retained();
        assert!(peak.bytes > 1024 * 1024, "{case}: the large frame retains {} bytes", peak.bytes);

        let frame = harness.draw(None, zero_use, &[]);

        assert!(frame.red_columns.is_empty(), "{case}: nothing is drawn");
        assert_eq!(harness.pipeline.vertex_scratch.capacity(), 0, "{case}: allocation released");
        assert_eq!(
            harness.pipeline.vertex_scratch_retained(),
            ResourceAmount::default(),
            "{case}: the retained report shows the released scratch"
        );
    }
}

/// Feed `calls` draws of `quads` quads each to `window` against `capacity` quads; return the call
/// number (1-based) and target of every shrink requested.
fn feed(
    window: &mut ShrinkWindow,
    capacity: &mut u64,
    calls: u32,
    quads: u64,
    offset: u32,
) -> Vec<(u32, u64)> {
    let mut shrinks = Vec::new();
    for call in 1..=calls {
        *capacity = (*capacity).max(quads.next_power_of_two());
        if let Some(target) = window.observe(quads, *capacity, 4096) {
            shrinks.push((offset + call, target));
            *capacity = target;
        }
    }
    shrinks
}

/// The 600-draw hysteresis: a steady 20,000-quad load never shrinks; a 30,000-quad window then a
/// 2,000-quad window shrinks exactly once, at call 1,200, to `next_pow2(2 × 2,000)` floored at the
/// initial 4,096; alternating windows of 30,000 and 20,000 never shrink.
#[test]
fn present_buffers_shrink_once_after_a_quiet_window() {
    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    assert!(feed(&mut window, &mut capacity, 600, 20_000, 0).is_empty(), "steady load");

    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    assert!(feed(&mut window, &mut capacity, 600, 30_000, 0).is_empty());
    assert_eq!(feed(&mut window, &mut capacity, 600, 2_000, 600), [(1200, 4096)]);
    assert!(feed(&mut window, &mut capacity, 600, 2_000, 1200).is_empty(), "already small");

    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    for (index, quads) in [30_000, 20_000, 30_000, 20_000].into_iter().enumerate() {
        let offset = 600 * index as u32;
        assert!(feed(&mut window, &mut capacity, 600, quads, offset).is_empty(), "{quads}");
    }
}

/// A shrink and a reset both clear the index pattern, so the next draw writes the pattern for the
/// new capacity and the index buffer reads back as `build_indices`.
#[test]
fn a_shrink_or_a_reset_rewrites_the_index_pattern() {
    let mut harness = FrameHarness::new(1);
    let wide: Vec<QuadInstance> = (0..64).map(|index| red_column(index % HARNESS_WIDTH)).collect();
    let _grown = harness.draw(None, &wide, &[]);
    assert!(harness.pipeline.index_capacity >= 64 * INDICES_PER_QUAD as u64, "precondition");
    // The wide draw opened the first window, so that window's peak is 64 and it closes without a
    // shrink; the next window of single-quad draws closes oversized and shrinks to twice its peak.
    for _ in 0..2 * SHRINK_WINDOW_CALLS - 2 {
        let _frame = harness.draw(None, &[red_column(0)], &[]);
    }
    assert!(harness.pipeline.index_capacity >= 64 * INDICES_PER_QUAD as u64, "no shrink yet");
    let closing = harness.draw(None, &[red_column(1)], &[]);
    assert_eq!(closing.red_columns, vec![1], "the closing frame still draws its quad");
    assert_eq!(harness.pipeline.index_capacity, 2 * INDICES_PER_QUAD as u64, "shrunk to 2 quads");
    // The shrink cleared the pattern before this draw's upload, so the closing draw rewrote it.
    assert_eq!(closing.stats.index_bytes, pattern_bytes(harness.pipeline.index_capacity));
    assert_eq!(harness.index_buffer(), build_indices(2));
    let next = harness.draw(None, &[red_column(2), red_column(3)], &[]);
    assert_eq!(next.red_columns, vec![2, 3]);
    assert_eq!(next.stats.index_bytes, 0, "the rewritten pattern covers the new capacity");

    let _grown = harness.draw(None, &wide, &[]);
    let released = harness.pipeline.reset_to_initial(&harness.device);
    assert!(released > 0, "the reset releases the grown buffers");
    assert_eq!(harness.pipeline.index_pattern_quads, 0, "the reset cleared the pattern");
    assert_eq!(harness.pipeline.vertex_scratch.capacity(), 0, "the scratch is emptied");
    let after = harness.draw(None, &[red_column(5)], &[]);
    assert_eq!(after.red_columns, vec![5]);
    assert_eq!(harness.index_buffer(), build_indices(1));
}
