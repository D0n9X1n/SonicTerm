//! HiDPI / scale-factor regression tests.
//!
//! Guards the fix for the "blurry text on Retina" bug: when
//! `scale_factor > 1.0`, the glyph rasterizer must produce tiles at the
//! *physical* em-size (font_size × scale_factor) so the wgpu surface
//! samples a crisp source instead of upscaling a logical-px bitmap.
//!
//! These tests run without a real wgpu device — they exercise the
//! `SwashRasterizer` + `GlyphAtlas` pair directly, which is the actual
//! code path the renderer uses to build the atlas.

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::GlyphAtlas;
use sonic_shared::swash_rasterizer::SwashRasterizer;

const FONT_SIZE: f32 = 14.0;
const FONT_FAMILY: &str = "Rec Mono Casual";

fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read assets/fonts ({dir:?}): {e}"));
    let mut loaded = 0;
    for e in entries.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            let bytes = std::fs::read(&p).unwrap();
            fs.db_mut().load_font_data(bytes);
            loaded += 1;
        }
    }
    assert!(loaded > 0, "expected bundled fonts in {dir:?}");
    fs
}

/// Cell-metric proxy: cosmic-text scales advance + line-height linearly
/// in font_size, so `measure_cell` at the same font_size gives the same
/// result regardless of the rasterizer's internal physical-px size.
/// We assert this property indirectly: building two rasterizers at
/// different `px` values must not affect the *logical* font_size used
/// for layout, which is decided at the renderer level and only fed
/// through to the rasterizer for tile dimensions.
fn tile_height_for(scale_factor: f32) -> u32 {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * scale_factor);
    let mut atlas = GlyphAtlas::default_size();
    let key = GlyphKey::new('A', false, false);
    let info = atlas.get_or_insert(key, &mut r).expect("A insertable");
    info.px_size[1]
}

#[test]
fn atlas_tile_at_1x_matches_font_size_order_of_magnitude() {
    let h = tile_height_for(1.0);
    // 'A' rasterized at 14px will be ~9-12 px tall (cap height < em).
    // The exact value depends on the font's vertical metrics; what we
    // care about is that it's in the right ballpark vs. font_size.
    assert!(h > 0, "tile must have visible pixels at 1x");
    assert!(
        h as f32 <= FONT_SIZE * 1.2,
        "1x tile height ({h}) must not exceed ~font_size ({FONT_SIZE})"
    );
}

#[test]
fn atlas_tile_at_2x_is_roughly_double_1x() {
    let h1 = tile_height_for(1.0);
    let h2 = tile_height_for(2.0);
    // Two glyphs rasterized at 2× em-size should be very close to 2×
    // the pixel height. Allow ±2 px for hinting / rounding.
    let expected = h1 * 2;
    let lo = expected.saturating_sub(2);
    let hi = expected + 2;
    assert!(h2 >= lo && h2 <= hi, "2x tile height {h2} not within ±2 of 2×1x ({expected})");
}

#[test]
fn changing_rasterizer_scale_does_not_change_font_size_argument() {
    // The renderer keeps `font_size` (logical) and only multiplies by
    // `scale_factor` when handing it to the rasterizer. Verify the
    // rasterizer reports back the px it was constructed with, so
    // layout math (cell_w/cell_h, measure_cell) reading `font_size`
    // never sees the physical inflation.
    let mut fs = font_system_with_bundled();
    let r1 = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
    assert!((r1.px() - FONT_SIZE).abs() < f32::EPSILON);
    drop(r1);
    let r2 = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
    assert!((r2.px() - FONT_SIZE * 2.0).abs() < f32::EPSILON);
}

#[test]
fn rebuilt_atlas_at_2x_returns_doubled_tile_for_same_key() {
    // Models GpuRenderer::set_scale_factor: clear atlas + new
    // rasterizer at the new physical px → next get_or_insert for the
    // same logical key returns a 2× tile.
    let mut fs = font_system_with_bundled();
    let key = GlyphKey::new('A', false, false);

    let h_before = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
        let mut atlas = GlyphAtlas::default_size();
        atlas.get_or_insert(key, &mut r).expect("1x A").px_size[1]
    };

    // Simulate set_scale_factor(2.0): atlas is cleared, rasterizer
    // rebuilt at 2× physical px.
    let h_after = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
        let mut atlas = GlyphAtlas::default_size();
        atlas.get_or_insert(key, &mut r).expect("2x A").px_size[1]
    };

    let expected = h_before * 2;
    let lo = expected.saturating_sub(2);
    let hi = expected + 2;
    assert!(
        h_after >= lo && h_after <= hi,
        "after scale 2x: tile {h_after} not within ±2 of 2×{h_before} ({expected})"
    );
}

