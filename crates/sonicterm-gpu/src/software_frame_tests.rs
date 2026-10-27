use super::*;
pub(super) use crate::{core::fit_single_cell_status_marker, quad::px_to_ndc};
pub(super) use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
pub(super) use sonicterm_types::GlyphKey;

#[cfg(target_os = "windows")]
#[test]
pub(super) fn native_view_keeps_extent_and_storage_from_the_same_frame() {
    // Native blits borrow one validated allocation; rejected resizes cannot change the view's extent or pixels.
    let mut frame = SoftwareFrame::new(2, 3, [0.0, 0.0, 0.0, 1.0]).unwrap();
    assert!(frame.prepare(8192, 8192, [0.0; 4]).is_err());
    let view = frame.bgra_frame();
    assert_eq!((view.width(), view.height()), (2, 3));
    assert_eq!(view.pixels().len(), 24);
    assert_eq!(view.pixels().as_ptr(), frame.pixels.as_ptr());
    assert!(view.pixels().as_chunks::<4>().0.iter().all(|pixel| *pixel == [0, 0, 0, 255]));
}

/// Destination bubble geometry uses the overlay layer and disappears on the next clean composition.
#[test]
pub(super) fn link_preview_overlay_covers_terminal_and_clears() {
    let atlas = GlyphAtlas::new(1, 1);
    let mut frame = SoftwareFrame::new(20, 20, [0.0, 0.0, 0.0, 1.0]).unwrap();
    let terminal = QuadInstance {
        rect: px_to_ndc(0.0, 0.0, 20.0, 20.0, 20.0, 20.0),
        color: [1.0, 0.0, 0.0, 1.0],
        ..Default::default()
    };
    let preview = QuadInstance {
        rect: px_to_ndc(4.0, 4.0, 10.0, 10.0, 20.0, 20.0),
        color: [0.0, 1.0, 0.0, 1.0],
        ..Default::default()
    };
    frame.draw_layers_with_subpixel_aa(
        &atlas,
        &atlas,
        SubpixelAaMode::Off,
        &[terminal],
        &[],
        &[],
        &[preview],
        &[],
    );
    assert_eq!(frame.pixel_bgra(6, 6), [0, 255, 0, 255]);
    frame.prepare(20, 20, [0.0, 0.0, 0.0, 1.0]).unwrap();
    frame.draw_layers_with_subpixel_aa(
        &atlas,
        &atlas,
        SubpixelAaMode::Off,
        &[terminal],
        &[],
        &[],
        &[],
        &[],
    );
    assert_eq!(frame.pixel_bgra(6, 6), [0, 0, 255, 255]);
}

pub(super) struct OneSubpixelGlyph;

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

pub(super) struct TileRasterizer(pub(super) RasterTile);

impl Rasterizer for TileRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(self.0.clone())
    }
}

/// Clearing encodes the requested linear background into every BGRA pixel.
#[test]
pub(super) fn clear_uses_straight_alpha_background() {
    let frame = SoftwareFrame::new(2, 2, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 255, 255]);
    assert_eq!(frame.pixel_bgra(1, 1), [0, 0, 255, 255]);
}

