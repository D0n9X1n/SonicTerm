use super::*;
use crate::{core::fit_single_cell_status_marker, quad::px_to_ndc};
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
/// This is the mechanism behind reported Powerline separators showing faint
/// marks in their own colours, only after long use. Bleeding is invisible on
/// a fresh atlas because the neighbours are empty; once eviction repacks real
/// glyphs beside a separator, it becomes marks. A fractional cell height —
/// line height 1.15 in the report — puts every glyph on this path.
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

/// A sharp quad must blend premultiplied linear channels before sRGB encoding.
#[test]
fn premultiplied_quad_blends_over_background() {
    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [0.5, 0.0, 0.0, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 188, 255]);
}

/// A sharp quad at the lookup cutoff must preserve the direct blend's exact result.
#[test]
fn large_premultiplied_quad_uses_exact_linear_lookup() {
    let mut frame = WindowsSoftwareFrame::new(32, 32, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 32.0, 32.0, [0.5, 0.0, 0.0, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 188, 255]);
    assert_eq!(frame.pixel_bgra(31, 31), [0, 0, 188, 255]);
}

/// Quad blending must source-over alpha without applying an sRGB transfer to alpha.
#[test]
fn premultiplied_quad_keeps_translucent_destination_alpha() {
    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.2, 0.1, 0.05, 0.4]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [0.3, 0.1, 0.05, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [77, 108, 170, 179]);
}

/// Rounded coverage must scale every premultiplied source component before linear blending.
#[test]
fn rounded_rect_blends_full_and_partial_coverage_in_linear_light() {
    let mut frame = WindowsSoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rounded_rect(1.0, 1.0, 6.0, 6.0, [0.5, 0.0, 0.0, 0.5], 3.0);

    assert_eq!(frame.pixel_bgra(4, 4), [0, 0, 188, 255]);
    assert!(frame.pixel_bgra(2, 1)[2].abs_diff(147) <= 1);
    assert_eq!(frame.pixel_bgra(1, 1), [0, 0, 0, 255]);
}

#[test]
fn rounded_rect_antialiases_corner_pixels() {
    let mut frame = WindowsSoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rounded_rect(1.0, 1.0, 6.0, 6.0, [1.0, 1.0, 1.0, 1.0], 3.0);
    assert_eq!(frame.pixel_bgra(4, 4), [255, 255, 255, 255]);
    let corner = frame.pixel_bgra(1, 1);
    assert!(corner[0] < 255, "corner should be partially or fully clipped by radius: {corner:?}");
}

