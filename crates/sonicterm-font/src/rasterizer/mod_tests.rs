use super::*;

#[test]
fn glyph_rgba_size_accepts_atlas_limit() {
    assert_eq!(
        checked_glyph_rgba_len(MAX_RASTERIZED_GLYPH_DIMENSION, MAX_RASTERIZED_GLYPH_DIMENSION)
            .expect("atlas-sized glyph is valid"),
        2048 * 2048 * 4
    );
}

#[test]
fn glyph_rgba_size_rejects_oversized_and_overflowing_bounds() {
    assert!(checked_glyph_rgba_len(MAX_RASTERIZED_GLYPH_DIMENSION + 1, 1).is_err());
    assert!(checked_glyph_rgba_len(1, MAX_RASTERIZED_GLYPH_DIMENSION + 1).is_err());
    assert!(checked_glyph_rgba_len(usize::MAX, usize::MAX).is_err());
}

#[test]
fn freetype_26_6_extent_rejects_oversized_native_render() {
    assert_eq!(
        checked_freetype_26_6_extent((MAX_RASTERIZED_GLYPH_DIMENSION as i64) * 64)
            .expect("atlas-sized outline is valid"),
        MAX_RASTERIZED_GLYPH_DIMENSION
    );
    assert!(
        checked_freetype_26_6_extent(((MAX_RASTERIZED_GLYPH_DIMENSION + 1) as i64) * 64).is_err()
    );
    assert!(checked_freetype_26_6_extent(i64::MIN).is_err());
}

#[test]
fn raster_pixel_size_rejects_unbounded_font_requests() {
    assert_eq!(checked_raster_pixel_size(14.0, 1.0, 72).expect("normal size"), 14.0);
    assert!(checked_raster_pixel_size(f64::INFINITY, 1.0, 72).is_err());
    assert!(checked_raster_pixel_size(4096.0, 1.0, 72).is_err());
}