/// Resizing repaints the new extent with the requested background.
#[test]
pub(super) fn prepare_resizes_buffer_and_repaints_background() {
    let mut frame = SoftwareFrame::new(2, 2, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.prepare(3, 1, [0.0, 1.0, 0.0, 1.0]).expect("valid resize");
    assert_eq!(frame.pixel_bgra(2, 0), [0, 255, 0, 255]);
}

/// A same-size prepare clears previous pixels without requiring a resize.
#[test]
pub(super) fn prepare_repaints_existing_buffer() {
    let mut frame = SoftwareFrame::new(2, 1, [1.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.prepare(2, 1, [0.0, 1.0, 0.0, 1.0]).expect("valid resize");
    assert_eq!(frame.pixel_bgra(0, 0), [0, 255, 0, 255]);
    assert_eq!(frame.pixel_bgra(1, 0), [0, 255, 0, 255]);
}

/// A large shrink releases the old high-water allocation rather than retaining it.
#[test]
pub(super) fn prepare_shrink_releases_high_water_capacity() {
    let mut frame = SoftwareFrame::new(1024, 1024, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let large_capacity = frame.pixels.capacity();

    frame.prepare(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid shrink");

    assert!(
        frame.pixels.capacity() < large_capacity / 2,
        "shrinking a software frame must release its old high-water allocation"
    );
}

/// An invalid resize leaves the existing dimensions and pixels untouched.
#[test]
pub(super) fn software_frame_rejects_unsafe_size_without_mutating_existing_buffer() {
    assert!(
        SoftwareFrame::new(8192, 8192, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "a 256 MiB BGRA frame exceeds the renderer budget"
    );

    let mut frame = SoftwareFrame::new(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    let before = frame.pixels.clone();
    assert!(frame.prepare(u32::MAX, u32::MAX, [1.0, 0.0, 0.0, 1.0]).is_err());
    assert_eq!((frame.width, frame.height), (2, 2));
    assert_eq!(frame.pixels, before);
}

/// Growth allocates only the validated BGRA extent, not geometric spare capacity.
#[test]
pub(super) fn software_frame_growth_uses_exact_validated_capacity() {
    let mut frame = SoftwareFrame::new(2, 2, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.prepare(100, 100, [0.0, 0.0, 0.0, 1.0]).expect("valid growth");

    assert_eq!(frame.pixels.capacity(), 100 * 100 * 4);
}

/// Decoded pixels and the destination surface are bounded against the same
/// ceiling, so a decode cannot be admitted that the surface then cannot hold.
///
/// Decode and destination checks are not independent: `validated_surface_size`
/// rejects any frame whose byte total crosses `MAX_SURFACE_BYTES`, and the
/// same helper gates both `new` and `prepare`, so the destination can never
/// be admitted above the bound whatever the decode asks for.
#[test]
pub(super) fn v120_native_decode_and_surface_share_bounds() {
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
        SoftwareFrame::new(side, side, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "constructing an over-budget frame must fail rather than allocate"
    );

    // The same ceiling governs a resize, so a frame cannot grow past it after
    // construction succeeded at a smaller size.
    let mut frame = SoftwareFrame::new(64, 64, [0.0, 0.0, 0.0, 1.0]).expect("small frame");
    let before = frame.retained_bytes();
    assert!(
        frame.prepare(side, side, [0.0, 0.0, 0.0, 1.0]).is_err(),
        "resizing past the byte ceiling must fail"
    );
    assert_eq!(frame.retained_bytes(), before, "a rejected resize must not have allocated");
}

/// Scaled glyphs use nearest sampling clamped to their own atlas tile; a transparent
/// subject next to an opaque neighbor exposes any sampling outside that tile.
#[test]
pub(super) fn a_scaled_glyph_does_not_sample_its_atlas_neighbour() {
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
    // 3px is not one_to_one, so this exercises scaled nearest sampling.
    let mut frame = SoftwareFrame::new(3, 3, [0.0, 0.0, 0.0, 255.0 / 255.0]).expect("valid frame");
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

/// Adjacent rectangles cover disjoint pixel-center spans without a blended seam.
#[test]
pub(super) fn adjacent_sharp_rects_do_not_overlap_edges() {
    let mut frame = SoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [1.0, 1.0, 1.0, 0.5]);
    frame.fill_rect(0.0, 1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 0.5]);
    assert_eq!(frame.pixel_bgra(0, 0), frame.pixel_bgra(0, 1));
    assert_eq!(frame.pixel_bgra(0, 2), [0, 0, 0, 255]);
}

/// A sharp quad must blend premultiplied linear channels before sRGB encoding.
#[test]
pub(super) fn premultiplied_quad_blends_over_background() {
    let mut frame = SoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [0.5, 0.0, 0.0, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 188, 255]);
}

/// A sharp quad at the lookup cutoff must preserve the direct blend's exact result.
#[test]
pub(super) fn large_premultiplied_quad_uses_exact_linear_lookup() {
    let mut frame = SoftwareFrame::new(32, 32, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 32.0, 32.0, [0.5, 0.0, 0.0, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 188, 255]);
    assert_eq!(frame.pixel_bgra(31, 31), [0, 0, 188, 255]);
}

/// Quad blending must source-over alpha without applying an sRGB transfer to alpha.
#[test]
pub(super) fn premultiplied_quad_keeps_translucent_destination_alpha() {
    let mut frame = SoftwareFrame::new(1, 1, [0.2, 0.1, 0.05, 0.4]).expect("valid frame");

    frame.fill_rect(0.0, 0.0, 1.0, 1.0, [0.3, 0.1, 0.05, 0.5]);

    assert_eq!(frame.pixel_bgra(0, 0), [77, 108, 170, 179]);
}

/// Rounded coverage must scale every premultiplied source component before linear blending.
#[test]
pub(super) fn rounded_rect_blends_full_and_partial_coverage_in_linear_light() {
    let mut frame = SoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.fill_rounded_rect(1.0, 1.0, 6.0, 6.0, [0.5, 0.0, 0.0, 0.5], 3.0);

    assert_eq!(frame.pixel_bgra(4, 4), [0, 0, 188, 255]);
    assert!(frame.pixel_bgra(2, 1)[2].abs_diff(147) <= 1);
    assert_eq!(frame.pixel_bgra(1, 1), [0, 0, 0, 255]);
}

/// A rounded rectangle fills its center while clipping or antialiasing its corners.
#[test]
pub(super) fn rounded_rect_antialiases_corner_pixels() {
    let mut frame = SoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
    frame.fill_rounded_rect(1.0, 1.0, 6.0, 6.0, [1.0, 1.0, 1.0, 1.0], 3.0);
    assert_eq!(frame.pixel_bgra(4, 4), [255, 255, 255, 255]);
    let corner = frame.pixel_bgra(1, 1);
    assert!(corner[0] < 255, "corner should be partially or fully clipped by radius: {corner:?}");
}

/// Line coverage must scale every premultiplied source component before linear blending.
#[test]
pub(super) fn line_quad_antialiases_near_segment() {
    let mut frame = SoftwareFrame::new(8, 8, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
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

/// Bilinear sampling interpolates alpha between the four neighboring texel centers.
#[test]
pub(super) fn atlas_bilinear_sampling_smooths_between_coverage_pixels() {
    let pixels = [0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 0];
    let sample = sample_atlas_bilinear(&pixels, 2, 2, 1.0, 1.0);
    assert!(
        sample[3] > 0.4 && sample[3] < 0.6,
        "center sample should blend neighbours: {sample:?}"
    );
}

/// Sampling an atlas texel center preserves that texel rather than mixing neighbors.
#[test]
pub(super) fn atlas_pixel_centers_sample_exact_texels() {
    let pixels = [0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 192, 0, 0, 0, 255];
    let sample = sample_atlas_bilinear(&pixels, 2, 2, 1.5, 1.5);
    assert!((sample[3] - 1.0).abs() < 0.001, "centered sample should hit exact texel: {sample:?}");
}

/// Ordinary grayscale coverage uses luminance rather than the largest color channel.
#[test]
pub(super) fn grayscale_coverage_is_not_max_channel() {
    let cov = grayscale_coverage([0.25, 0.5, 0.75, 0.75]);
    assert!(cov > 0.5 && cov < 0.75, "colored fallback should smooth edges: {cov}");
}

pub(super) fn render_subpixel_software(
    mode: sonicterm_render_model::boundary::cfg::config::SubpixelAaMode,
    background: [f32; 4],
    foreground: [f32; 4],
) -> [u8; 4] {
    let mut atlas = GlyphAtlas::new(4, 4);
    let info = atlas
        .get_or_insert(GlyphKey::new('d', false, false), &mut OneSubpixelGlyph)
        .expect("subpixel glyph inserts");
    assert!(info.is_subpixel);
    let mut frame = SoftwareFrame::new(1, 1, background).expect("valid frame");
    frame.draw_glyphs_with_subpixel_aa(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            uv: info.uv,
            color: foreground,
            flags: [0.0, 1.0, 0.0, 0.0],
        }],
        mode,
    );
    frame.pixel_bgra(0, 0)
}

/// Terminal and notification layers use identical AA and blending on identical colored backgrounds.
#[test]
pub(super) fn notification_and_terminal_antialias_pixels_match() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let info =
        atlas.get_or_insert(GlyphKey::new('d', false, false), &mut OneSubpixelGlyph).unwrap();
    let glyph = GlyphInstance {
        rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
        uv: info.uv,
        color: crate::color::hex_to_premultiplied_rgba("#202020", 1.0),
        flags: [0.0, 1.0, 0.0, 0.0],
    };
    let background = crate::color::hex_to_premultiplied_rgba("#ff453a", 1.0);
    for mode in [SubpixelAaMode::Off, SubpixelAaMode::Rgb, SubpixelAaMode::Bgr] {
        let mut terminal = SoftwareFrame::new(1, 1, background).unwrap();
        terminal.draw_layers_with_subpixel_aa(&atlas, &atlas, mode, &[], &[], &[glyph], &[], &[]);
        let mut notification = SoftwareFrame::new(1, 1, background).unwrap();
        notification.draw_layers_with_subpixel_aa(
            &atlas,
            &atlas,
            mode,
            &[],
            &[],
            &[],
            &[],
            &[glyph],
        );
        assert_eq!(terminal.pixel_bgra(0, 0), notification.pixel_bgra(0, 0), "{mode:?}");
    }
}

pub(super) fn assert_bgra_within_one(actual: &[u8], expected: &[u8], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}: output lengths differ");
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            actual.abs_diff(expected) <= 1,
            "{context}: channel {index} was {actual}, expected {expected}"
        );
    }
}

pub(super) fn render_software_color_glyph(source: [u8; 4], background: [f32; 4]) -> [u8; 4] {
    let mut atlas = GlyphAtlas::new(1, 1);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('😀', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: source.to_vec(),
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("color glyph inserts");
    let cpu_pixels = atlas.pixels_bgra().to_vec();
    let mut frame = SoftwareFrame::new(1, 1, background).expect("valid frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [1.0, 0.0, 0.0, 0.0],
        }],
    );

    assert_eq!(atlas.pixels_bgra(), cpu_pixels, "software drawing must not rewrite atlas bytes");
    frame.pixel_bgra(0, 0)
}

pub(super) fn render_software_image(
    source_width: u32,
    pixels: &[u8],
    target_width: u32,
    neighbor: Option<[u8; 4]>,
    background: [f32; 4],
) -> Vec<u8> {
    let atlas_width = source_width + u32::from(neighbor.is_some());
    let mut atlas = GlyphAtlas::new(atlas_width, 1);
    let info = atlas
        .get_or_insert_lazy_without_eviction(
            GlyphKey::new('\u{fffc}', false, false),
            source_width,
            1,
            || RasterTile {
                width: source_width,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: source_width as f32,
                coverage: pixels.to_vec(),
                is_color: true,
                is_subpixel: false,
            },
        )
        .expect("image tile inserts");
    if let Some(neighbor) = neighbor {
        atlas
            .get_or_insert_lazy_without_eviction(GlyphKey::new('N', false, false), 1, 1, || {
                RasterTile {
                    width: 1,
                    height: 1,
                    offset_x: 0,
                    offset_y: 0,
                    advance: 1.0,
                    coverage: neighbor.to_vec(),
                    is_color: true,
                    is_subpixel: false,
                }
            })
            .expect("neighbor tile inserts");
    }
    let cpu_pixels = atlas.pixels_bgra().to_vec();
    let mut frame = SoftwareFrame::new(target_width, 1, background).expect("valid image frame");

    frame.draw_images(
        &atlas,
        &[ImageInstance {
            rect_px: [0.0, 0.0, target_width as f32, 1.0],
            uv: info.uv,
            sample_uv: info.uv,
        }],
    );

    assert_eq!(atlas.pixels_bgra(), cpu_pixels, "software drawing must not rewrite atlas bytes");
    (0..target_width).flat_map(|x| frame.pixel_bgra(x, 0)).collect()
}

/// One-to-one translucent image pixels must match real wgpu blending over opaque backgrounds.
#[test]
pub(super) fn software_translucent_images_match_gpu_color_space_and_blending() {
    let black = [0.0, 0.0, 0.0, 1.0];
    let white = [1.0, 1.0, 1.0, 1.0];
    let colored = crate::color::hex_to_premultiplied_rgba("#204080", 1.0);
    let vectors = [
        ("translucent gray image over black", [64, 64, 64, 128], black),
        ("translucent gray image over color", [64, 64, 64, 128], colored),
        ("translucent color image over white", [16, 32, 64, 128], white),
        ("translucent color image over color", [16, 32, 64, 128], colored),
    ];

    for (name, source, background) in vectors {
        let gpu = crate::atlas_upload::render_image_readback_on(1, &source, 1, None, background);
        let software = render_software_image(1, &source, 1, None, background);

        assert_bgra_within_one(&software, &gpu, name);
    }
}

/// One-to-one opaque images must preserve their encoded CPU source bytes.
#[test]
pub(super) fn software_opaque_image_output_is_byte_compatible_with_cpu_atlas() {
    let source = [17, 34, 68, 255];
    let software = render_software_image(1, &source, 1, None, [0.0; 4]);

    assert_eq!(software, source);
}

/// A two-texel image must use the GPU texel-center rule and filter decoded linear taps.
#[test]
pub(super) fn software_scaled_image_matches_gpu_linear_light_midpoint() {
    let source = [0, 0, 0, 255, 255, 255, 255, 255];
    let background = [0.0, 0.0, 0.0, 1.0];
    let gpu = crate::atlas_upload::render_image_readback_on(2, &source, 3, None, background);
    let software = render_software_image(2, &source, 3, None, background);

    assert_bgra_within_one(&software, &gpu, "black-white 2-to-3 image");
    assert_eq!(&software[0..4], &[0, 0, 0, 255]);
    for &channel in &software[4..7] {
        assert!(channel.abs_diff(188) <= 1, "linear midpoint must encode near 188: {software:?}");
    }
    assert_eq!(&software[8..12], &[255, 255, 255, 255]);
}

/// Invalid RGB under zero alpha must canonicalize before filtering on both presenters.
#[test]
pub(super) fn software_scaled_image_matches_gpu_zero_alpha_canonicalization() {
    let source = [255, 255, 255, 255, 200, 0, 0, 0];
    let background = [0.0, 0.0, 0.0, 1.0];
    let gpu = crate::atlas_upload::render_image_readback_on(2, &source, 3, None, background);
    let software = render_software_image(2, &source, 3, None, background);

    assert_bgra_within_one(&software, &gpu, "zero-alpha image tap");
    assert!(
        software[4..7].iter().all(|channel| channel.abs_diff(188) <= 1),
        "zero-alpha RGB must not create a colored fringe: {software:?}"
    );
}

/// Bilinear software sampling must share the GPU clamp to the selected packed image tile.
#[test]
pub(super) fn software_scaled_image_matches_gpu_packed_tile_isolation() {
    let source = [255, 255, 255, 255];
    let neighbor = [0, 0, 255, 255];
    let background = [0.0, 0.0, 0.0, 1.0];
    let gpu =
        crate::atlas_upload::render_image_readback_on(1, &source, 3, Some(neighbor), background);
    let software = render_software_image(1, &source, 3, Some(neighbor), background);

    assert_bgra_within_one(&software, &gpu, "packed image tile");
    for pixel in software.as_chunks::<4>().0 {
        assert_eq!(*pixel, [255, 255, 255, 255]);
    }
}

/// A one-to-one opaque color glyph copies its encoded CPU source bytes exactly.
#[test]
pub(super) fn software_opaque_color_glyph_output_is_byte_compatible_with_cpu_atlas() {
    let source = [17, 34, 68, 255];
    let mut atlas = GlyphAtlas::new(1, 1);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('😀', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: source.to_vec(),
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("color glyph inserts");
    assert_eq!(&atlas.pixels_bgra()[0..4], &source);
    let mut frame = SoftwareFrame::new(1, 1, [0.0; 4]).expect("valid transparent frame");

    frame.draw_glyphs(
        &atlas,
        &[GlyphInstance {
            rect: px_to_ndc(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
            uv: info.uv,
            color: [1.0; 4],
            flags: [1.0, 0.0, 0.0, 0.0],
        }],
    );

    assert_eq!(frame.pixel_bgra(0, 0), source);
}

/// Scaled color glyphs keep nearest sampling rather than bleeding into transparent rows.
#[test]
pub(super) fn scaled_color_glyph_uses_nearest_sampling() {
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
    let mut frame = SoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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

/// Scaled images preserve bilinear sampling while interpolating decoded linear texels.
#[test]
pub(super) fn scaled_image_keeps_bilinear_sampling() {
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
    let mut frame = SoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    frame.draw_images(
        &atlas,
        &[ImageInstance { rect_px: [0.0, 0.0, 1.0, 3.0], uv: info.uv, sample_uv: info.uv }],
    );

    let middle = frame.pixel_bgra(0, 1);
    assert!(
        middle[0].abs_diff(188) <= 1,
        "image scaling should filter in linear light: {middle:?}"
    );
    assert_eq!(middle[0], middle[1]);
    assert_eq!(middle[1], middle[2]);
}

/// Image geometry retains fractional native-axis origins while text keeps its independent pixel alignment.
#[test]
pub(super) fn horizontally_scaled_image_keeps_fractional_vertical_origin() {
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
    let mut frame = SoftwareFrame::new(4, 4, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

    let image = ImageInstance { rect_px: [0.0, 1.5, 4.0, 1.0], uv: info.uv, sample_uv: info.uv };
    let gpu = crate::atlas_upload::render_image_instances_readback(
        &mut atlas,
        &[image],
        4,
        4,
        [0.0, 0.0, 0.0, 1.0],
    );
    frame.draw_images(&atlas, &[image]);
    for y in 0..4 {
        let offset = y as usize * 4 * 4;
        assert_eq!(frame.pixel_bgra(0, y).as_slice(), &gpu[offset..offset + 4]);
    }
    assert_eq!(frame.pixel_bgra(0, 1), [255, 255, 255, 255]);
    assert_eq!(frame.pixel_bgra(0, 2), [0, 0, 0, 255]);
}

/// Scaled text stays inside its tile even with an opaque neighbor packed underneath.
#[test]
pub(super) fn scaled_glyph_sampling_does_not_bleed_from_adjacent_atlas_tile() {
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

    let mut frame = SoftwareFrame::new(4, 5, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
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

/// The software presenter composes a degraded frame whole: it reads no damage, so every row a
/// degraded plan emits is drawn, including rows outside what a narrow hardware damage would cover,
/// and after a frame that painted every pixel nothing stale survives. That is why a partial plan,
/// whose rows outside the damage are not in the batches, must never reach this presenter.
#[test]
pub(super) fn a_degraded_frame_is_composed_whole_with_no_stale_pixel() {
    let atlas = GlyphAtlas::new(1, 1);
    let (width_px, height_px, row_h) = (12u32, 8u32, 2u32);
    let background = [0.0, 0.0, 0.0, 1.0];
    let mut frame = SoftwareFrame::new(width_px, height_px, background).unwrap();
    let surface = (width_px as f32, height_px as f32);
    let quad = |left: u32, top: u32, width: u32, height: u32, color: [f32; 4]| QuadInstance {
        rect: px_to_ndc(left as f32, top as f32, width as f32, height as f32, surface.0, surface.1),
        color,
        ..Default::default()
    };
    let red = [1.0, 0.0, 0.0, 1.0];
    frame.draw_layers(&atlas, &atlas, &[quad(0, 0, width_px, height_px, red)], &[], &[], &[], &[]);
    assert_eq!(frame.pixel_bgra(0, 0), [0, 0, 255, 255], "the first frame paints every pixel");

    // A degraded plan emits all four rows; row 1 is the edit a narrow hardware damage would
    // cover. Each row is a quad 10 px wide, so columns 10 and 11 are never drawn.
    let colors =
        [[0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0], [0.0, 1.0, 1.0, 1.0]];
    let rows: Vec<QuadInstance> =
        (0..4u32).map(|row| quad(0, row * row_h, 10, row_h, colors[row as usize])).collect();
    frame.prepare(width_px, height_px, background).unwrap();
    frame.draw_layers(&atlas, &atlas, &rows, &[], &[], &[], &[]);
    let bgra = |[red, green, blue, alpha]: [f32; 4]| {
        [(blue * 255.0) as u8, (green * 255.0) as u8, (red * 255.0) as u8, (alpha * 255.0) as u8]
    };
    for pixel_y in 0..height_px {
        for pixel_x in 0..width_px {
            let expected = if pixel_x < 10 {
                bgra(colors[(pixel_y / row_h) as usize])
            } else {
                // Columns no quad covers hold the background, not the first frame's red.
                [0, 0, 0, 255]
            };
            assert_eq!(frame.pixel_bgra(pixel_x, pixel_y), expected, "({pixel_x}, {pixel_y})");
        }
    }
}

/// The CPU compositor draws a scrolled frame whose rows replay from the content-keyed cache
/// exactly as one whose rows were all shaped cold: every composed pixel matches, and the
/// frame draws visible text, so the comparison is not of two empty frames.
#[test]
pub(super) fn software_frames_from_warm_and_cold_rows_are_identical() {
    let (atlas, cold, warm, (width, height)) = crate::core::warm_and_cold_row_glyphs();
    let background = [0.05, 0.05, 0.08, 1.0];
    let compose = |glyphs: &[GlyphInstance]| {
        let mut frame = SoftwareFrame::new(width, height, background).unwrap();
        frame.draw_layers(&atlas, &atlas, &[], &[], glyphs, &[], &[]);
        frame.pixels
    };
    let (warm_pixels, cold_pixels) = (compose(&warm), compose(&cold));
    let blank = SoftwareFrame::new(width, height, background).unwrap().pixels;
    let inked = cold_pixels.chunks(4).zip(blank.chunks(4)).filter(|(drawn, bare)| drawn != bare);
    assert!(inked.count() > 50, "the fixture draws text");
    assert!(warm_pixels == cold_pixels, "warm and cold software frames differ");
}
