//! Regression contracts for the native operations owned by this sibling.

use super::super::freetype_tests::{alpha_at, opaque_square, rect_clip, unbounded_in_composite};
use super::*;

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
