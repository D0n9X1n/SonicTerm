use super::*;

#[test]
fn grayscale_coverage_preserves_every_nonzero_edge_sample() {
    let samples = [0_u8, 1, 4, 8, 12, 16, 19, 20];
    let mut previous = grayscale_coverage([0.0; 4]);
    assert_eq!(previous, 0.0);

    for sample in samples.into_iter().skip(1) {
        let coverage = sample as f32 / 255.0;
        let retained = grayscale_coverage([coverage; 4]);
        assert!(retained > previous, "coverage {sample}/255 was discarded");
        previous = retained;
    }
}

#[test]
fn software_text_blend_matches_gpu_linear_source_over() {
    let mut background = [32, 96, 160, 255];
    let foreground = [0.5 * 0.6 * 0.25, 0.25 * 0.6 * 0.25, 0.75 * 0.6 * 0.25, 0.6 * 0.25];

    blend_premul_linear_over_srgb_bgra(&mut background, foreground);

    assert_eq!(background, [99, 103, 165, 255]);
}

/// Linear source-over must encode canonical grayscale opacity vectors exactly once.
#[test]
fn linear_source_over_encodes_white_and_black_opacity_vectors() {
    for (alpha, white_over_black, black_over_white) in
        [(0.25, 137, 225), (0.5, 188, 188), (0.75, 225, 137)]
    {
        let mut black = [0, 0, 0, 255];
        blend_premul_linear_over_srgb_bgra(&mut black, [alpha; 4]);
        assert_eq!(black, [white_over_black, white_over_black, white_over_black, 255]);

        let mut white = [255, 255, 255, 255];
        blend_premul_linear_over_srgb_bgra(&mut white, [0.0, 0.0, 0.0, alpha]);
        assert_eq!(white, [black_over_white, black_over_white, black_over_white, 255]);
    }
}

/// Colored sRGB destinations must be decoded before a premultiplied linear source is blended.
#[test]
fn linear_source_over_preserves_colored_background_vectors() {
    for (alpha, expected) in
        [(0.25, [27, 84, 190, 255]), (0.5, [20, 68, 214, 255]), (0.75, [12, 48, 236, 255])]
    {
        let mut background = [32, 96, 160, 255];
        blend_premul_linear_over_srgb_bgra(&mut background, [alpha, 0.0, 0.0, alpha]);
        assert_eq!(background, expected);
    }
}

/// Source-over alpha remains linear when both source and destination are translucent.
#[test]
fn linear_source_over_keeps_translucent_destination_alpha() {
    let mut background = [63, 89, 124, 102];

    blend_premul_linear_over_srgb_bgra(&mut background, [0.3, 0.1, 0.05, 0.5]);

    assert_eq!(background, [77, 108, 170, 179]);
}

/// The constant-source lookup path must remain byte-identical to direct blending.
#[test]
fn linear_source_over_lookup_matches_direct_blending() {
    for src in
        [[0.0, 0.0, 0.0, 0.0], [0.5, 0.0, 0.0, 0.5], [0.3, 0.1, 0.05, 0.5], [1.0, 1.0, 1.0, 1.0]]
    {
        let lookup = LinearOverSrgbBgraLut::new(src);
        for channel in 0..4 {
            for value in 0..=u8::MAX {
                let mut direct = [32, 96, 160, 102];
                direct[channel] = value;
                let mut table = direct;
                blend_premul_linear_over_srgb_bgra(&mut direct, src);
                lookup.blend(&mut table);
                assert_eq!(table, direct, "src={src:?} channel={channel} value={value}");
            }
        }
    }
}

/// Malformed Unicode and ASCII hex inputs fall back to black without slicing panics.
#[test]
fn malformed_utf8_hex_uses_black_fallback_without_panicking() {
    assert_eq!(hex_to_wgpu_with_alpha("#0é000", 0.5), wgpu::Color::BLACK);
    assert_eq!(hex_to_premultiplied_rgba("#0é000", 0.5), [0.0, 0.0, 0.0, 0.5]);
    assert_eq!(hex_to_premultiplied_rgba("#zz4020", 0.5), [0.0, 0.0, 0.0, 0.5]);
    assert_eq!(hex_to_chrome_color("#0é000"), ChromeColor::rgb(0, 0, 0));
}

/// Hex quad colors decode sRGB before multiplying every linear channel by opacity.
#[test]
fn hex_quad_color_is_premultiplied_in_linear_light() {
    let actual = hex_to_premultiplied_rgba("#e04020", 0.5);
    let expected = [0.372_702_1, 0.025_634_73, 0.007_221_92, 0.5];

    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 1.0e-7, "actual={actual} expected={expected}");
    }
}

/// Hex quad conversion clamps opacity and keeps malformed colors validly premultiplied black.
#[test]
fn hex_quad_color_clamps_alpha_and_preserves_black_fallback() {
    assert_eq!(hex_to_premultiplied_rgba("#ffffff", -1.0), [0.0; 4]);
    assert_eq!(hex_to_premultiplied_rgba("#ffffff", 2.0), [1.0; 4]);
    assert_eq!(hex_to_premultiplied_rgba("invalid", 0.25), [0.0, 0.0, 0.0, 0.25]);
}

/// Representative decoded colors remain valid premultiplied inputs at every common opacity.
#[test]
fn hex_quad_colors_satisfy_the_quad_invariant() {
    for hex in ["#000000", "#123456", "#e04020", "#ffffff"] {
        for alpha in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!(crate::quad::is_premultiplied_linear_rgba(hex_to_premultiplied_rgba(
                hex, alpha
            )));
        }
    }
}

/// Opaque quad conversion and straight-alpha glyph colors retain their established values.
#[test]
fn opaque_quads_and_glyph_foregrounds_remain_unchanged() {
    assert_eq!(
        hex_to_premultiplied_rgba("#e04020", 1.0),
        chrome_color_to_linear_rgba(ChromeColor::rgb(0xe0, 0x40, 0x20))
    );
    assert_eq!(hex_to_chrome_color("#e04020"), ChromeColor::rgb(0xe0, 0x40, 0x20));
}

/// Named translucent producers share exact premultiplied source-over output in software.
#[test]
fn named_quad_producers_have_exact_software_blend_vectors() {
    let opaque = hex_to_premultiplied_rgba("#e04020", 1.0);
    let cases = [
        ("selection", hex_to_premultiplied_rgba("#e04020", 0.5), [66, 58, 165, 255]),
        ("url hover", hex_to_premultiplied_rgba("#e04020", 0.9), [41, 63, 214, 255]),
        ("tofu", crate::quad::with_premultiplied_alpha(opaque, 0.55), [63, 59, 172, 255]),
        ("tab dimming", crate::quad::with_premultiplied_alpha(opaque, 0.18), [79, 54, 104, 255]),
        ("drag chip body", crate::quad::with_premultiplied_alpha(opaque, 0.5), [66, 58, 165, 255]),
    ];

    for (name, source, expected) in cases {
        let mut background = [0x56, 0x34, 0x12, 0xff];
        blend_premul_linear_over_srgb_bgra(&mut background, source);
        assert_eq!(background, expected, "{name}");
    }
}

#[test]
fn optimized_linear_encoder_matches_srgb_rounding() {
    for encoded_linear in 0..=u16::MAX {
        let linear = encoded_linear as f32 / u16::MAX as f32;
        let expected = (if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        } * 255.0)
            .round() as u8;
        assert_eq!(linear_channel_to_srgb_u8(linear), expected, "linear={linear}");
    }

    assert_eq!(srgb_channel_to_linear(0.0), 0.0);
}
