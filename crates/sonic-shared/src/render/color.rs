//! Color / sRGB conversion helpers extracted from `render.rs` (issue #143).

use glyphon::Color as GColor;

/// Convert a glyphon `Color` (sRGB-encoded u8 channels) to a `[r, g, b, a]`
/// array in linear-light space, suitable for the quad pipeline.
pub fn glyphon_color_to_linear_rgba(c: GColor) -> [f32; 4] {
    // Use the 256-entry u8 LUT — every input here is already an 8-bit
    // sRGB channel, and the per-glyph hot path called this once per
    // visible cell per frame, paying for two `powf(2.4)` evaluations
    // each time. The LUT collapses each conversion to a single load.
    let t = srgb_u8_to_linear_lut();
    [t[c.r() as usize], t[c.g() as usize], t[c.b() as usize], 1.0]
}

/// Borrow the process-wide sRGB→linear lookup table. Computed once on
/// first use via a `OnceLock`; the table maps each of the 256 possible
/// u8 sRGB channel values to its linear-light counterpart so the
/// per-glyph hot path never has to evaluate `powf(2.4)`.
///
/// Bit-exact with `srgb_channel_to_linear(x as f64 / 255.0) as f32` for
/// every `x in 0..=255` (verified by unit test).
#[inline]
pub fn srgb_u8_to_linear_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0f32; 256];
        let mut i = 0usize;
        while i < 256 {
            t[i] = srgb_channel_to_linear(i as f64 / 255.0) as f32;
            i += 1;
        }
        t
    })
}

/// Convert one sRGB-encoded channel (0..=1) to linear-light space.
///
/// Standard sRGB EOTF (IEC 61966-2-1). Used because our wgpu surface is
/// `Bgra8UnormSrgb`, which performs linear→sRGB encoding on write — colors
/// the shader / clear-color sees must therefore be in linear space, or the
/// gamma is applied twice and the result looks washed out (e.g. Gruvbox Dark
/// Hard `#1d2021` rendering as mid-gray `~#6e6e6e`).
#[doc(hidden)]
pub fn srgb_channel_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Parse a `#rrggbb` hex string into a `wgpu::Color` in **linear** space,
/// suitable for use as a render-pass clear color on an sRGB surface format.
///
/// Alpha is left straight (no gamma curve applies to alpha).
#[doc(hidden)]
pub fn hex_to_wgpu(h: &str) -> wgpu::Color {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    if h.len() == 6 {
        wgpu::Color {
            r: srgb_channel_to_linear(parse(0)),
            g: srgb_channel_to_linear(parse(2)),
            b: srgb_channel_to_linear(parse(4)),
            a: 1.0,
        }
    } else {
        wgpu::Color::BLACK
    }
}

/// Parse a `#rrggbb` hex string + alpha into a `[r, g, b, a]` array in
/// **linear** RGB space, suitable for the quad pipeline which writes into
/// the same `Bgra8UnormSrgb` surface as the clear color above.
///
/// Alpha is passed through unchanged.
///
/// Note: glyphon's text path uses a separate `hex_to_glyphon` helper that
/// returns sRGB-encoded bytes, because glyphon / cosmic-text's atlas
/// expects sRGB input — the wgpu surface format performs the sRGB→linear
/// decode on sample, so glyph colors must NOT be pre-linearized.
#[doc(hidden)]
pub fn hex_to_rgba(h: &str, alpha: f32) -> [f32; 4] {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as usize;
    if h.len() == 6 {
        let t = srgb_u8_to_linear_lut();
        [t[parse(0)], t[parse(2)], t[parse(4)], alpha]
    } else {
        [0.0, 0.0, 0.0, alpha]
    }
}
