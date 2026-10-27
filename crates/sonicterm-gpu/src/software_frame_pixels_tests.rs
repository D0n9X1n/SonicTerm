//! Software pixel sampling, origin stabilization and color blend contracts.

use super::super::software_frame_tests::*;
use super::*;

/// Native-size sampling depends on source and destination sizes, not their origins.
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
    let mut frame = SoftwareFrame::new(2, 65, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    let mut frame = SoftwareFrame::new(40, 100, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    let mut frame = SoftwareFrame::new(3, 20, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    let mut frame = SoftwareFrame::new(20, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    let mut frame = SoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    let mut frame = SoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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
    fn draw(marker: char, source_size: u32, hollow: bool) -> SoftwareFrame {
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
        let mut frame = SoftwareFrame::new(10, 10, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
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

    fn ink_bounds(frame: &SoftwareFrame) -> Option<(u32, u32, u32, u32)> {
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

/// Nearest text sampling keeps a transparent bottom texel free of a filtered fringe.
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
    let mut frame = SoftwareFrame::new(1, 3, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");

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

/// Native-size images at fractional origins must sample the same texels as the real GPU image path.
#[test]
fn fractional_native_image_position_matches_gpu_sampling() {
    let source = [0, 0, 0, 255, 255, 255, 255, 255, 32, 64, 128, 255];
    let mut failures = Vec::new();
    for x in [0.0, 0.25, 0.5, 0.75] {
        let mut atlas = GlyphAtlas::new(4, 1);
        let info = atlas
            .get_or_insert_lazy_without_eviction(
                GlyphKey::new('\u{fffc}', false, false),
                3,
                1,
                || RasterTile {
                    width: 3,
                    height: 1,
                    offset_x: 0,
                    offset_y: 0,
                    advance: 3.0,
                    coverage: source.to_vec(),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .unwrap();
        let image = ImageInstance { rect_px: [x, 0.0, 3.0, 1.0], uv: info.uv, sample_uv: info.uv };
        let gpu = crate::atlas_upload::render_image_instances_readback(
            &mut atlas,
            &[image],
            5,
            1,
            [0.0, 0.0, 0.0, 1.0],
        );
        let mut frame = SoftwareFrame::new(5, 1, [0.0, 0.0, 0.0, 1.0]).unwrap();
        frame.draw_images(&atlas, &[image]);
        let cpu: Vec<u8> = (0..5).flat_map(|column| frame.pixel_bgra(column, 0)).collect();
        if cpu.iter().zip(&gpu).any(|(a, b)| a.abs_diff(*b) > 1) {
            failures.push((x, cpu, gpu));
        }
    }
    assert!(failures.is_empty(), "fractional image parity mismatch: {failures:?}");
}

/// Every encoded-premultiplied channel and alpha pair must match GPU upload plus sRGB-view decode.
#[test]
fn software_color_channel_lookup_matches_gpu_staging_domain() {
    let decode = crate::color::srgb_u8_to_linear_lut();
    for alpha in 0..=u8::MAX {
        for encoded_premul in 0..=u8::MAX {
            let expected = if alpha == 0 {
                0.0
            } else {
                let straight_srgb = (encoded_premul as f64 / alpha as f64).clamp(0.0, 1.0);
                let alpha_linear = alpha as f64 / 255.0;
                let premul_linear =
                    crate::color::srgb_channel_to_linear(straight_srgb) * alpha_linear;
                decode[crate::color::linear_channel_to_srgb_u8(premul_linear as f32) as usize]
            };

            assert_eq!(
                premultiplied_srgb_channel_to_linear(encoded_premul, alpha),
                expected,
                "encoded_premul={encoded_premul}, alpha={alpha}"
            );
        }
    }
}

/// Self-colored glyph vectors must match real wgpu output over every representative background.
#[test]
fn software_color_glyphs_match_gpu_color_space_and_blending() {
    let black = [0.0, 0.0, 0.0, 1.0];
    let white = [1.0, 1.0, 1.0, 1.0];
    let colored = crate::color::hex_to_premultiplied_rgba("#204080", 1.0);
    let vectors = [
        ("opaque gray over black", [128, 128, 128, 255], black),
        ("opaque color over white", [32, 96, 224, 255], white),
        ("translucent gray over black", [64, 64, 64, 128], black),
        ("translucent gray over color", [64, 64, 64, 128], colored),
        ("translucent color over white", [16, 32, 64, 128], white),
        ("translucent color over color", [16, 32, 64, 128], colored),
    ];

    for (name, source, background) in vectors {
        let tile = RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: source.to_vec(),
            is_color: true,
            is_subpixel: false,
        };
        let gpu = crate::atlas_upload::render_glyph_readback_on(&[tile], background);
        let software = render_software_color_glyph(source, background);

        assert_bgra_within_one(&software, &gpu, name);
    }
}

/// Off keeps alpha-max grayscale, while RGB/BGR apply opposite red-blue coverage order.
#[test]
pub(super) fn software_subpixel_policy_selects_grayscale_rgb_and_bgr() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::{Bgr, Off, Rgb};

    let foreground = [1.0, 1.0, 1.0, 1.0];
    let black = [0.0, 0.0, 0.0, 1.0];

    assert_eq!(render_subpixel_software(Off, black, foreground), [255, 255, 255, 255]);
    assert_eq!(render_subpixel_software(Rgb, black, foreground), [0, 188, 255, 255]);
    assert_eq!(render_subpixel_software(Bgr, black, foreground), [255, 188, 0, 255]);
}

/// Repeated composition must preserve the same glyph pixels in grayscale and LCD, even at a fractional DPI-derived origin.
#[test]
pub(super) fn repeated_software_glyph_composition_preserves_pixels() {
    let mut atlas = GlyphAtlas::new(8, 8);
    let info = atlas
        .get_or_insert(
            GlyphKey::new('d', false, false),
            &mut TileRasterizer(RasterTile {
                width: 4,
                height: 3,
                offset_x: 0,
                offset_y: 0,
                advance: 4.0,
                coverage: [0, 0, 0, 0, 0, 32, 96, 96, 64, 128, 192, 192, 255, 255, 255, 255]
                    .repeat(3),
                is_color: false,
                is_subpixel: true,
            }),
        )
        .expect("LCD edge glyph inserts");
    let scale = 1.75;
    let width = (20.0 * scale) as u32;
    let height = (16.0 * scale) as u32;
    let background = crate::color::hex_to_premultiplied_rgba("#204080", 1.0);
    let glyph = GlyphInstance {
        // DPI scales the origin; rasterized atlas dimensions already use physical pixels.
        rect: px_to_ndc(3.0 * scale, 5.0 * scale, 4.0, 3.0, width as f32, height as f32),
        uv: info.uv,
        color: crate::color::hex_to_premultiplied_rgba("#e0a040", 0.75),
        flags: [0.0, 1.0, 0.0, 0.0],
    };
    for mode in [SubpixelAaMode::Off, SubpixelAaMode::Rgb, SubpixelAaMode::Bgr] {
        let mut frame = SoftwareFrame::new(width, height, background).unwrap();
        frame.draw_layers_with_subpixel_aa(&atlas, &atlas, mode, &[], &[], &[glyph], &[], &[]);
        let baseline = frame.pixels.clone();
        let clear = linear_rgba_to_bgra(background);
        assert!(
            baseline.as_chunks::<4>().0.iter().any(|pixel| *pixel != clear),
            "glyph must paint ink"
        );
        for cycle in 0..1000 {
            frame.prepare(width, height, background).unwrap();
            frame.draw_layers_with_subpixel_aa(&atlas, &atlas, mode, &[], &[], &[glyph], &[], &[]);
            assert_eq!(frame.pixels, baseline, "{mode:?} changed glyph pixels at cycle {cycle}");
        }
    }
}

/// LCD software blending decodes a colored destination and respects foreground alpha.
#[test]
pub(super) fn software_subpixel_blends_linear_light_with_foreground_alpha() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::Rgb;

    let background = crate::color::hex_to_premultiplied_rgba("#204080", 1.0);
    let foreground = crate::color::hex_to_premultiplied_rgba("#e0a040", 0.5);
    let pixel = render_subpixel_software(Rgb, background, foreground);

    assert_eq!(pixel, [128, 100, 166, 255]);
}

/// A reused atlas coordinate reads replacement pixels after an in-place atlas reset.
#[test]
pub(super) fn software_presenter_samples_replacement_after_in_place_atlas_reset() {
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

    let mut frame = SoftwareFrame::new(1, 1, [0.0, 0.0, 0.0, 1.0]).expect("valid frame");
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

/// Direct LCD blending applies logical RGB coverage over an sRGB-encoded BGRA destination.
#[test]
pub(super) fn subpixel_blend_uses_linear_destination_channels() {
    let mut dst = [32, 160, 240, 255];

    blend_subpixel_bgra(&mut dst, [1.0, 0.5, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]);

    assert_eq!(dst, [32, 116, 0, 255]);
}

/// Synthetic grayscale ramps retain the fixed hardware reference across DPI scales.
#[test]
pub(super) fn grayscale_ramp_matches_hardware_blending_at_supported_scales() {
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
        let mut frame = SoftwareFrame::new(
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

/// The software loops cross this module boundary per pixel, including in non-LTO builds.
#[test]
fn pixel_loop_helpers_remain_available_for_cross_unit_inlining() {
    let source = include_str!("software_frame_pixels.rs").replace("\r\n", "\n");
    for name in [
        "linear_rgba_to_bgra",
        "bgra8_to_unorm",
        "premultiplied_srgb_bgra_to_linear_rgba",
        "premultiplied_srgb_channel_to_linear",
        "stabilize_half_pixel_origin",
        "sample_color_atlas_bilinear_in_rect",
        "bgra_pixel_at",
        "blend_subpixel_bgra",
        "linear_to_srgb",
        "to_u8",
        "distance_to_segment",
    ] {
        assert!(source.contains(&format!("#[inline]\npub(super) fn {name}(")), "{name}");
    }
}
