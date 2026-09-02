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

    push_glyph_instances(&mut vertices, &[glyph], 16.0, 12.0);

    assert_eq!(vertices.len(), VERTICES_PER_QUAD);
    for vertex in vertices {
        assert_eq!(vertex.has_color, IS_GLYPH);
        assert_eq!(vertex.hsv, [1.0, 1.0, 1.0]);
        assert_eq!(vertex.fg_color, color);
    }
    assert!(SHADER.contains("hsv *= uniforms.foreground_text_hsb;"));
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
    let (device, queue) =
        pollster::block_on(adapter.request_device(&crate::core::device_descriptor_for(true)))
            .expect("WARP device");
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 4);
    let atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.texture_bind_group_layout(),
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
            upload.bind_group(),
            upload.bind_group(),
            surface[0],
            surface[1],
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

/// WARP and software must encode fully covered half-red sharp, rounded, and line quads equally.
#[cfg(target_os = "windows")]
#[test]
fn warp_half_red_quads_match_software_linear_blend() {
    const WIDTH: u32 = 16;
    const HEIGHT: u32 = 12;
    const BYTES_PER_ROW: u32 = 256;
    let surface = [WIDTH as f32, HEIGHT as f32];
    let half_red = [0.5, 0.0, 0.0, 0.5];
    let quads = [
        QuadInstance::sharp(
            crate::quad::px_to_ndc(0.0, 0.0, 4.0, 4.0, surface[0], surface[1]),
            half_red,
        ),
        QuadInstance::rounded(
            crate::quad::px_to_ndc(5.0, 0.0, 6.0, 6.0, surface[0], surface[1]),
            half_red,
            [6.0, 6.0],
            2.0,
        ),
        line_segment(surface, [4.0, 9.0], [12.0, 9.0], 4.0, half_red),
    ];
    let samples = [[1_u32, 1_u32], [8, 3], [8, 9]];

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
    let (device, queue) =
        pollster::block_on(adapter.request_device(&crate::core::device_descriptor_for(true)))
            .expect("WARP device");
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 1);
    let atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(1, 1);
    let upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.texture_bind_group_layout(),
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("half-red quad parity target"),
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
        label: Some("half-red quad parity readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("half-red quad parity pass"),
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
            upload.bind_group(),
            upload.bind_group(),
            surface[0],
            surface[1],
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
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll half-red WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped half-red WARP readback");
    let mut software =
        crate::software_windows::WindowsSoftwareFrame::new(WIDTH, HEIGHT, [0.0, 0.0, 0.0, 1.0])
            .expect("valid software frame");
    software.draw_layers(&atlas, &atlas, &quads, &[], &[], &[], &[]);

    for point in samples {
        let offset = point[1] as usize * BYTES_PER_ROW as usize + point[0] as usize * 4;
        let gpu = [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]];
        let cpu = software.pixel_bgra_at(point[0], point[1]).expect("software sample pixel");
        assert_eq!(cpu, [0, 0, 188, 255]);
        for channel in 0..4 {
            assert!(
                gpu[channel].abs_diff(cpu[channel]) <= 1,
                "half-red sample {point:?} channel {channel} differs: GPU {gpu:?}, software {cpu:?}"
            );
        }
    }
}
