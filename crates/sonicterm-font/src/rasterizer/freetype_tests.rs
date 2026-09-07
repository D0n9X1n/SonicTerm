use super::*;

fn bgra_fixture(width: u32, height: u32, data: &[u8]) -> RasterizedGlyph {
    let mut slot: FT_GlyphSlotRec_ =
        // SAFETY: the C slot permits null pointers; rasterize_bgra reads only the bitmap metadata initialized below.
        unsafe { std::mem::zeroed() };
    slot.bitmap.width = width as _;
    slot.bitmap.rows = height as _;
    slot.bitmap_left = -3;
    slot.bitmap_top = 9;
    FreeTypeRasterizer::rasterize_bgra(width as usize * 4, &slot, data, true, true).unwrap()
}

// Removed margins translate bearings independently of the cropped dimensions and channel swap.
#[test]
fn bgra_crop_translates_bearings_and_preserves_premultiplied_ink() {
    let mut image = image::ImageBuffer::from_pixel(7, 6, image::Rgba([0, 0, 0, 0]));
    for y in 1..3 {
        for x in 2..5 {
            image.put_pixel(x, y, image::Rgba([8, 16, 32, 64]));
        }
    }
    let raster = bgra_fixture(7, 6, image.as_raw());
    assert_eq!((raster.bearing_x.get(), raster.bearing_y.get()), (-1.0, 8.0));
    assert_eq!((raster.width, raster.height), (3, 2));
    assert!(raster.data.chunks_exact(4).all(|pixel| pixel == [32, 16, 8, 64]));
    assert!(raster.has_color && raster.is_scaled);
}

// An uncropped blank or opaque glyph keeps native bearings and valid owned RGBA storage.
#[test]
fn bgra_no_crop_preserves_metrics_and_blank_policy() {
    for pixel in [[0, 0, 0, 0], [8, 16, 32, 64]] {
        let source = pixel.repeat(9);
        let raster = bgra_fixture(3, 3, &source);
        assert_eq!((raster.width, raster.height), (3, 3));
        assert_eq!((raster.bearing_x.get(), raster.bearing_y.get()), (-3.0, 9.0));
        assert_eq!(raster.data, [pixel[2], pixel[1], pixel[0], pixel[3]].repeat(9));
        assert!(raster.has_color && raster.is_scaled);
    }
}

/// The BGRA image owns its bytes before the face-owned source can change or expire.
#[test]
fn bgra_image_copies_borrowed_glyph_bytes() {
    let mut source = vec![1, 2, 3, 4];
    let image = owned_bgra_image(1, 1, &source).expect("one BGRA pixel builds an image");

    source.fill(0);

    assert_eq!(image.as_raw(), &[1, 2, 3, 4]);
}
