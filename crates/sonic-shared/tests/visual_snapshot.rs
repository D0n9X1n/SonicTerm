//! Headless visual snapshot regression for the atlas text pipeline.
//!
//! Renders five canonical payloads (ASCII, CJK, emoji, ligature,
//! Powerline) through the real `SwashRasterizer + GlyphAtlas +
//! TextPipeline` stack into a 384×48 offscreen target, perceptually
//! hashes each frame with dHash (16×16 DoubleGradient → 18 bytes /
//! 36 hex chars per baseline), and compares the hex string to a
//! committed baseline under `tests/snapshots/`.
//!
//! Why dHash and not pixel-for-pixel? Subpixel ordering on
//! macOS Metal vs Windows DX12 differs by a few LSBs in places —
//! a strict pixel diff is noisy across platforms. dHash collapses
//! a frame to a structural fingerprint; small driver-level variations
//! land within a small hamming distance (we allow ≤ 8 bits across the
//! whole 144-bit hash).
//!
//! Workflow:
//!   `cargo test -p sonic-shared --test visual_snapshot` — assert.
//!   `UPDATE_SNAPSHOTS=1 cargo test -p sonic-shared --test visual_snapshot`
//!     — rewrite baselines from current output.
//!
//! On mismatch the test writes `<name>.actual.png` next to the baseline
//! so a human can eyeball the diff.
//!
//! If no wgpu adapter is available (CI without GPU, etc.) the test
//! prints "no adapter, skipping" and passes — strictly better than a
//! gated `#[ignore]` because the suite still runs end-to-end.
//!
//! The test is **macOS-only** (`#[cfg(target_os = "macos")]`). The
//! committed baselines were generated on macOS Metal against the
//! platform's CoreText fallback chain (PingFang SC, Apple Color
//! Emoji). Running them on Windows DX12 against Segoe UI Emoji /
//! Microsoft YaHei would diff a different font face for the same
//! codepoint and cannot land within an 8-bit hamming distance.
//! When we ship a Windows CI runner we'll add per-platform baselines.

#![cfg(target_os = "macos")]

use cosmic_text::FontSystem;
use image::{ImageBuffer, Rgba};
use image_hasher::{HashAlg, HasherConfig};
use pollster::FutureExt as _;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::{
    atlas_upload::AtlasUpload,
    glyph_atlas::GlyphAtlas,
    quad::px_to_ndc,
    swash_rasterizer::SwashRasterizer,
    text_pipeline::{GlyphInstance, TextPipeline},
};
use wgpu::{
    Color, CommandEncoderDescriptor, DeviceDescriptor, Extent3d, InstanceDescriptor, LoadOp,
    Operations, PowerPreference, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, StoreOp, TexelCopyBufferLayout, TextureDescriptor, TextureDimension,
    TextureFormat, TextureUsages, TextureViewDescriptor,
};

const W: u32 = 384;
const H: u32 = 48;
const RASTER_PX: f32 = 22.0;
const MAX_HAMMING: u32 = 8;

struct Payload {
    name: &'static str,
    text: &'static str,
}

const PAYLOADS: &[Payload] = &[
    Payload { name: "ascii", text: "Hello, Sonic!" },
    Payload { name: "cjk", text: "你好，世界 中文" },
    Payload { name: "emoji", text: "rust 🦀 fire 🔥 ok" },
    Payload { name: "ligature", text: "==> != >= -> |> <$>" },
    Payload { name: "powerline", text: "\u{e0b0}branch\u{e0b1}\u{e0b2}main\u{e0b3}" },
];

fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    fs.db_mut().load_font_data(bytes);
                }
            }
        }
    }
    fs
}

