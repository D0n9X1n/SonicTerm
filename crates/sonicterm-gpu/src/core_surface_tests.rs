//! Renderer surface contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn renderer_resize_has_no_unchecked_wrapper() {
    const SOURCE: &str = crate::core::CORE_SOURCE;
    assert!(!SOURCE.contains("pub fn resize(&mut self, width: u32, height: u32)"));
    let lines = SOURCE.lines().collect::<Vec<_>>();
    assert!(lines.windows(2).any(|pair| {
        pair[0].trim() == "#[must_use]" && pair[1].trim_start().starts_with("pub fn try_resize")
    }));
}

/// Padded dirty damage clears real GPU glyph ink outside a compressed cell row.
#[cfg(target_os = "windows")]
#[test]
fn warp_retained_redraw_clears_overhanging_glyph_ink() {
    const WIDTH: u32 = 2;
    const HEIGHT: u32 = 64;
    const BYTES_PER_ROW: u32 = 256;

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
        adapter.request_device(&device_descriptor_for(true, wgpu::Features::empty())),
    )
    .expect("WARP device");
    let mut pipeline =
        crate::wezterm_pipeline::WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm, 2);
    let mut atlas = GlyphAtlas::new(1, 30);
    let glyph = atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('T', false, false), &mut SolidTallAtlasGlyph)
        .expect("tall glyph inserts");
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
        label: Some("retained overhang test target"),
        size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("retained overhang readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let glyphs = [sonicterm_text::GlyphInstance {
        rect: crate::quad::px_to_ndc(0.0, 10.0, 1.0, 30.0, WIDTH as f32, HEIGHT as f32),
        uv: glyph.uv,
        color: [1.0; 4],
        flags: [0.0; 4],
    }];
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("retained overhang initial pass"),
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
            WIDTH as f32,
            HEIGHT as f32,
            SubpixelAaMode::Off,
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
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll initial WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped initial WARP readback");
    assert_ne!(&bytes[35 * BYTES_PER_ROW as usize..35 * BYTES_PER_ROW as usize + 3], &[0, 0, 0]);
    drop(bytes);
    readback.unmap();

    let damage = dirty_rows_damage_rect_with_ink_pad(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 10, w: WIDTH, h: 40 },
        0.0,
        10.0,
        1,
        2.0,
        12.0,
        18.0,
        WIDTH,
        HEIGHT,
    )
    .expect("dirty row produces padded damage");
    let clear = crate::quad::QuadInstance::sharp(
        crate::quad::px_to_ndc(
            damage.x as f32,
            damage.y as f32,
            damage.w as f32,
            damage.h as f32,
            WIDTH as f32,
            HEIGHT as f32,
        ),
        [0.0, 0.0, 0.0, 1.0],
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("retained overhang clear pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(damage.x as u32, damage.y as u32, damage.w, damage.h);
        pipeline.draw_frame(
            &device,
            &queue,
            &mut pass,
            image_upload.image_bind_group(),
            glyph_upload.glyph_bind_group(),
            WIDTH as f32,
            HEIGHT as f32,
            SubpixelAaMode::Off,
            Some(clear),
            &[],
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
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped WARP readback");

    assert_eq!(&bytes[35 * BYTES_PER_ROW as usize..35 * BYTES_PER_ROW as usize + 3], &[0, 0, 0]);
}

/// The first retained frame writes background alpha once at every supported opacity.
#[test]
fn retained_first_frame_does_not_blend_a_second_background_reset() {
    let mut fixture = RetainedPixelFixture::new();
    let mut failures = Vec::new();
    for opacity in [0.0, 0.5, 1.0] {
        let background = reset_background(opacity);
        fixture.draw(true, full_surface_rect(4, 2), background, SubpixelAaMode::Off, &[]);
        let actual = fixture.pixels()[0];
        let expected = clear_bgra(background);
        if !pixels_close(actual, expected) {
            failures.push((opacity, actual, expected));
        }
    }
    assert!(failures.is_empty(), "first-frame background mismatch: {failures:?}");
}

/// Partial resets erase old ink once, preserve peer pixels, and cannot rely on later redraws to decay.
#[test]
fn retained_partial_resets_replace_pixels_and_remain_idempotent() {
    let mut fixture = RetainedPixelFixture::new();
    let damage = PixelRect { x: 0, y: 0, w: 2, h: 2 };
    let ink = [QuadInstance::sharp(px_to_ndc(0.0, 0.0, 2.0, 2.0, 4.0, 2.0), [1.0, 0.0, 0.0, 1.0])];
    let mut modes = vec![SubpixelAaMode::Off];
    if fixture.dual_source {
        modes.extend([SubpixelAaMode::Rgb, SubpixelAaMode::Bgr]);
    }
    let mut failures = Vec::new();
    for mode in modes {
        for opacity in [0.0, 0.5, 1.0] {
            fixture.draw(true, full_surface_rect(4, 2), wgpu::Color::GREEN, mode, &ink);
            let baseline = fixture.pixels();
            assert_eq!(baseline[0], [0, 0, 255, 255], "old ink must be present");
            let background = reset_background(opacity);
            fixture.draw(false, damage, background, mode, &[]);
            let first = fixture.pixels();
            let idle = fixture.pixels();
            fixture.draw(false, damage, background, mode, &[]);
            let repeated = fixture.pixels();
            let expected = clear_bgra(background);
            if !pixels_close(first[0], expected) || !pixels_close(repeated[0], expected) {
                failures.push((mode, opacity, first[0], repeated[0], expected));
            }
            assert_eq!(first[3], baseline[3], "outside damage must remain untouched");
            assert_eq!(repeated[3], baseline[3]);
            assert_eq!(idle, first, "an idle frame submits no correcting draw");
        }
    }
    println!("replacement_reset=EXERCISED dual_source={}", fixture.dual_source);
    assert!(failures.is_empty(), "retained reset mismatch: {failures:?}");
}

/// A later full invalidation clears once while ordinary translucent content still source-overs.
#[test]
fn retained_full_clear_preserves_content_blending_and_replaces_prior_frame() {
    let mut fixture = RetainedPixelFixture::new();
    let full = full_surface_rect(4, 2);
    fixture.draw(true, full, wgpu::Color::RED, SubpixelAaMode::Off, &[]);
    let background = reset_background(0.5);
    let source = [0.0, 0.25, 0.0, 0.5];
    let content = [QuadInstance::sharp(px_to_ndc(0.0, 0.0, 1.0, 1.0, 4.0, 2.0), source)];

    fixture.draw(false, full, background, SubpixelAaMode::Off, &content);

    let pixels = fixture.pixels();
    assert!(pixels_close(pixels[3], clear_bgra(background)), "full clear must discard prior red");
    let expected = wgpu::Color {
        r: background.r * 0.5,
        g: f64::from(source[1]) + background.g * 0.5,
        b: background.b * 0.5,
        a: 0.5 + background.a * 0.5,
    };
    assert!(
        pixels_close(pixels[0], clear_bgra(expected)),
        "content must retain source-over blending"
    );
}

#[test]
fn surface_size_budget_accepts_8k_and_minimized_windows() {
    let eight_k =
        validated_surface_size(7680, 4320, MAX_SURFACE_DIMENSION).expect("8K must fit the budget");
    assert_eq!((eight_k.width, eight_k.height), (7680, 4320));
    assert_eq!(eight_k.bytes, 7680 * 4320 * 4);
    let dci_eight_k = validated_surface_size(8192, 4320, MAX_SURFACE_DIMENSION)
        .expect("DCI 8K must fit the budget");
    assert_eq!((dci_eight_k.width, dci_eight_k.height), (8192, 4320));

    let minimized =
        validated_surface_size(0, 0, MAX_SURFACE_DIMENSION).expect("zero clamps to one pixel");
    assert_eq!((minimized.width, minimized.height, minimized.bytes), (1, 1, 4));
}

#[test]
fn surface_size_budget_rejects_multi_gibabyte_frames() {
    assert!(validated_surface_size(8192, 8192, MAX_SURFACE_DIMENSION).is_none());
    assert!(validated_surface_size(u32::MAX, u32::MAX, MAX_SURFACE_DIMENSION).is_none());
    assert!(validated_surface_size(MAX_SURFACE_DIMENSION + 1, 1, MAX_SURFACE_DIMENSION).is_none());
}

#[test]
fn frame_texture_extent_is_one_pixel_under_the_windows_software_presenter() {
    // GDI presents from the CPU frame and never samples the wgpu frame texture, so it holds 1x1; the GPU
    // presenter needs the surface size, never zero.
    assert_eq!(frame_texture_extent(true, 1920, 1080), (1, 1));
    assert_eq!(frame_texture_extent(false, 1920, 1080), (1920, 1080));
    assert_eq!(frame_texture_extent(false, 0, 0), (1, 1));
    assert_eq!(frame_texture_payload_bytes((1, 1)), 4);
    assert_eq!(frame_texture_payload_bytes((1920, 1080)), 1920 * 1080 * 4);
}

#[test]
fn every_frame_texture_comes_from_build_frame_texture() {
    // Construction, resize, recovery prepare and the degrade switch all size the texture through one
    // helper, so none of them can allocate the surface size under GDI. Windows CI checks sources out
    // with CRLF line ends, so the scan runs on a CRLF copy too.
    let lf = [
        crate::core::CORE_SOURCE,
        include_str!("rebind.rs"),
        include_str!("present.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .map(|source| source.replace("\r\n", "\n"));
    let crlf = lf.clone().map(|source| source.replace('\n', "\r\n"));
    for [core, rebind, present, atlas_lifecycle] in [&lf, &crlf] {
        check_frame_texture_inventory(core, rebind, present, atlas_lifecycle);
    }
}

/// A resize to the configured size is `Unchanged`, a new valid size `Changed`, and an
/// unrepresentable one `Rejected`; a zero size clamps to one pixel before it is compared.
#[test]
fn resize_outcome_distinguishes_changed_unchanged_and_rejected() {
    let max = MAX_SURFACE_DIMENSION;
    let same = validated_surface_size(800, 600, max);
    assert_eq!(classify_resize((800, 600), same.as_ref()), ResizeOutcome::Unchanged);
    let wider = validated_surface_size(801, 600, max);
    assert_eq!(classify_resize((800, 600), wider.as_ref()), ResizeOutcome::Changed);
    let unsafe_size = validated_surface_size(u32::MAX, u32::MAX, max);
    assert_eq!(classify_resize((800, 600), unsafe_size.as_ref()), ResizeOutcome::Rejected);
    let zero = validated_surface_size(0, 0, max);
    assert_eq!(classify_resize((1, 1), zero.as_ref()), ResizeOutcome::Unchanged);
}

/// `try_resize_outcome` returns `Unchanged` through `classify_resize` before it reconfigures the
/// surface or clears the retained key, and reports `Changed` only after the new size is applied.
#[test]
fn try_resize_outcome_returns_unchanged_before_any_reconfiguration() {
    let core = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let body = core.split("pub fn try_resize_outcome(").nth(1).expect("the resize body");
    let body = &body[..body.find("\n    }\n").expect("the body ends")];
    let unchanged =
        body.find("return ResizeOutcome::Unchanged;").expect("an unchanged early return");
    assert!(body[..unchanged].contains("classify_resize("), "{body}");
    let configure = body.find("self.surface.configure(").expect("the surface is configured");
    let key = body.find("self.last_frame_key = None;").expect("the retained key is cleared");
    assert!(unchanged < configure && unchanged < key, "{body}");
    assert!(body.trim_end().ends_with("ResizeOutcome::Changed"), "{body}");
}
