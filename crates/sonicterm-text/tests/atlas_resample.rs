//! Integration tests for the atlas-insert bitmap resample path
//! (#610 sym-1, PR-B). Pure CPU — no fonts required.
//!
//! These tests assert the contract of `GlyphAtlas::insert_resampled` +
//! `resample::resample_tile`. They are deliberately font-free so they
//! run identically on macOS / Windows / Linux CI and don't depend on
//! the bundled St.Helens face being present.

use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile};
use sonicterm_text::resample::resample_tile;
use sonicterm_types::GlyphKey;

fn synth_mono_tile(w: u32, h: u32, fill: u8) -> RasterTile {
    RasterTile {
        width: w,
        height: h,
        offset_x: 0,
        offset_y: 0,
        advance: w as f32,
        coverage: vec![fill; (w * h) as usize],
        is_color: false,
    }
}

fn synth_color_tile(w: u32, h: u32, alpha: u8) -> RasterTile {
    let mut data = Vec::with_capacity((w * h) as usize * 4);
    for _ in 0..(w * h) {
        // Premultiplied BGRA, opaque red of `alpha` intensity.
        data.extend_from_slice(&[0, 0, alpha, alpha]);
    }
    RasterTile {
        width: w,
        height: h,
        offset_x: 0,
        offset_y: 0,
        advance: w as f32,
        coverage: data,
        is_color: true,
    }
}

/// At a 2.0 device pixel ratio the cell is 16x32 px and a natural-size
/// 10x12 NF icon tile must be resampled to fill that cell exactly. The
/// post-#610 atlas tile dimensions MUST equal the target box — that's
/// the invariant that lets the Nearest sampler stay correct.
#[test]
fn atlas_resample_bitmap_dims_match_cell_at_2x_dpi() {
    let mut atlas = GlyphAtlas::new(256, 256);
    let key = GlyphKey::new('\u{f0e7}', false, false); // NF lightning
    let natural = synth_mono_tile(10, 12, 200);
    let (target_w, target_h) = (16, 32);

    let info = atlas
        .insert_resampled(key, natural, target_w, target_h)
        .expect("insert_resampled should allocate room in a fresh atlas");

    assert_eq!(
        info.px_size,
        [target_w, target_h],
        "atlas tile must be the cell size, not natural size"
    );
    let lookup = atlas.get(key).expect("post-insert lookup");
    assert_eq!(lookup.px_size, [target_w, target_h]);
}

/// 1.5× DPR (cell 12x24 from logical 8x16). Same invariant as the 2×
/// case — guards against accidentally hard-coding even multiples.
#[test]
fn atlas_resample_bitmap_dims_match_cell_at_1_5x_dpi() {
    let mut atlas = GlyphAtlas::new(256, 256);
    let key = GlyphKey::new('\u{e0a0}', false, false); // NF branch
    let natural = synth_mono_tile(7, 10, 180);
    let (target_w, target_h) = (12, 24);

    let info = atlas.insert_resampled(key, natural, target_w, target_h).expect("insert_resampled");

    assert_eq!(info.px_size, [target_w, target_h]);
    // Sanity: the resampled tile actually landed in the atlas — at
    // least one alpha pixel is non-zero within the UV rect. A bug
    // that resampled to all-zero (e.g. wrong PixelType) would fail
    // here even though the dims look right.
    let any_painted = (0..target_h).any(|dy| {
        (0..target_w).any(|dx| {
            let (x, y) = (
                (info.uv[0] * atlas.width() as f32) as u32 + dx,
                (info.uv[1] * atlas.height() as f32) as u32 + dy,
            );
            atlas.sample(x, y) != 0
        })
    });
    assert!(any_painted, "resampled tile must have at least one painted pixel");
}

/// If the natural-size tile already matches the cell box, the
/// resampler short-circuits and the atlas just inserts the tile
/// verbatim. Dimensions stay matched and we don't pay for a Lanczos
/// pass on the warm-cache "exact size" common case.
#[test]
fn atlas_natural_dims_preserved_when_no_resample_needed() {
    let mut atlas = GlyphAtlas::new(128, 128);
    let key = GlyphKey::new('\u{e0b0}', false, false); // Powerline triangle
    let exact = synth_mono_tile(16, 32, 255);

    let info = atlas.insert_resampled(key, exact.clone(), 16, 32).expect("insert_resampled");
    assert_eq!(info.px_size, [16, 32], "exact-size insert is a pass-through");

    // And the standalone helper agrees: no-op resample preserves all
    // metadata (offsets, advance, is_color) exactly.
    let r = resample_tile(&exact, 16, 32).expect("no-op resample");
    assert_eq!(r.width, exact.width);
    assert_eq!(r.height, exact.height);
    assert_eq!(r.coverage, exact.coverage);
}

/// Color (emoji) tiles round-trip through the resampler with the alpha
/// channel intact. Pre-#610 the IconCellFit policy didn't run on color
/// emoji, but the resampler must still preserve alpha for the
/// renderer's premultiplied-alpha blend invariant.
#[test]
fn resample_preserves_alpha_channel() {
    let src = synth_color_tile(8, 8, 200);
    let r = resample_tile(&src, 16, 16).expect("color resample");
    assert!(r.is_color, "color flag must propagate");
    assert_eq!(r.coverage.len(), 16 * 16 * 4, "BGRA byte count");
    // At least one pixel has non-zero alpha in the resampled output —
    // a buggy resampler that dropped the A channel (e.g. treating it
    // as PixelType::U8x3) would zero this out.
    let any_alpha = r.coverage.chunks_exact(4).any(|px| px[3] != 0);
    assert!(any_alpha, "alpha channel must survive resample");
    // The premultiplied invariant: R <= A for every pixel (because
    // the source was (0,0,A,A) and Mitchell is a linear filter,
    // bounded above by the maximum input). Catches a swap of the
    // PixelType to RGBA-straight semantics.
    for px in r.coverage.chunks_exact(4) {
        assert!(px[2] <= px[3], "premul invariant: R={} A={}", px[2], px[3]);
    }
}
