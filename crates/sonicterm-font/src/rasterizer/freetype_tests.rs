use super::*;
use crate::rasterizer::colr::DrawOp;

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
    for row in 1..3 {
        for column in 2..5 {
            image.put_pixel(column, row, image::Rgba([8, 16, 32, 64]));
        }
    }
    let raster = bgra_fixture(7, 6, image.as_raw());
    assert_eq!((raster.bearing_x.get(), raster.bearing_y.get()), (-1.0, 8.0));
    assert_eq!((raster.width, raster.height), (3, 2));
    assert!(raster.data.as_chunks::<4>().0.iter().all(|pixel| *pixel == [32, 16, 8, 64]));
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

/// Draw ops tracing the axis-aligned rectangle `left..right` by `top..bottom`.
fn rect_path(left: f32, top: f32, right: f32, bottom: f32) -> Vec<DrawOp> {
    vec![
        DrawOp::MoveTo { to_x: left, to_y: top },
        DrawOp::LineTo { to_x: right, to_y: top },
        DrawOp::LineTo { to_x: right, to_y: bottom },
        DrawOp::LineTo { to_x: left, to_y: bottom },
        DrawOp::ClosePath,
    ]
}

/// Paint ops filling the rectangle with opaque red.
fn opaque_square(left: f32, top: f32, right: f32, bottom: f32) -> Vec<PaintOp> {
    vec![
        PaintOp::PushClip(rect_path(left, top, right, bottom)),
        PaintOp::PaintSolid(SrgbaPixel::rgba(255, 0, 0, 255)),
        PaintOp::PopClip,
    ]
}

/// The 🇯 shape: an unclipped gradient composited `In` over a bounded backdrop. Cairo's `In` is
/// unbounded, so without an outer clip the recording has no finite ink extents.
fn unbounded_in_composite() -> Vec<PaintOp> {
    let color_line = ColorLine {
        color_stops: vec![
            ColorStop { offset: 0.0, color: SrgbaPixel::rgba(0, 0, 255, 255) },
            ColorStop { offset: 1.0, color: SrgbaPixel::rgba(0, 0, 255, 255) },
        ],
        extend: Extend::Pad,
    };
    let mut ops = opaque_square(2.0, 2.0, 6.0, 6.0);
    ops.push(PaintOp::PushGroup);
    ops.push(PaintOp::PaintLinearGradient {
        start_x: 0.0,
        start_y: 0.0,
        end_x: 10.0,
        end_y: 0.0,
        rotation_x: 0.0,
        rotation_y: 10.0,
        color_line,
    });
    ops.push(PaintOp::PopGroup(Operator::In));
    ops
}

/// Alpha of the pixel at (`x_px`, `y_px`) in an RGBA glyph.
fn alpha_at(glyph: &RasterizedGlyph, x_px: usize, y_px: usize) -> u8 {
    glyph.data[(y_px * glyph.width + x_px) * 4 + 3]
}

/// Axis-aligned clip corners for the rectangle `left..right` by `top..bottom`.
fn rect_clip(left: f64, top: f64, right: f64, bottom: f64) -> [(f64, f64); 4] {
    [(left, bottom), (left, top), (right, top), (right, bottom)]
}