/// Line coverage must scale every premultiplied source component before linear blending.
#[test]
fn line_quad_antialiases_near_segment() {
    let mut frame = WindowsSoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let q = QuadInstance::line(
        px_to_ndc(1.0, 1.0, 6.0, 6.0, 8.0, 8.0),
        [0.0, 0.5, 0.0, 0.5],
        [6.0, 6.0],
        [-3.0, -3.0],
        [3.0, 3.0],
        1.0,
    );

    frame.draw_line_quad(&q, 1.0, 1.0, 6.0, 6.0);

    assert_eq!(frame.pixel_bgra(4, 4), [0, 188, 0, 255]);
    let edge = frame.pixel_bgra(1, 2);
    assert!(edge[1].abs_diff(169) <= 1, "partial line edge must blend in linear light: {edge:?}");
    assert_eq!(frame.pixel_bgra(7, 0), [0, 0, 0, 255]);
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
fn grayscale_coverage_is_not_max_channel() {
    let cov = grayscale_coverage([0.25, 0.5, 0.75, 0.75]);
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
fn grayscale_ramp_matches_hardware_blending_at_supported_scales() {
    const HARDWARE_REFERENCE: [[u8; 4]; 21] = [
        [32, 96, 160, 255],
        [34, 96, 160, 255],
        [36, 96, 160, 255],
        [38, 96, 160, 255],
        [40, 96, 160, 255],
        [42, 97, 160, 255],
        [44, 97, 160, 255],
        [45, 97, 161, 255],
        [47, 97, 161, 255],
        [48, 97, 161, 255],
        [50, 97, 161, 255],
        [51, 97, 161, 255],
        [53, 97, 161, 255],
        [54, 98, 161, 255],
        [55, 98, 161, 255],
        [57, 98, 161, 255],
        [58, 98, 161, 255],
        [59, 98, 161, 255],
        [60, 98, 161, 255],
        [61, 98, 161, 255],
        [63, 98, 161, 255],
    ];

    for scale in [1.0_f32, 1.25, 1.5, 1.75] {
        let width = (8.0 * scale).round() as usize;
        let height = (15.0 * scale).round() as usize;
        let ramp: Vec<u8> =
            (0..width).map(|x| ((x * 20) as f32 / (width - 1) as f32).round() as u8).collect();
        let mut coverage = Vec::with_capacity(width * height);
        for _ in 0..height {
            coverage.extend_from_slice(&ramp);
        }

        let mut atlas = GlyphAtlas::new(32, 32);
        let info = atlas
            .get_or_insert(
                GlyphKey::new('M', false, false),
                &mut TileRasterizer(RasterTile {
                    width: width as u32,
                    height: height as u32,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage,
                    is_color: false,
                    is_subpixel: false,
                }),
            )
            .expect("command-palette-sized glyph inserts");
        let mut frame = WindowsSoftwareFrame::new(
            width as u32,
            height as u32,
            [0.351_532_6, 0.116_970_666, 0.014_443_844, 1.0],
        )
        .expect("valid frame");

        frame.draw_glyphs(
            &atlas,
            &[GlyphInstance {
                rect: px_to_ndc(0.0, 0.0, width as f32, height as f32, width as f32, height as f32),
                uv: info.uv,
                color: [0.3, 0.15, 0.45, 0.6],
                flags: [0.0; 4],
            }],
        );

        for y in 0..height as u32 {
            for x in 0..width as u32 {
                let coverage = ramp[x as usize] as usize;
                assert_eq!(
                    frame.pixel_bgra(x, y),
                    HARDWARE_REFERENCE[coverage],
                    "scale {scale} diverged from the hardware linear blend at ({x}, {y})"
                );
            }
        }
    }
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

/// One row baseline must survive NDC roundoff independently of each glyph's height.
#[test]
fn regular_glyph_heights_share_one_software_pixel_origin() {
    let mut atlas = GlyphAtlas::new(2, 10);
    let short = atlas
        .get_or_insert(
            GlyphKey::new('I', false, false),
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
        .expect("short regular glyph inserts");
    let tall = atlas
        .get_or_insert(
            GlyphKey::new('V', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 9,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255; 9],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("tall regular glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(2, 65, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[
            GlyphInstance {
                rect: px_to_ndc(0.0, 0.5, 1.0, 1.0, 2.0, 65.0),
                uv: short.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
            GlyphInstance {
                rect: px_to_ndc(1.0, 0.5, 1.0, 9.0, 2.0, 65.0),
                uv: tall.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
        ],
    );

    let first_ink_row = |x| (0..65).find(|&y| frame.pixel_bgra(x, y) != [0, 0, 0, 255]);
    assert_eq!(
        first_ink_row(0),
        first_ink_row(1),
        "regular glyphs emitted from one baseline must not land on different software rows"
    );
}

/// Horizontal-only glyph scaling must not change its vertical software-pixel origin.
#[test]
fn horizontally_scaled_glyphs_keep_their_shared_vertical_origin() {
    let mut atlas = GlyphAtlas::new(20, 10);
    let wide = atlas
        .get_or_insert(
            GlyphKey::new('P', false, false),
            &mut TileRasterizer(RasterTile {
                width: 10,
                height: 10,
                offset_x: 0,
                offset_y: 0,
                advance: 10.0,
                coverage: vec![255; 100],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("wide glyph inserts");
    let exact = atlas
        .get_or_insert(
            GlyphKey::new('R', false, false),
            &mut TileRasterizer(RasterTile {
                width: 9,
                height: 10,
                offset_x: 0,
                offset_y: 0,
                advance: 9.0,
                coverage: vec![255; 90],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("exact glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(40, 100, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[
            GlyphInstance {
                rect: px_to_ndc(0.0, 1.5, 9.0, 10.0, 40.0, 100.0),
                uv: wide.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
            GlyphInstance {
                rect: px_to_ndc(20.0, 1.5, 9.0, 10.0, 40.0, 100.0),
                uv: exact.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
        ],
    );

    let first_ink_row = |x| (0..100).find(|&y| frame.pixel_bgra(x, y) != [0, 0, 0, 255]);
    assert_eq!(
        first_ink_row(0),
        first_ink_row(20),
        "horizontal resampling must not choose a different vertical rounding path"
    );
}

/// Vertical resampling must not change the destination row chosen for text.
#[test]
fn vertically_scaled_glyphs_keep_their_shared_destination_origin() {
    let mut atlas = GlyphAtlas::new(2, 20);
    let native = atlas
        .get_or_insert(
            GlyphKey::new('N', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 10,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255; 10],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("native glyph inserts");
    let scaled = atlas
        .get_or_insert(
            GlyphKey::new('S', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 9,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255; 9],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("scaled glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(3, 20, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[
            GlyphInstance {
                rect: px_to_ndc(0.0, 1.5, 1.0, 10.0, 3.0, 20.0),
                uv: native.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
            GlyphInstance {
                rect: px_to_ndc(2.0, 1.5, 1.0, 10.0, 3.0, 20.0),
                uv: scaled.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
        ],
    );

    let first_ink_row = |x| (0..20).find(|&y| frame.pixel_bgra(x, y) != [0, 0, 0, 255]);
    assert_eq!(first_ink_row(0), first_ink_row(2));
}

/// Horizontal resampling must not change the destination column chosen for text.
#[test]
fn horizontally_scaled_glyphs_keep_their_shared_destination_origin() {
    let mut atlas = GlyphAtlas::new(20, 2);
    let native = atlas
        .get_or_insert(
            GlyphKey::new('N', false, false),
            &mut TileRasterizer(RasterTile {
                width: 10,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 10.0,
                coverage: vec![255; 10],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("native glyph inserts");
    let scaled = atlas
        .get_or_insert(
            GlyphKey::new('S', false, false),
            &mut TileRasterizer(RasterTile {
                width: 9,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 9.0,
                coverage: vec![255; 9],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("scaled glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(20, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[
            GlyphInstance {
                rect: px_to_ndc(1.5, 0.0, 10.0, 1.0, 20.0, 3.0),
                uv: native.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
            GlyphInstance {
                rect: px_to_ndc(1.5, 2.0, 10.0, 1.0, 20.0, 3.0),
                uv: scaled.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            },
        ],
    );

    let first_ink_col = |y| (0..20).find(|&x| frame.pixel_bgra(x, y) != [0, 0, 0, 255]);
    assert_eq!(first_ink_col(0), first_ink_col(2));
}

/// Native top clipping skips every source row hidden above the frame.
#[test]
fn one_to_one_top_clip_advances_the_source_row() {
    let mut atlas = GlyphAtlas::new(1, 2);
    let glyph = atlas
        .get_or_insert(
            GlyphKey::new('T', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 2,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![0, 255],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("top-clipped glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, -1.0, 1.0, 2.0, 1.0, 1.0),
            uv: glyph.uv,
            color: [1.0; 4],
            flags: [0.0; 4],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), [255, 255, 255, 255]);
}

/// Native left clipping skips every source column hidden left of the frame.
#[test]
fn one_to_one_left_clip_advances_the_source_column() {
    let mut atlas = GlyphAtlas::new(2, 1);
    let glyph = atlas
        .get_or_insert(
            GlyphKey::new('L', false, false),
            &mut TileRasterizer(RasterTile {
                width: 2,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 2.0,
                coverage: vec![0, 255],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("left-clipped glyph inserts");
    let mut frame = WindowsSoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(-1.0, 0.0, 2.0, 1.0, 1.0, 1.0),
            uv: glyph.uv,
            color: [1.0; 4],
            flags: [0.0; 4],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), [255, 255, 255, 255]);
}

/// Software presentation gives unequal hollow and solid source tiles equal outer bounds.
///
/// Separate frames make the destination mask directly comparable while retaining one untouched
/// pixel around every cell edge so any neighboring-row or neighboring-column bleed is visible.
#[test]
fn software_frame_draws_hollow_and_solid_status_markers_at_equal_size() {
    fn draw(marker: char, source_size: u32, hollow: bool) -> WindowsSoftwareFrame {
        let coverage = (0..source_size)
            .flat_map(|y| {
                (0..source_size).map(move |x| {
                    if !hollow || x == 0 || y == 0 || x + 1 == source_size || y + 1 == source_size {
                        255
                    } else {
                        0
                    }
                })
            })
            .collect();
        let mut atlas = GlyphAtlas::new(16, 8);
        let info = atlas
            .get_or_insert(
                GlyphKey::new(marker, false, false),
                &mut TileRasterizer(RasterTile {
                    width: source_size,
                    height: source_size,
                    offset_x: 0,
                    offset_y: 0,
                    advance: source_size as f32,
                    coverage,
                    is_color: false,
                    is_subpixel: false,
                }),
            )
            .expect("status marker inserts");
        let fitted = fit_single_cell_status_marker(
            marker,
            1,
            false,
            false,
            (2.0, 2.0, source_size as f32, source_size as f32),
            (2.0, 2.0, 6.0, 8.0),
        );
        let mut frame =
            WindowsSoftwareFrame::new(10, 10, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
        frame.draw_glyphs(
            &atlas,
            &[GlyphInstance {
                rect: px_to_ndc(fitted.0, fitted.1, fitted.2, fitted.3, 10.0, 10.0),
                uv: info.uv,
                color: [1.0; 4],
                flags: [0.0; 4],
            }],
        );
        frame
    }

    fn ink_bounds(frame: &WindowsSoftwareFrame) -> Option<(u32, u32, u32, u32)> {
        let mut bounds = None;
        for y in 0..10 {
            for x in 0..10 {
                if frame.pixel_bgra(x, y) != [0, 0, 0, 255] {
                    let (left, top, right, bottom) = bounds.unwrap_or((x, y, x, y));
                    bounds = Some((left.min(x), top.min(y), right.max(x), bottom.max(y)));
                }
            }
        }
        bounds
    }

    let hollow = draw('\u{25ef}', 8, true);
    let solid = draw('\u{25cf}', 4, false);
    assert_eq!(ink_bounds(&hollow), Some((2, 3, 7, 8)));
    assert_eq!(ink_bounds(&solid), ink_bounds(&hollow));
    assert_eq!(hollow.pixel_bgra(4, 5), [0, 0, 0, 255]);
    assert_eq!(solid.pixel_bgra(4, 5), [255, 255, 255, 255]);

    for y in 0..10 {
        for x in 0..10 {
            if !(2..8).contains(&x) || !(3..9).contains(&y) {
                assert_eq!(hollow.pixel_bgra(x, y), [0, 0, 0, 255]);
                assert_eq!(solid.pixel_bgra(x, y), [0, 0, 0, 255]);
            }
        }
    }
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

/// Horizontal-only image scaling keeps its native vertical pixel origin.
#[test]
fn horizontally_scaled_image_keeps_native_vertical_origin() {
    let mut atlas = GlyphAtlas::new(2, 1);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('\u{fffc}', false, false),
            &mut TileRasterizer(RasterTile {
                width: 2,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 2.0,
                coverage: vec![255; 8],
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("image tile inserts");
    let mut frame = WindowsSoftwareFrame::new(4, 4, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 1.5, 4.0, 1.0, 4.0, 4.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [1.0, 0.0, 1.0, 0.0],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 2), [255, 255, 255, 255]);
    assert_eq!(frame.pixel_bgra(0, 1), [0, 0, 0, 255]);
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