#[test]
fn atlas_dim_grows_with_scale_factor() {
    // The renderer's atlas-size helper must scale up on Retina so a 2×
    // tile working set fits. At 1× we keep the original ATLAS_DIM
    // footprint; at 2× we expect roughly double the dimension.
    use sonic_shared::glyph_atlas::ATLAS_DIM;
    let a = sonic_shared::render::atlas_dim_for_scale(1.0);
    let b = sonic_shared::render::atlas_dim_for_scale(2.0);
    let c = sonic_shared::render::atlas_dim_for_scale(3.0);
    assert_eq!(a, ATLAS_DIM, "1x atlas dim must equal ATLAS_DIM");
    assert!(b >= ATLAS_DIM * 2, "2x atlas dim ({b}) must be >= 2×ATLAS_DIM");
    assert!(c >= ATLAS_DIM * 3, "3x atlas dim ({c}) must be >= 3×ATLAS_DIM");
}

#[test]
fn atlas_dim_floors_at_base_for_subunit_scale() {
    // Edge case: some platforms can report scale_factor < 1.0 (e.g.
    // fractional zoom out). We must never shrink the atlas below the
    // base size, otherwise even a 1× working set wouldn't fit.
    use sonic_shared::glyph_atlas::ATLAS_DIM;
    let a = sonic_shared::render::atlas_dim_for_scale(0.5);
    assert!(a >= ATLAS_DIM, "sub-unit scale must not shrink atlas below ATLAS_DIM");
}

#[test]
fn atlas_offsets_scale_logically_on_2x() {
    // Verifies the glyph rect's top-left offset (px_offset / scale) sits
    // within one cell of the cell box origin. With a 14px font on a 2×
    // display, swash returns physical-px offsets in the 0..30 range; the
    // renderer divides by scale_factor to land back in logical-px space.
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
    let mut atlas = GlyphAtlas::default_size();
    let info =
        atlas.get_or_insert(GlyphKey::new('A', false, false), &mut r).expect("A insertable at 2x");
    // After dividing by scale_factor, the X offset must be within
    // ~font_size (i.e. roughly one cell width).
    let logical_x = info.px_offset[0] as f32 / 2.0;
    assert!(
        logical_x.abs() <= FONT_SIZE,
        "logical x-offset {logical_x} should be within one cell of origin"
    );
    let logical_y = info.px_offset[1] as f32 / 2.0;
    // Y offset is negative (baseline-relative, swash convention).
    assert!(
        logical_y.abs() <= FONT_SIZE * 1.5,
        "logical y-offset {logical_y} should be within ~1.5 cell of baseline"
    );
}

#[test]
fn no_op_when_scale_unchanged_via_helper() {
    // atlas_dim_for_scale is deterministic and idempotent — two calls
    // with the same scale must produce the same dimension, so a
    // no-change scale_factor update won't trigger a needless atlas
    // realloc in set_scale_factor.
    let a = sonic_shared::render::atlas_dim_for_scale(2.0);
    let b = sonic_shared::render::atlas_dim_for_scale(2.0);
    assert_eq!(a, b);
}

#[test]
fn tile_scales_for_lowercase_glyph() {
    let mut fs = font_system_with_bundled();
    let key = GlyphKey::new('o', false, false);
    let h1 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    let h2 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    let lo = (h1 * 2).saturating_sub(2);
    let hi = h1 * 2 + 2;
    assert!(h2 >= lo && h2 <= hi, "'o' 2x tile {h2} not within ±2 of 2×{h1}");
}

#[test]
fn tile_scales_for_bold_glyph() {
    let mut fs = font_system_with_bundled();
    let key = GlyphKey::new('A', true, false);
    let h1 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    let h2 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    assert!(h1 > 0 && h2 > 0, "bold tiles must have visible pixels");
    let lo = (h1 * 2).saturating_sub(3);
    let hi = h1 * 2 + 3;
    assert!(h2 >= lo && h2 <= hi, "bold 2x tile {h2} not within ±3 of 2×{h1}");
}