fn snapshots_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Render one payload into a `W×H` RGBA8 buffer (sRGB-encoded).
/// Returns `None` if no wgpu adapter is available — caller should skip.
/// The boolean is `true` if at least one glyph in the payload reported
/// `info.is_color`, i.e. the color BGRA shader branch was exercised.
fn render_payload(text: &str) -> Option<(Vec<u8>, bool)> {
    let instance = wgpu::Instance::new(InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on()
        .ok()?;
    let (device, queue) = adapter.request_device(&DeviceDescriptor::default()).block_on().ok()?;

    let mut fs = font_system();
    let mut atlas = GlyphAtlas::default_size();
    let format = TextureFormat::Rgba8UnormSrgb;
    let mut pipeline = TextPipeline::new(&device, format, 256);
    let upload = AtlasUpload::new(&device, &queue, &atlas, &pipeline.bind_group_layout);

    let mut rasterizer = SwashRasterizer::new(&mut fs, "Rec Mono Casual", RASTER_PX);

    // Lay out characters left-to-right with a fixed advance roughly the
    // cell width; ligature shaping isn't reproduced here (we render each
    // codepoint to its own atlas tile) but the visual fingerprint is
    // still stable and that's all dHash needs.
    let advance: f32 = RASTER_PX * 0.62;
    let baseline_y: f32 = (H as f32) * 0.78;
    let mut instances: Vec<GlyphInstance> = Vec::new();
    let mut saw_color = false;
    let mut pen_x: f32 = 4.0;
    let sw = W as f32;
    let sh = H as f32;

    for ch in text.chars() {
        if ch == ' ' {
            pen_x += advance * 0.5;
            continue;
        }
        let Some(slot) = rasterizer.resolve_slot(ch, false, false) else {
            pen_x += advance;
            continue;
        };
        let key = GlyphKey { ch, font_slot: slot, weight_bold: false, italic: false, glyph_id: 0 };
        let Some(info) = atlas.get_or_insert(key, &mut rasterizer) else {
            pen_x += advance;
            continue;
        };
        let gw = info.px_size[0] as f32;
        let gh = info.px_size[1] as f32;
        if gw == 0.0 || gh == 0.0 {
            pen_x += advance;
            continue;
        }
        let gx = pen_x;
        let gy = baseline_y - gh;
        if info.is_color {
            saw_color = true;
        }
        instances.push(GlyphInstance {
            rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
            uv: info.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        });
        pen_x += advance;
        if pen_x > sw - advance {
            break;
        }
    }
    upload.sync(&queue, &mut atlas);

    let target = device.create_texture(&TextureDescriptor {
        label: Some("snap-color"),
        size: Extent3d { width: W, height: H, depth_or_array_layers: 1 },
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
            label: Some("snap-pass"),
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

    // bytes_per_row must be 256-aligned; W=384 * 4 = 1536 is fine.
    let bytes_per_row = W * 4;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("snap-readback"),
        size: (bytes_per_row * H) as u64,
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
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(H),
            },
        },
        Extent3d { width: W, height: H, depth_or_array_layers: 1 },
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
    Some((data, saw_color))
}

/// Returns `true` if the buffer contains a pixel whose R, G, B channels
/// are not all equal — proof the color BGRA shader branch produced a
/// non-monochrome tile, not the coverage path which only ever emits
/// shades of the source color (here, white → grey).
fn has_chromatic_pixel(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|px| px[0] != px[1] || px[1] != px[2])
}

fn dhash_hex(rgba: &[u8]) -> String {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(W, H, rgba.to_vec()).expect("rgba buffer matches W×H×4");
    let hasher =
        HasherConfig::new().hash_alg(HashAlg::DoubleGradient).hash_size(16, 16).to_hasher();
    let h = hasher.hash_image(&img);
    hex_lower(h.as_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn hamming(a: &[u8], b: &[u8]) -> u32 {
    assert_eq!(a.len(), b.len(), "hash length mismatch");
    a.iter().zip(b.iter()).map(|(x, y)| (x ^ y).count_ones()).sum()
}

#[test]
fn visual_snapshot_regression_dhash() {
    let dir = snapshots_dir();
    std::fs::create_dir_all(&dir).expect("mkdir snapshots");

    // Probe once: if no adapter, skip the whole suite gracefully.
    let probe = render_payload("A");
    if probe.is_none() {
        eprintln!("[visual_snapshot] no wgpu adapter, skipping");
        return;
    }

    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut failures: Vec<String> = Vec::new();

    for p in PAYLOADS {
        let Some((rgba, saw_color)) = render_payload(p.text) else {
            eprintln!("[visual_snapshot] adapter went away for {}; skipping", p.name);
            return;
        };

        // The emoji payload exists specifically to exercise the
        // is_color BGRA shader branch added in PR #49. If swash didn't
        // route any glyph through the color path, the snapshot would
        // silently pass through the monochrome coverage path — defeating
        // the purpose of the test. Assert structurally before hashing.
        if p.name == "emoji" {
            assert!(
                saw_color,
                "emoji payload did not exercise the color glyph path \
                 (no glyph reported info.is_color); the BGRA branch \
                 added in PR #49 is not under test"
            );
            assert!(
                has_chromatic_pixel(&rgba),
                "emoji payload rendered no chromatic pixels; the color \
                 BGRA shader branch produced only greyscale output"
            );
        }

        let actual_hex = dhash_hex(&rgba);
        let baseline_path = dir.join(format!("{}.hash", p.name));

        if update {
            std::fs::write(&baseline_path, &actual_hex).expect("write baseline");
            eprintln!("[visual_snapshot] wrote baseline {} = {}", p.name, actual_hex);
            continue;
        }

        if !baseline_path.exists() {
            failures.push(format!(
                "{}: baseline missing at {} (rerun with UPDATE_SNAPSHOTS=1)",
                p.name,
                baseline_path.display()
            ));
            continue;
        }

        let expected_hex =
            std::fs::read_to_string(&baseline_path).expect("read baseline").trim().to_string();

        let dist = hamming(&hex_to_bytes(&actual_hex), &hex_to_bytes(&expected_hex));
        if dist > MAX_HAMMING {
            // Dump the actual frame for human inspection.
            let actual_png = dir.join(format!("{}.actual.png", p.name));
            let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_raw(W, H, rgba).expect("rgba buffer matches W×H×4");
            let _ = img.save(&actual_png);
            failures.push(format!(
                "{}: hamming={} > {} (expected={}, actual={}, wrote {})",
                p.name,
                dist,
                MAX_HAMMING,
                expected_hex,
                actual_hex,
                actual_png.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "visual snapshot regression:\n  {}\n\n\
         Re-run with UPDATE_SNAPSHOTS=1 to refresh baselines if the change is intentional.",
        failures.join("\n  ")
    );
}
