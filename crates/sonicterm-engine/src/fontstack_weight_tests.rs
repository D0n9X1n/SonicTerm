//! Regression contracts for the native operations owned by this sibling.

use super::*;

// Identity and endpoint coverage remain exact while intermediate coverage follows the weight control.
#[test]
fn weight_scale_preserves_identity_and_extremes() {
    let original = vec![0, 1, 64, 128, 254, 255];
    let mut coverage = original.clone();
    apply_weight_scale(&mut coverage, 1.0, false);
    assert_eq!(coverage, original);

    let mut stronger = original.clone();
    apply_weight_scale(&mut stronger, 1.1, false);
    assert_eq!(stronger[0], 0);
    assert_eq!(*stronger.last().unwrap(), 255);
    assert!(stronger[2] > original[2]);
    assert!(stronger[3] > original[3]);

    let mut lighter = original.clone();
    apply_weight_scale(&mut lighter, 0.9, false);
    assert!(lighter[2] < original[2]);
    assert!(lighter[3] < original[3]);
}

// RGB remapping rebuilds the alpha envelope without adding ink to empty pixels.
#[test]
fn subpixel_weight_scale_recomputes_alpha_from_rgb_coverage() {
    let mut coverage = vec![32, 64, 96, 96, 0, 0, 0, 0];
    apply_weight_scale(&mut coverage, 1.1, true);
    assert_eq!(coverage[3], coverage[0].max(coverage[1]).max(coverage[2]));
    assert_eq!(&coverage[4..], &[0, 0, 0, 0]);
}

/// Nonfinite and unsupported weight scales preserve native coverage through the identity fallback.
#[test]
fn invalid_weight_scales_fall_back_to_identity() {
    for scale in [f32::NAN, f32::INFINITY, 0.0, 0.49, 5.01] {
        assert_eq!(sanitize_weight_scale(scale), 1.0);
    }
    assert_eq!(sanitize_weight_scale(1.1), 1.1);
    assert_eq!(sanitize_weight_scale(5.0), 5.0);
}

/// The coverage remap saturates: a stem pixel already at 255 cannot get
/// darker, which is why `weight_scale` was close to invisible at HiDPI where
/// stem cores are solid. Outline growth is the part that adds real ink, so it
/// must reach pixels the remap can never touch.
#[test]
fn embolden_puts_ink_where_the_coverage_remap_cannot() {
    // Single solid pixel surrounded by empty space.
    let coverage = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];

    // The remap leaves every zero pixel at zero, at any scale in range.
    let mut remapped = coverage.clone();
    apply_weight_scale(&mut remapped, 5.0, false);
    assert_eq!(remapped, coverage, "gamma remap cannot create ink");

    // Dilation spreads into those same pixels, inside the tile it was given.
    // The dimensions must not move: a weight control that resizes glyphs makes
    // every character change size when the user asks for more ink, and glyphs
    // from different fonts change by different amounts.
    let (grown, width, height, pad) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("radius 1.0 must dilate");
    assert_eq!(
        (width, height, pad),
        (3, 3, 0),
        "the tile keeps its dimensions and its origin, so the glyph gains weight without \
         gaining size"
    );
    let center = (height / 2) * width + (width / 2);
    assert_eq!(grown[center], 255);
    assert!(grown[center - 1] > 0, "ink must spread horizontally");
    assert!(grown[center + 1] > 0, "ink must spread horizontally");
    assert!(grown[center - width] > 0, "ink must spread vertically");
    assert!(grown[center + width] > 0, "ink must spread vertically");
}

/// Growth is independent of how much spare bitmap margin a glyph carries.
///
/// FreeType returns a tight control box. Flat-sided glyphs commonly touch one
/// edge while curved glyphs retain antialiasing margin, so using the nearest
/// spare margin as a bound makes the same weight scale act differently by glyph
/// shape. Crop-back is allowed at the touched side; in-bounds neighbours still
/// receive ink and tile geometry stays fixed.
#[test]
fn embolden_grows_an_asymmetric_glyph_that_touches_one_edge() {
    // 5x5 with a vertical stem on the left edge and room to its right.
    let mut coverage = vec![0u8; 25];
    for row in 1..4 {
        coverage[row * 5] = 255;
    }

    let (grown, width, height, pad) =
        embolden_coverage(&coverage, 5, 5, 1.0, false).expect("edge-touching stem must grow");
    assert_eq!((width, height, pad), (5, 5, 0), "weight must not resize or reposition the tile");
    assert!(
        grown[2 * 5 + 1] > 0,
        "the left-edge stem must spread into its in-bounds neighbour even though outward ink is cropped"
    );
}

