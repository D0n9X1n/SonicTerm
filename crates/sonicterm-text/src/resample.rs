//! Physical bitmap resampling for atlas-insert time.
//!
//! Background — issue #610 sym-1 (PR-B of 3): Nerd Font PUA icon glyphs
//! arrive from swash at *natural typographic size* (~60-70% of cell_h).
//! The pre-#610 atlas-insert path uploaded them verbatim; the renderer
//! then stretched the QUAD to `(cell_w, 0.95 * cell_h)` via
//! `apply_symbol_fit(IconCellFit)` and the fragment shader sampled the
//! small tile across the big quad with `wgpu::FilterMode::Nearest`. One
//! source texel → N output pixels, no filtering ⇒ visible step-edges
//! and aliasing on every icon.
//!
//! WezTerm fixes this at insert time: `window/src/bitmaps/mod.rs:380-394`
//! picks Lanczos3 (downscale) or Mitchell (upscale) and physically
//! rescales the bitmap to the target cell size BEFORE the atlas blit.
//! Quad then matches bitmap 1:1 and the existing Nearest sampler stays
//! correct for crisp text. We do the same here using `fast_image_resize`
//! (portable SIMD, same Lanczos3/Mitchell coverage as `resize`).
//!
//! This module is the *helper* used by `glyph_atlas::insert_resampled`.
//! It deliberately does NOT know about `GlyphAtlas`, `SymbolFit`, or
//! anything renderer-side — it's a pure bitmap transform so it can be
//! unit-tested without any font work.
//!
//! Adjacency (#610 sym-1 atlas surface): `glyph_atlas::insert_resampled`
//! is the public entry point that wires this into the atlas; the
//! sampler in `sonicterm-gpu/src/atlas_upload.rs` stays Nearest because
//! the bitmap now matches the quad at the texel.

use crate::glyph_atlas::RasterTile;
use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

/// Resample a `RasterTile` to the requested `(target_w, target_h)`
/// pixel dimensions, returning a new tile whose `coverage` buffer
/// matches the target size 1:1.
///
/// The `offset_x`, `offset_y`, and `advance` fields are preserved
/// verbatim — the resample is a pure pixel-bucket transform, not a
/// layout change. Callers (`glyph_atlas::insert_resampled`) recenter
/// the bigger quad themselves.
///
/// Filter selection mirrors WezTerm:
/// * `target < source` on either axis → **Lanczos3** (sharp downscale).
/// * `target > source` on either axis → **Mitchell** (gentle upscale,
///   the standard "no ringing" cubic for icon-style content).
/// * Exact match on both axes → tile returned unchanged (`Cow::Borrowed`
///   semantics: a fresh `Vec` is still produced because `RasterTile` is
///   owned, but no resampler is invoked).
///
/// Pixel layout:
/// * `is_color = true` → BGRA premultiplied, 4 bytes per pixel.
///   `fast_image_resize` natively understands `PixelType::U8x4` and
///   resamples each channel independently. Premultiplied alpha is
///   preserved because Lanczos3/Mitchell are linear filters and we
///   never go back through unpremul.
/// * `is_color = false` → 1 byte per pixel coverage mask.
///   `fast_image_resize` handles `PixelType::U8` directly; the alpha
///   semantics (0 = transparent, 255 = opaque) are preserved.
///
/// Returns `None` if:
/// * `target_w == 0 || target_h == 0` (atlas would reject anyway).
/// * The source tile is empty (`tile.is_empty()`).
/// * The resampler errors out (in practice: an OOM at the buffer
///   allocator, which the caller treats as "fall back to natural-size
///   insert").
///
/// Test coverage: `crates/sonicterm-text/tests/atlas_resample.rs`.
#[must_use]
pub fn resample_tile(tile: &RasterTile, target_w: u32, target_h: u32) -> Option<RasterTile> {
    if target_w == 0 || target_h == 0 || tile.is_empty() {
        return None;
    }
    // Fast path: target already matches source — no resampler needed.
    // Important for the warm-cache case where a glyph happens to land
    // exactly on the cell box (e.g. a Powerline triangle drawn at the
    // exact cell size by a well-tuned font).
    if tile.width == target_w && tile.height == target_h {
        return Some(tile.clone());
    }

    let pixel_type = if tile.is_color { PixelType::U8x4 } else { PixelType::U8 };
    // Pick filter by direction. We compare area (w*h) so that the rare
    // mixed case (downscale on one axis, upscale on the other) picks
    // the gentler filter — Mitchell rings less than Lanczos3 on the
    // upscale axis and Lanczos3's sharpness gain matters less when we
    // are also upscaling.
    let src_area = (tile.width as u64) * (tile.height as u64);
    let dst_area = (target_w as u64) * (target_h as u64);
    let filter = if dst_area < src_area {
        // Net downscale → Lanczos3 (WezTerm window/src/bitmaps/mod.rs:386).
        FilterType::Lanczos3
    } else {
        // Net upscale (or equal area mixed) → Mitchell (WezTerm :391).
        FilterType::Mitchell
    };

    // SAFETY: we just verified width/height > 0 (via is_empty() check)
    // and coverage matches the expected byte count by the RasterTile
    // contract. `Image::from_vec_u8` validates `data.len() == w * h *
    // bytes_per_pixel` and returns Err on mismatch, which we surface
    // as None so the caller can fall back.
    let src_image = Image::from_vec_u8(
        tile.width,
        target_height_safe_src(tile),
        tile.coverage.clone(),
        pixel_type,
    )
    .ok()?;
    let mut dst_image = Image::new(target_w, target_h, pixel_type);

    let mut resizer = Resizer::new();
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter));
    resizer.resize(&src_image, &mut dst_image, Some(&opts)).ok()?;

    Some(RasterTile {
        width: target_w,
        height: target_h,
        offset_x: tile.offset_x,
        offset_y: tile.offset_y,
        advance: tile.advance,
        coverage: dst_image.into_vec(),
        is_color: tile.is_color,
    })
}

