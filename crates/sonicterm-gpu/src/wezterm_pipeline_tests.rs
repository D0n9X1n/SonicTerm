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
    assert_eq!(kind(instance([0.0, 1.0, 1.0, 0.0]), Rgb), IS_IMAGE);
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
    let mut frame = crate::software_windows::WindowsSoftwareFrame::new(1, 1, background)
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
        crate::software_windows::WindowsSoftwareFrame::new(WIDTH, HEIGHT, [0.0, 0.0, 0.0, 1.0])
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
    let mut software = crate::software_windows::WindowsSoftwareFrame::new(
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
