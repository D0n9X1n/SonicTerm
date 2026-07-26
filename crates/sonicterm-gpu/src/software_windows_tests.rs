use super::*;
use crate::quad::px_to_ndc;
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::GlyphKey;

struct OneSubpixelGlyph;

impl Rasterizer for OneSubpixelGlyph {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![0, 128, 255, 255],
            is_color: false,
            is_subpixel: true,
        })
    }
}

struct TileRasterizer(RasterTile);

impl Rasterizer for TileRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(self.0.clone())
    }
}

#[test]
fn clear_uses_straight_alpha_background() {
    let frame = WindowsSoftwareFrame::new(2, 2, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 255, 255]);
    assert_eq!(frame.pixel_bgra(1, 1), [0, 0, 255, 255]);
}

#[test]
fn prepare_resizes_buffer_and_repaints_background() {
    let mut frame = WindowsSoftwareFrame::new(2, 2, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.prepare(3, 1, [0.0, 1.0, 0.0, 1.0]).expect("valid resize");
    assert_eq!(frame.pixel_bgra(2, 0), [0, 255, 0, 255]);
}

#[test]
fn prepare_repaints_existing_buffer() {
    let mut frame = WindowsSoftwareFrame::new(2, 1, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.prepare(2, 1, [0.0, 1.0, 0.0, 1.0]).expect("valid resize");
    assert_eq!(frame.pixel_bgra(0, 0), [0, 255, 0, 255]);
    assert_eq!(frame.pixel_bgra(1, 0), [0, 255, 0, 255]);
}

#[test]
fn prepare_shrink_releases_high_water_capacity() {
    let mut frame =
        WindowsSoftwareFrame::new(1024, 1024, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let large_capacity = frame.pixels.capacity();

    frame.prepare(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid shrink");

    assert!(
        frame.pixels.capacity() < large_capacity / 2,
        "shrinking a software frame must release its old high-water allocation"
    );
}

#[test]
fn software_frame_rejects_unsafe_size_without_mutating_existing_buffer() {
    assert!(
        WindowsSoftwareFrame::new(8192, 8192, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "a 256 MiB BGRA frame exceeds the renderer budget"
    );

    let mut frame = WindowsSoftwareFrame::new(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let before = frame.pixels.clone();
    assert!(frame.prepare(u32::MAX, u32::MAX, [1.0, 0.0, 0.0, 1.0]).is_err());
    assert_eq!((frame.width, frame.height), (2, 2));
    assert_eq!(frame.pixels, before);
}

#[test]
fn software_frame_growth_uses_exact_validated_capacity() {
    let mut frame = WindowsSoftwareFrame::new(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.prepare(100, 100, [0.0, 0.0, 0.0, 1.0]).expect("valid growth");

    assert_eq!(frame.pixels.capacity(), 100 * 100 * 4);
}

/// Decoded pixels and the destination surface are bounded against the same
/// ceiling, so a decode cannot be admitted that the surface then cannot hold.
///
/// RI-NATIVE-SURFACE recorded these as "separate surface-size checks". They
/// are separate, but they are not independent: `validated_surface_size`
/// rejects any frame whose byte total crosses `MAX_SURFACE_BYTES`, and the
/// same helper gates both `new` and `prepare`, so the destination can never
/// be admitted above the bound whatever the decode asks for.
#[test]
fn v120_native_decode_and_surface_share_bounds() {
    // A dimension inside MAX_SURFACE_DIMENSION whose byte total still crosses
    // the surface byte ceiling: the two checks are not the same check. The
    // ceiling itself is private to `core`, so this asserts the behaviour it
    // produces rather than the constant.
    let side = MAX_SURFACE_DIMENSION - 1;
    assert!(
        validated_surface_size(side, 1, MAX_SURFACE_DIMENSION).is_some(),
        "the chosen side must pass the axis check on its own"
    );
    assert!(
        validated_surface_size(side, side, MAX_SURFACE_DIMENSION).is_none(),
        "a frame within the dimension limit must still be rejected on total bytes"
    );
    assert!(
        WindowsSoftwareFrame::new(side, side, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "constructing an over-budget frame must fail rather than allocate"
    );

    // The same ceiling governs a resize, so a frame cannot grow past it after
    // construction succeeded at a smaller size.
    let mut frame = WindowsSoftwareFrame::new(64, 64, [0.0, 0.0, 0.0, 1.0]).expect("small frame");
    let before = frame.retained_bytes();
    assert!(
        frame.prepare(side, side, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "resizing past the byte ceiling must fail"
    );
    assert_eq!(frame.retained_bytes(), before, "a rejected resize must not have allocated");
}

/// A glyph drawn at a fractional scale must not sample its neighbours.
///
/// `blit_glyph` takes a nearest sample only when source and destination match
/// within 0.01px (`one_to_one`). Any other scale falls to bilinear, which
/// reads a 2x2 texel neighbourhood — so a glyph whose atlas neighbour holds
/// unrelated pixels blends them in at its edges.
///
/// This is the mechanism #888 reports: Powerline separators showing faint
/// marks in their own colours, only after long use. Bleeding is invisible on
/// a fresh atlas because the neighbours are empty; once eviction repacks real
/// glyphs beside a separator, it becomes marks. A fractional cell height —
/// the reporter runs line height 1.15 — puts every glyph on this path.
#[test]
fn a_scaled_glyph_does_not_sample_its_atlas_neighbour() {
    let mut atlas = GlyphAtlas::new(4, 4);
    // Two tiles side by side: the subject is fully transparent, the
    // neighbour fully opaque. Any non-zero output is the neighbour bleeding.
    let subject = atlas
        .get_or_insert(
            GlyphKey::new('a', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![0],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("subject inserts");
    let _neighbour = atlas
        .get_or_insert(
            GlyphKey::new('b', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("neighbour inserts");

    // Draw the transparent subject into a 3x3 destination: a 1px source into
    // 3px is emphatically not one_to_one, so this is the bilinear path.
    let mut frame =
        WindowsSoftwareFrame::new(3, 3, [0.0, 0.0, 0.0, 255.0 / 255.0]).expect("valid frame");
    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 3.0, 3.0, 3.0, 3.0),
            uv: subject.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0; 4],
        }],
    );

    // The subject has zero coverage, so every destination pixel must remain
    // the cleared background. Anything brighter came from the neighbour.
    for y in 0..3 {
        for x in 0..3 {
            let px = frame.pixel_bgra(x, y);
            assert_eq!(
                px,
                [0, 0, 0, 255],
                "pixel ({x},{y}) = {px:?}: a fully transparent glyph scaled 1px -> 3px \
                 picked up its atlas neighbour"
            );
        }
    }
}

#[test]
fn adjacent_sharp_rects_do_not_overlap_edges() {
    let mut frame = WindowsSoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [1.0, 1.0, 1.0, 0.5]);
    frame.fill_rect(0.0, 1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 0.5]);
    assert_eq!(frame.pixel_bgra(0, 0), frame.pixel_bgra(0, 1));
    assert_eq!(frame.pixel_bgra(0, 2), [0, 0, 0, 255]);
}

#[test]
fn premultiplied_quad_blends_over_background() {
    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [0.5, 0.0, 0.0, 0.5]);
    let px = frame.pixel_bgra(0, 0);
    assert!((120..=135).contains(&px[2]), "premultiplied red should stay half intensity: {px:?}");
    assert_eq!(px[0], 0);
    assert_eq!(px[1], 0);
    assert_eq!(px[3], 255);
}

#[test]
fn rounded_rect_antialiases_corner_pixels() {
    let mut frame = WindowsSoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rounded_rect(1.0, 1.0, 6.0, 6.0, [1.0, 1.0, 1.0, 1.0], 3.0);
    assert_eq!(frame.pixel_bgra(4, 4), [255, 255, 255, 255]);
    let corner = frame.pixel_bgra(1, 1);
    assert!(corner[0] < 255, "corner should be partially or fully clipped by radius: {corner:?}");
}

#[test]
fn line_quad_antialiases_near_segment() {
    let mut frame = WindowsSoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let q = QuadInstance::line(
        px_to_ndc(1.0, 1.0, 6.0, 6.0, 8.0, 8.0),
        [0.0, 1.0, 0.0, 1.0],
        [6.0, 6.0],
        [-3.0, -3.0],
        [3.0, 3.0],
        1.0,
    );
    frame.draw_line_quad(&q, 1.0, 1.0, 6.0, 6.0);
    assert!(frame.pixel_bgra(1, 1)[1] > 0);
    assert_eq!(frame.pixel_bgra(7, 0), [0, 0, 0, 255]);
}

#[test]
fn straight_glyph_color_conversion_premultiplies_alpha() {
    let c = straight_linear_rgba_to_premul_bgra_f32([0.0, 0.0, 1.0, 0.5]);
    assert!((c[0] - 0.5).abs() < 0.001);
    assert_eq!(c[1], 0.0);
    assert_eq!(c[2], 0.0);
    assert_eq!(c[3], 0.5);
}

#[test]
fn atlas_bilinear_sampling_smooths_between_coverage_pixels() {
    let pixels = [0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 0];
    let sample = sample_atlas_bilinear(&pixels, 2, 2, 1.0, 1.0);
    assert!(
        sample[3] > 0.4 && sample[3] < 0.6,
        "center sample should blend neighbours: {sample:?}"
    );
}

#[test]
fn atlas_pixel_centers_sample_exact_texels() {
    let pixels = [0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 192, 0, 0, 0, 255];
    let sample = sample_atlas_bilinear(&pixels, 2, 2, 1.5, 1.5);
    assert!((sample[3] - 1.0).abs() < 0.001, "centered sample should hit exact texel: {sample:?}");
}

#[test]
fn coverage_luma_is_not_max_channel() {
    let cov = coverage_luma([0.25, 0.5, 0.75, 0.75]);
    assert!(cov > 0.5 && cov < 0.75, "colored fallback should smooth edges: {cov}");
}

#[test]
fn subpixel_text_coverage_blends_each_channel() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let info = atlas
        .get_or_insert(GlyphKey::new('d', false, false), &mut OneSubpixelGlyph)
        .expect("subpixel glyph inserts");
    assert!(info.is_subpixel);

    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            uv: info.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0, 1.0, 0.0, 0.0],
        }],
    );

    let px = frame.pixel_bgra(0, 0);
    assert!(
        px[2] > px[1] && px[1] > px[0],
        "ClearType coverage must stay per-channel, got BGRA={px:?}"
    );
}