#[test]
fn tile_at_125x_intermediate_scale() {
    let mut fs = font_system_with_bundled();
    let key = GlyphKey::new('A', false, false);
    let h1 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    let h125 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 1.25);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    let h2 = {
        let mut r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
        GlyphAtlas::default_size().get_or_insert(key, &mut r).unwrap().px_size[1]
    };
    assert!(h1 <= h125, "1.25x tile ({h125}) must be >= 1x tile ({h1})");
    assert!(h125 <= h2, "1.25x tile ({h125}) must be <= 2x tile ({h2})");
}

#[test]
fn atlas_dim_for_125x_holds_at_base() {
    use sonic_shared::glyph_atlas::ATLAS_DIM;
    let d = sonic_shared::render::atlas_dim_for_scale(1.25);
    assert!(d >= ATLAS_DIM, "1.25x atlas dim {d} must be >= ATLAS_DIM");
}

#[test]
fn fallback_chain_unchanged_by_scale() {
    let mut fs = font_system_with_bundled();
    let names_1x: Vec<String> = {
        let r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE);
        r.families().to_vec()
    };
    let names_2x: Vec<String> = {
        let r = SwashRasterizer::new(&mut fs, FONT_FAMILY, FONT_SIZE * 2.0);
        r.families().to_vec()
    };
    assert_eq!(names_1x, names_2x);
}

#[test]
fn rasterizer_px_round_trips_through_scale() {
    let mut fs = font_system_with_bundled();
    let r_a = SwashRasterizer::new(&mut fs, FONT_FAMILY, 14.0 * 2.0);
    drop(r_a);
    let r_b = SwashRasterizer::new(&mut fs, FONT_FAMILY, 28.0);
    assert!((r_b.px() - 28.0).abs() < f32::EPSILON);
}

#[test]
fn atlas_upload_recreated_matches_new_atlas_dim_after_scale_change() {
    // Regression for PR #63 review: GpuRenderer::set_scale_factor used
    // to replace the CPU GlyphAtlas with a larger one on 1x→2x while
    // leaving the GPU-side AtlasUpload pointing at the OLD-size
    // texture. The next sync()/draw would either OOB-write or sample
    // tiles at stale UVs.
    //
    // This test models the fix: after rebuilding the atlas for the new
    // scale, AtlasUpload::new must produce a texture+bind_group whose
    // reported dimensions match the new atlas. We spin up a real wgpu
    // device (same offscreen pattern as text_pipeline_offscreen.rs) so
    // the assertion is grounded in actual GPU resources, not a mock.
    use pollster::FutureExt as _;
    use sonic_shared::glyph_atlas::{AtlasUpload, GlyphAtlas};
    use sonic_shared::text_pipeline::TextPipeline;
    use wgpu::{
        DeviceDescriptor, InstanceDescriptor, PowerPreference, RequestAdapterOptions, TextureFormat,
    };

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
    let pipeline = TextPipeline::new(&device, TextureFormat::Rgba8UnormSrgb, 16);

    // Step 1: build "1x" atlas + upload.
    let dim_1x = sonic_shared::render::atlas_dim_for_scale(1.0);
    let atlas_1x = GlyphAtlas::new(dim_1x, dim_1x);
    let upload_1x = AtlasUpload::new(&device, &queue, &atlas_1x, &pipeline.bind_group_layout);
    assert_eq!(upload_1x.width(), dim_1x);
    assert_eq!(upload_1x.height(), dim_1x);

    // Step 2: model set_scale_factor(2.0) — replace the CPU atlas with
    // a larger one, then recreate AtlasUpload so the GPU texture +
    // bind group match the new dimensions.
    let dim_2x = sonic_shared::render::atlas_dim_for_scale(2.0);
    assert!(dim_2x > dim_1x, "2x atlas dim must exceed 1x for the regression to be meaningful");
    let atlas_2x = GlyphAtlas::new(dim_2x, dim_2x);
    let upload_2x = AtlasUpload::new(&device, &queue, &atlas_2x, &pipeline.bind_group_layout);

    assert_eq!(
        upload_2x.width(),
        atlas_2x.width(),
        "AtlasUpload width must track the new GlyphAtlas after scale change"
    );
    assert_eq!(
        upload_2x.height(),
        atlas_2x.height(),
        "AtlasUpload height must track the new GlyphAtlas after scale change"
    );
    assert_eq!(upload_2x.width(), dim_2x);

    // And sync() against the new atlas must not panic (would OOB if
    // the upload were still sized to dim_1x).
    let mut atlas_2x = atlas_2x;
    upload_2x.sync(&queue, &mut atlas_2x);
}
