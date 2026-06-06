//! Color / sRGB conversion helpers extracted from `render.rs` (issue #143).
//!
//! T13 (wezterm-takeover G3): `ChromeColor` replaces `legacy chrome color` as
//! the chrome-text fg type. It carries the same 8-bit sRGB-encoded
//! channels the legacy chrome layer used (so the LUT-based linearization path is byte-
//! identical) but does not pull the legacy chrome layer into the dep graph. Every
//! `_the legacy chrome layer_` identifier in this file is renamed to `_chrome_color_` /
//! `_chrome_text_` so the must-pass #4 grep gate (`grep -rE 'the legacy chrome layer'`)
//! returns zero.

/// SonicTerm's chrome-text foreground color. 8-bit sRGB-encoded
/// channels in the same byte layout the legacy chrome layer's `Color` used, so the
/// existing LUT-based linearization (`chrome_color_to_linear_rgba`) is
/// byte-identical to the legacy path.
///
/// Alpha is straight (no premultiplication); the per-glyph pipeline
/// premultiplies on the way to the GPU instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeColor {
    /// Red channel, 0..=255 sRGB-encoded.
    pub r: u8,
    /// Green channel, 0..=255 sRGB-encoded.
    pub g: u8,
    /// Blue channel, 0..=255 sRGB-encoded.
    pub b: u8,
    /// Straight alpha, 0..=255.
    pub a: u8,
}

impl ChromeColor {
    /// Opaque white. Matches `legacy chrome color::rgb(255,255,255)`.
    pub const WHITE: ChromeColor = ChromeColor { r: 255, g: 255, b: 255, a: 255 };

    /// Construct an opaque color from sRGB-encoded `(r, g, b)` channels.
    /// Matches `legacy chrome color::rgb` byte-for-byte.
    #[inline]
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Construct a color from sRGB-encoded `(r, g, b, a)` channels.
    /// Matches `legacy chrome color::rgba` byte-for-byte.
    #[inline]
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Red channel accessor — same name as the the legacy chrome layer `Color::r()` helper
    /// every caller used to invoke. Keeps the migration mechanical.
    #[inline]
    #[must_use]
    pub const fn r(&self) -> u8 {
        self.r
    }

    /// Green channel accessor.
    #[inline]
    #[must_use]
    pub const fn g(&self) -> u8 {
        self.g
    }

    /// Blue channel accessor.
    #[inline]
    #[must_use]
    pub const fn b(&self) -> u8 {
        self.b
    }

    /// Alpha accessor.
    #[inline]
    #[must_use]
    pub const fn a(&self) -> u8 {
        self.a
    }
}

impl From<[u8; 4]> for ChromeColor {
    #[inline]
    fn from(c: [u8; 4]) -> Self {
        Self { r: c[0], g: c[1], b: c[2], a: c[3] }
    }
}

impl From<ChromeColor> for [u8; 4] {
    #[inline]
    fn from(c: ChromeColor) -> Self {
        [c.r, c.g, c.b, c.a]
    }
}

/// Convert a [`ChromeColor`] (sRGB-encoded u8 channels) to a `[r, g, b, a]`
/// array in linear-light space, suitable for the quad pipeline.
///
/// Bit-exact with the legacy `the legacy chrome layer_color_to_linear_rgba` path (same
/// LUT, same channel ordering, alpha always `1.0`).
pub fn chrome_color_to_linear_rgba(c: ChromeColor) -> [f32; 4] {
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
    hex_to_wgpu_with_alpha(h, 1.0)
}

/// Parse a `#rrggbb` hex string into a premultiplied `wgpu::Color` in
/// **linear** space with the requested opacity.
#[doc(hidden)]
pub fn hex_to_wgpu_with_alpha(h: &str, alpha: f32) -> wgpu::Color {
    let alpha = alpha.clamp(0.0, 1.0) as f64;
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    if h.len() == 6 {
        wgpu::Color {
            r: srgb_channel_to_linear(parse(0)) * alpha,
            g: srgb_channel_to_linear(parse(2)) * alpha,
            b: srgb_channel_to_linear(parse(4)) * alpha,
            a: alpha,
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
/// Note: the chrome-text path uses a separate [`hex_to_chrome_color`]
/// helper that returns sRGB-encoded bytes, because the chrome atlas
/// stores tiles in the same sRGB-encoded coverage layout the legacy chrome layer /
/// cosmic-text used — the wgpu surface format performs the sRGB→linear
/// decode on sample, so glyph foreground colors must NOT be
/// pre-linearized.
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

/// Parse a `#rrggbb` hex string into a [`ChromeColor`] (sRGB-encoded
/// u8 channels). Alpha is set to `0xFF` (fully opaque). On malformed
/// input falls back to opaque black.
///
/// Replaces the legacy `hex_to_the legacy chrome layer` helper that lived in `core.rs`
/// and returned a `legacy chrome color`. The byte layout is preserved
/// (sRGB-encoded `r,g,b,a` u8s) so chrome theming values round-trip
/// identically through the new path.
#[must_use]
pub fn hex_to_chrome_color(h: &str) -> ChromeColor {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    if h.len() == 6 {
        ChromeColor::rgb(parse(0), parse(2), parse(4))
    } else {
        ChromeColor::rgb(0, 0, 0)
    }
}

/// Multiply the alpha channel of a [`ChromeColor`] by `factor`
/// (clamped to `0.0..=1.0`) and return a fresh color with the same
/// RGB triplet.
///
/// Replaces the legacy `scale_the legacy chrome layer_alpha` helper (Phase D
/// drag-feedback path, Epic #289) — same math, new type. Used to dim
/// the source-tab title text and the ghost-chip title text so they
/// match their corresponding dimmed body quads.
#[doc(hidden)]
#[must_use]
pub fn scale_chrome_text_alpha(c: ChromeColor, factor: f32) -> ChromeColor {
    let f = factor.clamp(0.0, 1.0);
    let a = ((c.a() as f32) * f).round().clamp(0.0, 255.0) as u8;
    ChromeColor::rgba(c.r(), c.g(), c.b(), a)
}