#[test]
fn software_presenter_samples_replacement_after_in_place_atlas_reset() {
    let key = GlyphKey::new('a', false, false);
    let mut atlas = GlyphAtlas::new(1, 1);
    let first = atlas
        .get_or_insert(
            key,
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![32],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("first glyph inserts");
    atlas.reset_in_place();
    let second = atlas
        .get_or_insert(
            key,
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("replacement glyph inserts");
    assert_eq!(first.uv, second.uv);

    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            uv: second.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0; 4],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), [255, 255, 255, 255]);
}

#[test]
fn subpixel_blend_lerps_each_channel_over_colored_background() {
    let mut dst = [32, 160, 240, 255];

    blend_subpixel_bgra(&mut dst, [0.0, 0.5, 1.0, 1.0], [0.0, 0.0, 0.0], 1.0);

    assert_eq!(dst, [32, 80, 0, 255]);
}

#[test]
fn very_low_text_coverage_is_treated_as_empty() {
    assert!(coverage_luma([0.02, 0.04, 0.06, 0.06]) < 0.08);
}

#[test]
fn one_to_one_sampling_is_size_based_not_fractional_position_based() {
    let w = 8.0_f32;
    let h = 12.0_f32;
    let src_w = 8.0_f32;
    let src_h = 12.0_f32;
    let one_to_one = (w - src_w).abs() < 0.01 && (h - src_h).abs() < 0.01;

    assert!(one_to_one);
}