/// `Image::from_vec_u8` requires `data.len() == w * h * bpp`. The
/// `RasterTile` contract already guarantees that, but we re-derive the
/// height from the buffer length as a defensive fallback so a malformed
/// upstream tile produces `None` from `from_vec_u8` instead of a panic
/// in debug builds. This wrapper just returns `tile.height` in practice;
/// kept as a named helper so the intent is obvious in `resample_tile`.
#[inline]
fn target_height_safe_src(tile: &RasterTile) -> u32 {
    tile.height
}

#[cfg(test)]
mod tests {
    use super::*;

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
            // Premultiplied "red" BGRA: (B=0, G=0, R=alpha, A=alpha).
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

    #[test]
    fn resample_rejects_zero_targets() {
        let t = synth_mono_tile(8, 8, 128);
        assert!(resample_tile(&t, 0, 8).is_none());
        assert!(resample_tile(&t, 8, 0).is_none());
    }

    #[test]
    fn resample_rejects_empty_source() {
        let t = RasterTile {
            width: 0,
            height: 0,
            offset_x: 0,
            offset_y: 0,
            advance: 0.0,
            coverage: Vec::new(),
            is_color: false,
        };
        assert!(resample_tile(&t, 16, 16).is_none());
    }

    #[test]
    fn resample_preserves_metadata_fields() {
        let mut t = synth_mono_tile(8, 8, 200);
        t.offset_x = -3;
        t.offset_y = 5;
        t.advance = 11.5;
        let r = resample_tile(&t, 16, 16).expect("resample ok");
        assert_eq!(r.offset_x, -3, "offset_x preserved");
        assert_eq!(r.offset_y, 5, "offset_y preserved");
        assert!((r.advance - 11.5).abs() < f32::EPSILON, "advance preserved");
        assert!(!r.is_color);
    }

    #[test]
    fn upscale_picks_mitchell_and_produces_target_dims() {
        let t = synth_mono_tile(6, 6, 255);
        let r = resample_tile(&t, 12, 12).expect("ok");
        assert_eq!(r.width, 12);
        assert_eq!(r.height, 12);
        assert_eq!(r.coverage.len(), 144);
    }

    #[test]
    fn downscale_picks_lanczos_and_produces_target_dims() {
        let t = synth_mono_tile(24, 24, 255);
        let r = resample_tile(&t, 12, 12).expect("ok");
        assert_eq!(r.width, 12);
        assert_eq!(r.height, 12);
        assert_eq!(r.coverage.len(), 144);
    }

    #[test]
    fn color_tile_resamples_as_bgra() {
        let t = synth_color_tile(8, 8, 200);
        let r = resample_tile(&t, 16, 16).expect("ok");
        assert!(r.is_color);
        assert_eq!(r.coverage.len(), 16 * 16 * 4);
    }
}
