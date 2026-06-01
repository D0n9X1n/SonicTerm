//! Verify the 256-entry sRGB→linear LUT is bit-exact with the float
//! implementation it replaces. The LUT is in the hot path of every
//! per-glyph `glyphon_color_to_linear_rgba` call; any mismatch would
//! produce a visible color shift on body text.

use sonicterm_shared::render::{srgb_channel_to_linear, srgb_u8_to_linear_lut};

#[test]
fn lut_matches_float_pow_path_for_every_u8() {
    let t = srgb_u8_to_linear_lut();
    for i in 0u32..=255 {
        let want = srgb_channel_to_linear(i as f64 / 255.0) as f32;
        let got = t[i as usize];
        assert!((want - got).abs() < f32::EPSILON, "i={i}: want {want}, got {got}",);
    }
}