#[test]
fn a_clip_box_bounds_a_composite_cairo_cannot() {
    // The 🇯 regression: an unbounded `In` composite has no finite extents and failed with -1x-1.
    // Its ClipBox bounds it, and the artwork stays visible inside the backdrop, transparent outside.
    let unclipped = rasterize_from_ops(unbounded_in_composite(), 1.0, 1.0, None);
    assert!(
        matches!(&unclipped, Err(err) if err.to_string().contains("invalid color glyph extents"))
    );

    let clip = rect_clip(0.0, 0.0, 10.0, 10.0);
    let glyph = rasterize_from_ops(unbounded_in_composite(), 1.0, 1.0, Some(clip)).unwrap();
    assert_eq!((glyph.width, glyph.height), (10, 10));
    assert_eq!((glyph.bearing_x.get(), glyph.bearing_y.get()), (0.0, 0.0));
    assert_eq!(alpha_at(&glyph, 4, 4), 255, "inside the backdrop the gradient shows");
    // Source-in keeps the gradient's colour, opaque blue, not the red backdrop.
    let interior = (4 * glyph.width + 4) * 4;
    assert_eq!(&glyph.data[interior..interior + 4], &[0, 0, 255, 255]);
    assert!(glyph.has_color);
    assert_eq!(alpha_at(&glyph, 8, 8), 0, "outside the backdrop `In` leaves nothing");
}

#[test]
fn a_skewed_clip_box_clips_to_its_quad_not_its_bounding_box() {
    // An italic ClipBox is a parallelogram. A pixel inside its bounding box but outside the
    // parallelogram must stay transparent; replacing the quad with its bounds fails this.
    let clip = [(0.0, 10.0), (4.0, 0.0), (14.0, 0.0), (10.0, 10.0)];
    let glyph =
        rasterize_from_ops(opaque_square(-5.0, -5.0, 20.0, 20.0), 1.0, 1.0, Some(clip)).unwrap();
    assert_eq!((glyph.width, glyph.height), (14, 10));
    assert_eq!(alpha_at(&glyph, 7, 5), 255, "inside the parallelogram");
    assert_eq!(alpha_at(&glyph, 0, 0), 0, "inside the bounds, left of the slanted edge");
    assert_eq!(alpha_at(&glyph, 13, 9), 0, "inside the bounds, right of the slanted edge");
}

#[test]
fn pixel_bounds_round_fractional_extents_outward() {
    // Truncating would drop a fractional edge pixel, or collapse a sliver to nothing.
    assert_eq!(
        pixel_bounds(-0.5, -1.25, 3.0, 2.5).unwrap(),
        PixelBounds { left: -1, top: -2, width: 4, height: 4 }
    );
    assert_eq!(
        pixel_bounds(2.25, 0.5, 0.25, 1.0).unwrap(),
        PixelBounds { left: 2, top: 0, width: 1, height: 2 }
    );
}

#[test]
fn pixel_bounds_reject_unbounded_or_oversized_extents() {
    // Cairo's unbounded sentinel, non-finite values and spans past the glyph limit are errors,
    // reported before any allocation. Zero area is a valid empty glyph.
    assert!(pixel_bounds(-8388608.0, -8388608.0, -1.0, -1.0).is_err());
    assert!(pixel_bounds(f64::NAN, 0.0, 1.0, 1.0).is_err());
    assert!(pixel_bounds(0.0, 0.0, f64::INFINITY, 1.0).is_err());
    let limit = MAX_RASTERIZED_GLYPH_DIMENSION as f64;
    assert!(pixel_bounds(0.0, 0.0, limit + 1.0, 1.0).is_err());
    assert_eq!(
        pixel_bounds(5.5, 7.0, 0.0, 3.0).unwrap(),
        PixelBounds { left: 0, top: 0, width: 0, height: 0 }
    );
}

#[test]
fn the_glyph_limit_bounds_the_bitmap_not_its_origin() {
    // A small glyph far from the pen still fits: only its size is limited.
    assert_eq!(
        pixel_bounds(131073.0, -200000.0, 1.0, 1.0).unwrap(),
        PixelBounds { left: 131073, top: -200000, width: 1, height: 1 }
    );
}

#[test]
fn a_non_finite_clip_box_corner_is_rejected() {
    // A corner FreeType could never produce must not reach Cairo as a clip path.
    let mut clip = rect_clip(0.0, 0.0, 10.0, 10.0);
    clip[2].0 = f64::NAN;
    let result = rasterize_from_ops(opaque_square(0.0, 0.0, 4.0, 4.0), 1.0, 1.0, Some(clip));
    assert!(
        matches!(&result, Err(err) if err.to_string().contains("invalid color glyph clip box"))
    );
}

