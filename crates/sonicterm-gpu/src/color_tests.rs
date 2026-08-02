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