/// Outline growth never exceeds its one-pixel bound regardless of glyph shape.
#[test]
fn embolden_uses_a_literal_one_pixel_shape_independent_ceiling() {
    const EXPECTED_CEILING_PX: f64 = 1.0;
    assert_eq!(
        MAX_EMBOLDEN_RADIUS_PX, EXPECTED_CEILING_PX,
        "the documented crop-back ceiling is one raster pixel"
    );
    let inset = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];
    let (at_ceiling, width, height, pad) =
        embolden_coverage(&inset, 3, 3, EXPECTED_CEILING_PX, false)
            .expect("the one-pixel ceiling permits growth");
    assert_eq!((width, height, pad), (3, 3, 0));
    let (above_ceiling, ..) =
        embolden_coverage(&inset, 3, 3, 5.0, false).expect("large radius is capped");
    assert_eq!(
        at_ceiling, above_ceiling,
        "radii above one pixel must produce the same bounded crop-back result"
    );
}

#[test]
fn embolden_radius_scales_with_weight_and_is_zero_at_or_below_identity() {
    for scale in [0.5, 0.9, 1.0] {
        assert_eq!(embolden_radius_px(scale, 30.0), 0.0, "scale {scale} must not grow outlines");
    }
    let at_two = embolden_radius_px(2.0, 30.0);
    let at_five = embolden_radius_px(5.0, 30.0);
    assert!(at_two > 0.0);
    assert!(at_five > at_two, "higher weight must grow more");
    // Radius tracks cell height so the effect holds its proportion across DPI.
    assert!(embolden_radius_px(2.0, 60.0) > at_two);
    // Degenerate metrics disable growth instead of guessing.
    assert_eq!(embolden_radius_px(2.0, 0.0), 0.0);
    assert_eq!(embolden_radius_px(2.0, f64::NAN), 0.0);
}

/// Invalid bitmap dimensions and oversized growth are refused before mutation.
#[test]
fn embolden_declines_work_it_cannot_do_safely() {
    let coverage = vec![255; 9];
    assert!(embolden_coverage(&coverage, 3, 3, 0.0, false).is_none(), "no radius, no work");
    assert!(embolden_coverage(&[], 0, 0, 1.0, false).is_none(), "empty glyph");
    assert!(embolden_coverage(&[0u8; 8], 3, 3, 1.0, false).is_none(), "short buffer");
    assert!(embolden_coverage(&[0u8; 10], 3, 3, 1.0, false).is_none(), "long buffer");
    assert!(embolden_coverage(&[0u8; 2], usize::MAX, 2, 1.0, false).is_none());

    let over = MAX_RASTERIZED_GLYPH_DIMENSION + 1;
    assert!(embolden_coverage(&vec![0u8; over], over, 1, 1.0, false).is_none());
}

/// A final bitmap exactly at the dimension limit remains admissible.
#[test]
fn embolden_accepts_a_legal_final_tile_at_the_dimension_limit() {
    let width = MAX_RASTERIZED_GLYPH_DIMENSION;
    let mut coverage = vec![0u8; width];
    coverage[width / 2] = 255;

    let (grown, grown_width, grown_height, pad) =
        embolden_coverage(&coverage, width, 1, 1.0, false)
            .expect("bounded scratch padding must not reject a legal cropped result");

    assert_eq!((grown_width, grown_height, pad), (width, 1, 0));
    assert_eq!(grown.len(), coverage.len());
    assert!(grown[width / 2 - 1] > 0);
}

#[test]
fn embolden_recomputes_subpixel_alpha_from_dilated_rgb() {
    // 4x3 BGRA with a single lit pixel near the centre. Subpixel channels are
    // dilated independently and alpha is rebuilt from their envelope.
    let mut coverage = vec![0u8; 4 * 3 * 4];
    let (row, col, tile_w, bytes_per_px) = (1usize, 1usize, 4usize, 4usize);
    let centre = (row * tile_w + col) * bytes_per_px;
    coverage[centre..centre + 4].copy_from_slice(&[200, 100, 50, 200]);
    let (grown, width, height, _) =
        embolden_coverage(&coverage, 4, 3, 1.0, true).expect("subpixel dilation");
    assert_eq!(grown.len(), width * height * 4);
    for px in grown.as_chunks::<4>().0 {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}

/// Fractional radii blend the outer ring proportionally, so growth ramps
/// smoothly instead of snapping a whole pixel at a time as weight increases.
#[test]
fn embolden_fractional_radius_blends_rather_than_snapping() {
    let coverage = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];
    let (half, half_width, _, _) =
        embolden_coverage(&coverage, 3, 3, 0.5, false).expect("half radius");
    let (full, full_width, _, _) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("full radius");
    let half_neighbor = half[(half.len() / half_width / 2) * half_width + half_width / 2 + 1];
    let full_neighbor = full[(full.len() / full_width / 2) * full_width + full_width / 2 + 1];
    assert!(half_neighbor > 0, "fractional radius still spreads ink");
    assert!(half_neighbor < full_neighbor, "half radius must spread less than full");
}