#[test]
fn ink_outside_the_clip_box_is_empty_and_an_oversized_fill_is_rejected() {
    // Ink wholly outside the ClipBox yields the empty glyph; a fill covering a huge ClipBox is
    // rejected by the glyph limit before the target surface is allocated.
    let disjoint = rasterize_from_ops(
        opaque_square(20.0, 20.0, 24.0, 24.0),
        1.0,
        1.0,
        Some(rect_clip(0.0, 0.0, 10.0, 10.0)),
    )
    .unwrap();
    assert_eq!((disjoint.width, disjoint.height), (0, 0));
    assert!(disjoint.data.is_empty());

    let huge = rect_clip(0.0, 0.0, 3000.0, 3000.0);
    let oversized =
        rasterize_from_ops(opaque_square(-1.0, -1.0, 3001.0, 3001.0), 1.0, 1.0, Some(huge));
    assert!(matches!(&oversized, Err(err) if err.to_string().contains("glyph limit")));
}

#[test]
fn a_bounded_glyph_inside_its_clip_box_renders_unchanged() {
    // The ClipBox must not change a glyph that already rendered: same pixels and bearings, for ink
    // left of the origin (negative bearing) and right of it (the bearing rule clamps to 0).
    for (left, expect_bearing_x) in [(-3.0_f32, -3.0), (2.0, 0.0)] {
        let ops = opaque_square(left, -2.0, left + 4.0, 3.0);
        let plain = rasterize_from_ops(ops.clone(), 1.0, 1.0, None).unwrap();
        let clipped =
            rasterize_from_ops(ops, 1.0, 1.0, Some(rect_clip(-20.0, -20.0, 20.0, 20.0))).unwrap();
        assert_eq!((plain.width, plain.height), (4, 5), "left={left}");
        assert_eq!((clipped.width, clipped.height), (plain.width, plain.height), "left={left}");
        assert_eq!(clipped.data, plain.data, "left={left}");
        assert_eq!(clipped.bearing_x.get(), expect_bearing_x, "left={left}");
        assert_eq!(plain.bearing_x.get(), expect_bearing_x, "left={left}");
        assert_eq!((clipped.bearing_y.get(), plain.bearing_y.get()), (2.0, 2.0), "left={left}");
    }
}

#[test]
fn clip_box_corners_stay_in_device_pixels_under_the_paint_scale() {
    // FreeType's ClipBox is 26.6 device pixels, y up. The production conversion divides by 64 and
    // flips y, and the clip is set before the paint scale, so the scale never moves it.
    let corner = |x_26_6: i64, y_26_6: i64| ::freetype::FT_Vector {
        x: ::freetype::FT_Pos::from_font_units(x_26_6 as _),
        y: ::freetype::FT_Pos::from_font_units(y_26_6 as _),
    };
    let clip_box = FT_ClipBox_ {
        bottom_left: corner(64, 64),
        top_left: corner(64, 256),
        top_right: corner(448, 256),
        bottom_right: corner(448, 64),
    };
    let corners = clip_box_corners_px(&clip_box);
    assert_eq!(corners, [(1.0, -1.0), (1.0, -4.0), (7.0, -4.0), (7.0, -1.0)]);

    // A fill far larger than the clip, under the production scale sign convention (x positive,
    // y negative): the result is exactly the clip, wherever the scale would have put it.
    let scale = 16.0 / 13.0;
    let glyph = rasterize_from_ops(
        opaque_square(-100.0, -100.0, 100.0, 100.0),
        scale,
        -scale,
        Some(corners),
    )
    .unwrap();
    assert_eq!((glyph.width, glyph.height), (6, 3));
    assert_eq!((glyph.bearing_x.get(), glyph.bearing_y.get()), (0.0, 4.0));
}
