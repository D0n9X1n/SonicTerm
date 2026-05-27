//! Offscreen end-to-end test for the B3 atlas-backed text pipeline.
//!
//! Why this exists: PR #36 cut the renderer over to `TextPipeline +
//! GlyphAtlas + SwashRasterizer`, the suite passed, but the windowed
//! app produced a blank screen — a runtime regression no headless
//! check would catch.
//!
//! This test builds a real wgpu device, rasterizes one glyph via
//! `SwashRasterizer`, packs it into a `GlyphAtlas`, uploads it through
//! `AtlasUpload`, and issues exactly the draw call the renderer would
//! use — `text_pipeline.draw(..., &[GlyphInstance])` — into a 256×256
//! offscreen color attachment. It then maps the texture back and
//! asserts the rendered region has non-zero pixels.
//!
//! If glyph positioning, UV math, atlas upload, or the shader's
//! vertical-flip is wrong, the rendered region is black and this test
//! fails — long before the windowed app ever runs.

use cosmic_text::FontSystem;
use pollster::FutureExt as _;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::{
    atlas_upload::AtlasUpload,
    glyph_atlas::GlyphAtlas,
    quad::px_to_ndc,
    swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
    text_pipeline::{GlyphInstance, TextPipeline},
};
use wgpu::{
    Color, CommandEncoderDescriptor, DeviceDescriptor, Extent3d, InstanceDescriptor, LoadOp,
    Operations, PowerPreference, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, StoreOp, TexelCopyBufferLayout, TextureDescriptor, TextureDimension,
    TextureFormat, TextureUsages, TextureViewDescriptor,
};

fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            let bytes = std::fs::read(&p).unwrap();
            fs.db_mut().load_font_data(bytes);
        }
    }
    fs
}

/// Render a single glyph instance into a 256×256 offscreen texture and
/// assert that at least one pixel in the expected glyph region has a
/// non-zero red channel. This is the canonical "did the cutover
/// actually draw text?" check.
#[test]
fn atlas_text_pipeline_writes_visible_pixels_offscreen() {
    // ----- wgpu device --------------------------------------------------
    let instance = wgpu::Instance::new(InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on()
        .expect("adapter");
    let (device, queue) =
        adapter.request_device(&DeviceDescriptor::default()).block_on().expect("device");

    // ----- glyph + atlas + pipeline ------------------------------------
    let mut fs = font_system();
    let mut atlas = GlyphAtlas::default_size();
    let format = TextureFormat::Rgba8UnormSrgb;
    let mut pipeline = TextPipeline::new(&device, format, 16);
    let upload = AtlasUpload::new(&device, &queue, &atlas, &pipeline.bind_group_layout);

    // Rasterize 'A' and pack it.
    let info = {
        let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
        atlas.get_or_insert(GlyphKey::new('A', false, false), &mut r).expect("rasterize A")
    };
    assert!(info.px_size[0] > 0 && info.px_size[1] > 0, "A must have visible pixels");
    upload.sync(&queue, &mut atlas);

    // ----- build one centered glyph instance ---------------------------
    let sw = 256.0_f32;
    let sh = 256.0_f32;
    let gw = info.px_size[0] as f32;
    let gh = info.px_size[1] as f32;
    let gx = (sw - gw) * 0.5;
    let gy = (sh - gh) * 0.5;
    let instances = [GlyphInstance {
        rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
        uv: info.uv,
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0, 0.0, 0.0, 0.0],
    }];

    // ----- offscreen color attachment ----------------------------------
    let target = device.create_texture(&TextureDescriptor {
        label: Some("test-color"),
        size: Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("test-pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipeline.draw(&device, &queue, &mut pass, upload.bind_group(), &instances);
    }

    // Copy texture -> buffer for readback. bytes_per_row must be 256-byte aligned.
    let bytes_per_row = 256 * 4; // 1024, already aligned
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bytes_per_row * 256) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row as u32),
                rows_per_image: Some(256),
            },
        },
        Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range().to_vec();
    drop(readback);

    // ----- assertions --------------------------------------------------
    // 1. The entire image isn't black.
    let total_lit = data.chunks_exact(4).filter(|px| px[0] > 0 || px[1] > 0 || px[2] > 0).count();
    assert!(total_lit > 0, "frame is entirely black; cutover draw call produced no visible pixels");

    // 2. Some lit pixels fall inside the expected glyph bounding box.
    let x0 = gx as usize;
    let y0 = gy as usize;
    let x1 = (gx + gw) as usize;
    let y1 = (gy + gh) as usize;
    let mut hit = 0;
    for y in y0..y1.min(256) {
        for x in x0..x1.min(256) {
            let off = (y * 256 + x) * 4;
            if data[off] > 0 || data[off + 1] > 0 || data[off + 2] > 0 {
                hit += 1;
            }
        }
    }
    assert!(
        hit > 0,
        "no lit pixels inside the expected glyph rect ({x0},{y0})..({x1},{y1}); \
         glyphs are landing somewhere else (likely a UV / Y-flip bug)"
    );

    // 3. Orientation check: 'A' has more lit pixels in its bottom half
    // (the wide base) than its top half (the apex). A vertical UV flip
    // would invert this ratio. This is the assertion that catches the
    // PR #36 regression — the offscreen pixels-non-zero check on its own
    // passes even with the glyph rendered upside-down.
    let y_mid = (y0 + y1) / 2;
    let mut top_lit = 0;
    let mut bot_lit = 0;
    for y in y0..y1.min(256) {
        for x in x0..x1.min(256) {
            let off = (y * 256 + x) * 4;
            let lit = data[off] > 0 || data[off + 1] > 0 || data[off + 2] > 0;
            if !lit {
                continue;
            }
            if y < y_mid {
                top_lit += 1;
            } else {
                bot_lit += 1;
            }
        }
    }
    assert!(
        bot_lit > top_lit,
        "'A' must have more lit pixels in its bottom half than its top half \
         (top={top_lit}, bot={bot_lit}); a vertical UV flip in the shader \
         would invert this and render upside-down"
    );
}