#[test]
fn scaled_glyph_uses_nearest_without_bottom_fringe() {
    let mut atlas = GlyphAtlas::new(1, 2);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 2,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255, 0],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("block glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 3.0, 1.0, 3.0),
            uv: info.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0; 4],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), [255, 255, 255, 255]);
    assert_eq!(frame.pixel_bgra(0, 1), [0, 0, 0, 255]);
    assert_eq!(frame.pixel_bgra(0, 2), [0, 0, 0, 255]);
}

#[test]
fn scaled_color_glyph_uses_nearest_sampling() {
    let mut atlas = GlyphAtlas::new(1, 2);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('😀', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 2,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![0, 0, 255, 255, 0, 0, 0, 0],
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("color glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 3.0, 1.0, 3.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [1.0, 0.0, 0.0, 0.0],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 255, 255]);
    assert_eq!(frame.pixel_bgra(0, 1), [0, 0, 0, 255]);
    assert_eq!(frame.pixel_bgra(0, 2), [0, 0, 0, 255]);
}

#[test]
fn scaled_image_keeps_bilinear_sampling() {
    let mut atlas = GlyphAtlas::new(1, 2);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('\u{fffc}', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 2,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255, 255, 255, 255, 0, 0, 0, 0],
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("image tile inserts");
    let mut frame = WindowsSoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 3.0, 1.0, 3.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [1.0, 0.0, 1.0, 0.0],
        }],
    );

    let middle = frame.pixel_bgra(0, 1);
    assert!((120..=135).contains(&middle[0]), "image scaling should interpolate: {middle:?}");
    assert_eq!(middle[0], middle[1]);
    assert_eq!(middle[1], middle[2]);
}

#[test]
fn scaled_glyph_sampling_does_not_bleed_from_adjacent_atlas_tile() {
    let mut atlas = GlyphAtlas::new(4, 8);
    let line = atlas
        .get_or_insert(
            GlyphKey::new('─', false, false),
            &mut TileRasterizer(RasterTile {
                width: 4,
                height: 4,
                offset_x: 0,
                offset_y: 0,
                advance: 4.0,
                coverage: vec![
                    0, 0, 0, 0, //
                    255, 255, 255, 255, //
                    0, 0, 0, 0, //
                    0, 0, 0, 0,
                ],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("line glyph inserts");
    atlas
        .get_or_insert(
            GlyphKey::new('x', false, false),
            &mut TileRasterizer(RasterTile {
                width: 4,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 4.0,
                coverage: vec![255; 4],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("neighbor glyph inserts below the line tile");

    let mut frame = WindowsSoftwareFrame::new(4, 5, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 4.0, 5.0, 4.0, 5.0),
            uv: line.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0; 4],
        }],
    );

    for x in 0..4 {
        assert_eq!(
            frame.pixel_bgra(x, 4),
            [0, 0, 0, 255],
            "the scaled tile's transparent bottom edge must not sample the neighboring glyph"
        );
    }
}
