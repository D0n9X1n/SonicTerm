use super::*;

// Half-open ink bounds preserve opaque bitmaps, single pixels, and each final row/column.
#[test]
fn crop_preserves_complete_ink_extents() {
    for (width, height) in [(3, 3), (1, 3), (3, 1), (1, 1)] {
        let mut image = ImageBuffer::from_pixel(width, height, Rgba([10, 20, 30, 255]));
        let cropped = crop_to_non_transparent(&mut image).to_image();
        assert_eq!(cropped.dimensions(), (width, height));
        assert!(cropped.pixels().all(|pixel| *pixel == Rgba([10, 20, 30, 255])));
    }
    let mut image = ImageBuffer::from_pixel(5, 7, Rgba([0, 0, 0, 0]));
    image.put_pixel(4, 6, Rgba([10, 20, 30, 255]));
    let cropped = crop_to_non_transparent(&mut image).to_image();
    assert_eq!(cropped.dimensions(), (1, 1));
    assert_eq!(cropped.get_pixel(0, 0), &Rgba([10, 20, 30, 255]));
}

// A transparent bitmap is a valid blank raster, not the zero-size missing-glyph sentinel.
#[test]
fn transparent_crop_preserves_blank_dimensions_and_pixels() {
    let mut image = ImageBuffer::from_pixel(5, 7, Rgba([0, 0, 0, 0]));
    let cropped = crop_to_non_transparent(&mut image).to_image();
    assert_eq!(cropped.dimensions(), (5, 7));
    assert_eq!(cropped.as_raw(), image.as_raw());
}

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
