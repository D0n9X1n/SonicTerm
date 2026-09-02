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

#[test]
fn malformed_utf8_hex_uses_black_fallback_without_panicking() {
    assert_eq!(hex_to_wgpu_with_alpha("#0é000", 0.5), wgpu::Color::BLACK);
    assert_eq!(hex_to_rgba("#0é000", 0.5), [0.0, 0.0, 0.0, 0.5]);
    assert_eq!(hex_to_chrome_color("#0é000"), ChromeColor::rgb(0, 0, 0));
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
