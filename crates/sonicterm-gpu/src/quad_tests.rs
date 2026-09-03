use super::*;

/// Replacing opacity rescales premultiplied RGB while preserving the straight-color ratio.
#[test]
fn absolute_alpha_replacement_preserves_straight_hue() {
    assert_eq!(with_premultiplied_alpha([0.2, 0.1, 0.05, 0.25], 0.5), [0.4, 0.2, 0.1, 0.5]);
}

/// A transparent source has no recoverable hue and therefore remains premultiplied black.
#[test]
fn absolute_alpha_replacement_handles_zero_alpha_as_black() {
    assert_eq!(with_premultiplied_alpha([0.0, 0.0, 0.0, 0.0], 0.75), [0.0, 0.0, 0.0, 0.75]);
}

/// Multiplicative opacity scales all premultiplied components and clamps its factor.
#[test]
fn multiplicative_alpha_scales_every_channel() {
    let color = [0.4, 0.2, 0.1, 0.5];
    assert_eq!(scale_premultiplied_alpha(color, 0.5), [0.2, 0.1, 0.05, 0.25]);
    assert_eq!(scale_premultiplied_alpha(color, 2.0), color);
    assert_eq!(scale_premultiplied_alpha(color, -1.0), [0.0; 4]);
}

/// The producer validator rejects non-finite, out-of-range, and straight-alpha inputs.
#[test]
fn premultiplied_validator_rejects_invalid_sources() {
    assert!(is_premultiplied_linear_rgba([0.4, 0.2, 0.1, 0.5]));
    assert!(!is_premultiplied_linear_rgba([0.8, 0.2, 0.1, 0.5]));
    assert!(!is_premultiplied_linear_rgba([-0.1, 0.0, 0.0, 0.5]));
    assert!(!is_premultiplied_linear_rgba([0.0, 0.0, 0.0, 1.1]));
    assert!(!is_premultiplied_linear_rgba([f32::NAN, 0.0, 0.0, 0.5]));
    assert!(!is_premultiplied_linear_rgba([0.0, 0.0, 0.0, f32::INFINITY]));
}

/// Partial mask coverage attenuates RGB and alpha together before quad emission.
#[test]
fn mask_icon_coverage_scales_every_channel() {
    let mut mask = [0_u8; 64];
    mask[0] = 128;
    let color = [0.4, 0.2, 0.1, 0.5];
    let mut quads = Vec::new();

    push_mask_icon_quads(
        &mut quads,
        MaskIconParams {
            mask: &mask,
            x: 0.0,
            y: 0.0,
            size: 8.0,
            min_cell: 0.5,
            color,
            sw: 8.0,
            sh: 8.0,
        },
    );

    assert_eq!(quads.len(), 1);
    assert_eq!(quads[0].color, scale_premultiplied_alpha(color, 128.0 / 255.0));
}

/// Debug layer validation catches invalid colors from direct struct-literal producers.
#[cfg(debug_assertions)]
#[test]
fn quad_slice_validation_rejects_invalid_direct_literals() {
    let invalid = QuadInstance { color: [0.8, 0.2, 0.1, 0.5], ..Default::default() };
    assert!(std::panic::catch_unwind(|| debug_assert_premultiplied_quads("overlay", &[invalid]))
        .is_err());
}

/// Debug constructors reject invalid straight-alpha producer colors at their creation seam.
#[cfg(debug_assertions)]
#[test]
fn quad_constructors_reject_straight_alpha_colors() {
    let invalid = [0.8, 0.2, 0.1, 0.5];
    assert!(std::panic::catch_unwind(|| QuadInstance::sharp([0.0; 4], invalid)).is_err());
    assert!(std::panic::catch_unwind(|| QuadInstance::rounded([0.0; 4], invalid, [1.0; 2], 1.0))
        .is_err());
    assert!(std::panic::catch_unwind(|| {
        QuadInstance::line([0.0; 4], invalid, [1.0; 2], [0.0; 2], [1.0; 2], 1.0)
    })
    .is_err());
}