/// The saturation ceiling cuts both ways: gamma cannot lighten a pixel that is
/// already fully opaque, so below 1.0 the outline has to shrink for thinning to
/// reach a solid stem core.
#[test]
fn erosion_removes_ink_the_coverage_remap_cannot() {
    // 5x5 with a solid 3x3 core — a stem thick enough to have an interior.
    let mut coverage = vec![0u8; 25];
    for row in 1..4 {
        for column in 1..4 {
            coverage[row * 5 + column] = 255;
        }
    }

    // The remap leaves every 255 exactly where it was, at any scale in range.
    let mut remapped = coverage.clone();
    apply_weight_scale(&mut remapped, 0.5, false);
    assert_eq!(remapped, coverage, "gamma remap cannot erode a solid core");

    // Erosion eats the rim of that core.
    let eroded = erode_coverage(&coverage, 5, 5, 1.0, false).expect("radius 1.0 must erode");
    let before: u32 = coverage.iter().map(|&byte| u32::from(byte)).sum();
    let after: u32 = eroded.iter().map(|&byte| u32::from(byte)).sum();
    assert!(after < before, "erosion must remove ink: {before} -> {after}");
    // The centre of a 3x3 core survives a radius-1 erosion; its rim does not.
    assert_eq!(eroded[2 * 5 + 2], 255, "core centre must survive");
    assert_eq!(eroded[5 + 1], 0, "core corner must erode");
}

/// A stem wide enough to have an interior must keep opaque pixels under a
/// light thin. Guards against an over-eager erosion that washes glyphs out
/// instead of slimming them.
#[test]
fn light_erosion_preserves_the_interior_of_a_thick_stem() {
    // 9x9 fully solid: every interior pixel is far from an edge.
    let coverage = vec![255u8; 81];
    let eroded = erode_coverage(&coverage, 9, 9, 0.05, false).expect("sub-pixel erosion");
    let centre = eroded[4 * 9 + 4];
    assert_eq!(centre, 255, "a sub-pixel thin must not touch a deep interior pixel");
    assert!(
        eroded.iter().filter(|&&byte| byte == 255).count() > 20,
        "most of a solid block must stay opaque under a light thin"
    );
}

/// Thinning follows the weight scale and never erodes identity or heavier requests.
#[test]
fn thin_radius_scales_with_weight_and_is_zero_at_or_above_identity() {
    for scale in [1.0, 1.5, 5.0] {
        assert_eq!(thin_radius_px(scale, 30.0), 0.0, "scale {scale} must not erode");
    }
    let at_09 = thin_radius_px(0.9, 30.0);
    let at_05 = thin_radius_px(0.5, 30.0);
    assert!(at_09 > 0.0);
    assert!(at_05 > at_09, "lower weight must erode more");
    assert_eq!(thin_radius_px(0.9, 0.0), 0.0);
    assert_eq!(thin_radius_px(0.9, f64::NAN), 0.0);
}

#[test]
fn erosion_declines_work_it_cannot_do_safely() {
    let coverage = vec![255u8; 9];
    assert!(erode_coverage(&coverage, 3, 3, 0.0, false).is_none(), "no radius, no work");
    assert!(erode_coverage(&[], 0, 0, 1.0, false).is_none(), "empty glyph");
    // Length that disagrees with the declared geometry is refused rather than
    // indexed past the end, in either direction.
    assert!(erode_coverage(&[0u8; 5], 3, 3, 1.0, false).is_none(), "short buffer");
    assert!(erode_coverage(&[0u8; 10], 3, 3, 1.0, false).is_none(), "long buffer");
    assert!(erode_coverage(&[0u8; 2], usize::MAX, 2, 1.0, false).is_none());
}

#[test]
fn erosion_keeps_tile_geometry_and_subpixel_alpha_consistent() {
    // Erosion only removes ink, so the buffer length must be preserved
    // exactly — the caller relies on dimensions and offsets staying valid.
    let coverage = vec![200u8; 4 * 4 * 4];
    let eroded = erode_coverage(&coverage, 4, 4, 0.5, true).expect("subpixel erosion");
    assert_eq!(eroded.len(), coverage.len(), "erosion must not resize the tile");
    for px in eroded.as_chunks::<4>().0 {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}
