//! Unit tests for the pure variation-axis scaling helpers in `ftwrap`.
//!
//! These exercise the safe core of `Face::weight_and_width` without touching
//! FreeType: `scaled_weight_and_width` and `AxisScaling::scale`. The unsafe
//! `FT_Get_MM_Var`/`FT_Done_MM_Var` collection path is a link/build gate, not a
//! hollow unit test.

use super::*;

fn wght_tag() -> FT_ULong {
    ft_make_tag(b'w', b'g', b'h', b't')
}

fn wdth_tag() -> FT_ULong {
    ft_make_tag(b'w', b'd', b't', b'h')
}

fn axis(tag: FT_ULong, value: f64, default_value: f64) -> AxisScaling {
    AxisScaling { tag, value, default_value }
}

#[test]
fn no_scalings_returns_rounded_base() {
    // The non-variable path (and the OS/2 fallback of 400/5) must pass through
    // unchanged.
    assert_eq!(scaled_weight_and_width(400., 5., &[]), (400, 5));
}

#[test]
fn metadata_error_retains_rounded_base_metrics() {
    assert_eq!(weight_and_width_with_variation(400.4, 4.6, Err(())), (400, 5));
}

#[test]
fn usable_metadata_applies_axis_scaling() {
    let axes = vec![axis(wght_tag(), 700., 400.)];
    assert_eq!(weight_and_width_with_variation(400., 5., Ok(axes)), (700, 5));
}

#[test]
fn wght_axis_scales_only_weight() {
    // value/default = 700/400 = 1.75; 400 * 1.75 = 700. Width is untouched.
    let axes = [axis(wght_tag(), 700., 400.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (700, 5));
}

#[test]
fn wdth_axis_scales_only_width() {
    // value/default = 200/100 = 2.0; 5 * 2.0 = 10. Weight is untouched.
    let axes = [axis(wdth_tag(), 200., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 10));
}

#[test]
fn wght_and_wdth_scale_independently() {
    // weight: 400 * (800/400 = 2.0) = 800
    // width:    5 * (75/100 = 0.75) = 3.75 -> rounds to 4
    let axes = [axis(wght_tag(), 800., 400.), axis(wdth_tag(), 75., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (800, 4));
}

#[test]
fn zero_default_yields_neutral_scale() {
    // A zero axis default must not divide by zero; the scale is 1.0 so the base
    // weight/width are preserved.
    assert_eq!(axis(wght_tag(), 700., 0.).scale(), 1.);
    let axes = [axis(wght_tag(), 700., 0.), axis(wdth_tag(), 200., 0.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn unrelated_axes_are_ignored() {
    // ital/slnt/opsz carry real scales but must not affect weight or width.
    let ital = ft_make_tag(b'i', b't', b'a', b'l');
    let slnt = ft_make_tag(b's', b'l', b'n', b't');
    let opsz = ft_make_tag(b'o', b'p', b's', b'z');
    let axes = [axis(ital, 1., 0.5), axis(slnt, -10., 5.), axis(opsz, 8., 12.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn unrelated_axis_does_not_leak_into_weight_or_width() {
    // A mix: only the wght axis applies; the optical-size axis is inert.
    let opsz = ft_make_tag(b'o', b'p', b's', b'z');
    let axes = [axis(opsz, 8., 12.), axis(wght_tag(), 600., 400.)];
    // 400 * (600/400 = 1.5) = 600; width stays 5.
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (600, 5));
}

#[test]
fn scaling_rounds_half_away_from_zero() {
    // 401 * (3/2 = 1.5) = 601.5 -> rounds up to 602.
    let axes = [axis(wght_tag(), 3., 2.)];
    assert_eq!(scaled_weight_and_width(401., 5., &axes), (602, 5));
}

#[test]
fn scaling_rounds_fraction_down() {
    // 400 * (1001/1000 = 1.001) = 400.4 -> rounds down to 400.
    let axes = [axis(wght_tag(), 1001., 1000.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn identity_scale_leaves_base_unchanged() {
    // value == default => scale 1.0 for both axes.
    assert_eq!(axis(wght_tag(), 400., 400.).scale(), 1.);
    let axes = [axis(wght_tag(), 400., 400.), axis(wdth_tag(), 100., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}
