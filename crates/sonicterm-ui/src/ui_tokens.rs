//! UI design tokens — the cross-cutting foundation for all P0 visual work.
//!
//! These tokens are the single source of truth for chrome colors, radii,
//! shadows, spacing, motion curves, and typography.
//!
//! As of the theme-driven UI work, chrome colors are derived from the
//! active terminal [`Theme`] via [`UiPalette::from_theme`] — the palette
//! / tab bar inherit the user's chosen colors instead of being
//! locked to Tokyo Night. The previous Tokyo-Night-derived constants
//! (`color::ACCENT_BLUE`, `color::BG_BASE`, etc.) remain available but
//! `#[deprecated]` for backward compatibility.
//!
//! Colors are stored as **linear-sRGB premultiplied `[r, g, b, a]`** so they
//! can be uploaded to wgpu without further conversion. The [`color::hex`]
//! helper performs the sRGB→linear transform and the premultiply step.

use sonicterm_cfg::theme::Theme;

/// Theme-derived UI chrome palette. Built from a [`Theme`] via
/// [`UiPalette::from_theme`]; every field is a linear-sRGB premultiplied
/// `[r, g, b, a]` ready for wgpu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiPalette {
    pub accent: [f32; 4],
    pub bg_base: [f32; 4],
    pub bg_elevated: [f32; 4],
    pub bg_surface: [f32; 4],
    pub bg_hover: [f32; 4],
    pub bg_active: [f32; 4],
    pub border_subtle: [f32; 4],
    pub border_strong: [f32; 4],
    pub border_focus: [f32; 4],
    pub text_primary: [f32; 4],
    pub text_secondary: [f32; 4],
    pub text_muted: [f32; 4],
    pub text_faint: [f32; 4],
    pub danger: [f32; 4],
    pub accent_orange: [f32; 4],
    pub accent_purple: [f32; 4],
    pub scrim: [f32; 4],
    pub selection: [f32; 4],
    pub search_match: [f32; 4],
    pub search_current: [f32; 4],
}

impl UiPalette {
    /// Derive a chrome palette from the active terminal [`Theme`].
    ///
    /// - `accent`        — `theme.colors.tab.active_fg` (the theme's
    ///   explicit chrome accent), e.g. `#fabd2f` for gruvbox-dark-hard,
    ///   `#7aa2f7` for tokyo-night.
    /// - `bg_base`       — `theme.colors.background` shifted -8% lightness.
    /// - `bg_elevated`   — `theme.colors.background` (i.e. base).
    /// - `bg_surface`    — `theme.colors.background` shifted +5% lightness.
    /// - `bg_hover`      — `foreground` @ 6% alpha.
    /// - `bg_active`     — accent @ 14% alpha.
    /// - `border_subtle` — `foreground` @ 8% alpha.
    /// - `border_strong` — `foreground` @ 12% alpha.
    /// - `border_focus`  — accent @ 65% alpha.
    /// - `text_primary`  — `theme.colors.foreground`.
    /// - `text_secondary`— `foreground` darkened 15%.
    /// - `text_muted`    — `theme.colors.bright.black`.
    /// - `text_faint`    — `bright.black` darkened 15%.
    /// - `danger`        — `theme.colors.ansi.red`.
    /// - `accent_orange` — `theme.colors.bright.yellow`.
    /// - `accent_purple` — `theme.colors.ansi.magenta`.
    /// - `scrim`         — pure black @ 28% alpha (theme-independent).
    /// - `selection`     — accent @ 26% alpha.
    /// - `search_match`  — `theme.colors.ansi.yellow` @ 28% alpha.
    /// - `search_current`— `theme.colors.bright.yellow` @ 42% alpha.
    pub fn from_theme(theme: &Theme) -> Self {
        Self::from_theme_with_strong_focus(theme, false)
    }

    /// Derive a chrome palette and optionally boost focus/accent affordances
    /// for the `accessibility.strong_focus` mode.
    pub fn from_theme_with_strong_focus(theme: &Theme, strong_focus: bool) -> Self {
        let theme_colors = &theme.colors;
        let accent = if strong_focus {
            color::double_saturate_hex(&theme_colors.tab.active_fg.0)
        } else {
            // When: strong_focus is off, so the accent keeps the theme's own
            // saturation rather than the boosted accessibility variant.
            color::hex(&theme_colors.tab.active_fg.0)
        };
        let bg_elevated = color::hex(&theme_colors.background.0);
        let bg_base = color::hex_with_lightness_delta(&theme_colors.background.0, -0.08);
        let bg_surface = color::hex_with_lightness_delta(&theme_colors.background.0, 0.05);
        let fg = color::hex(&theme_colors.foreground.0);
        let text_secondary = color::hex_with_lightness_delta(&theme_colors.foreground.0, -0.15);
        let muted = color::hex(&theme_colors.bright.black.0);
        let text_faint = color::hex_with_lightness_delta(&theme_colors.bright.black.0, -0.15);

        Self {
            accent,
            bg_base,
            bg_elevated,
            bg_surface,
            bg_hover: color::with_alpha(fg, 0.06),
            bg_active: color::with_alpha(accent, 0.14),
            border_subtle: color::with_alpha(fg, 0.08),
            border_strong: color::with_alpha(fg, 0.12),
            border_focus: color::with_alpha(accent, 0.65),
            text_primary: fg,
            text_secondary,
            text_muted: muted,
            text_faint,
            danger: color::hex(&theme_colors.ansi.red.0),
            accent_orange: color::hex(&theme_colors.bright.yellow.0),
            accent_purple: color::hex(&theme_colors.ansi.magenta.0),
            scrim: color::with_alpha(color::hex("#000000"), 0.28),
            selection: color::with_alpha(accent, 0.26),
            search_match: color::with_alpha(color::hex(&theme_colors.ansi.yellow.0), 0.28),
            search_current: color::with_alpha(color::hex(&theme_colors.bright.yellow.0), 0.42),
        }
    }
}

impl From<&Theme> for UiPalette {
    fn from(theme: &Theme) -> Self {
        Self::from_theme(theme)
    }
}

/// Extension trait wired into `sonicterm_cfg::theme::Theme` so call sites can
/// write `theme.ui_palette()`.
pub trait ThemeUiPaletteExt {
    /// Chrome palette derived from this theme's colors.
    fn ui_palette(&self) -> UiPalette;
}

impl ThemeUiPaletteExt for Theme {
    fn ui_palette(&self) -> UiPalette {
        UiPalette::from_theme(self)
    }
}

/// Chrome color tokens.
pub mod color {
    /// Runtime sRGB→linear (accurate piecewise EOTF).
    #[inline]
    fn srgb_to_linear_f(channel: f32) -> f32 {
        if channel <= 0.040_448_237 {
            channel / 12.92
        } else {
            // When: channel sits above the sRGB linear-segment cutoff, so the curved
            // gamma expression applies instead of the straight scale.
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Convert 8-bit sRGB + alpha into linear-sRGB premultiplied `[r,g,b,a]`.
    #[inline]
    fn rgba8_premul_linear(red: u8, green: u8, blue: u8, alpha: f32) -> [f32; 4] {
        let linear_red = srgb_to_linear_f(red as f32 / 255.0);
        let linear_green = srgb_to_linear_f(green as f32 / 255.0);
        let linear_blue = srgb_to_linear_f(blue as f32 / 255.0);
        let alpha = alpha.clamp(0.0, 1.0);
        [linear_red * alpha, linear_green * alpha, linear_blue * alpha, alpha]
    }

    /// Parse `#RRGGBB` or `#RRGGBBAA` into linear-sRGB premultiplied `[r,g,b,a]`.
    ///
    /// Returns opaque black on any parse error (so token usage stays
    /// infallible at call sites).
    pub fn hex(text: &str) -> [f32; 4] {
        const SENTINEL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        let text = text.trim();
        let text = text.strip_prefix('#').unwrap_or(text);
        let bytes = text.as_bytes();
        if bytes.len() != 6 && bytes.len() != 8 {
            // When: bytes is neither a 6- nor 8-digit body, so the text cannot
            // be a hex color; report the opaque-black sentinel.
            return SENTINEL;
        }
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            // When: bytes holds a character outside the hex alphabet, so no
            // channel can be decoded; report the opaque-black sentinel.
            return SENTINEL;
        }
        #[inline]
        fn nyb(digit: u8) -> u8 {
            match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => 0,
            }
        }
        #[inline]
        fn pair(digits: &[u8], index: usize) -> u8 {
            (nyb(digits[index]) << 4) | nyb(digits[index + 1])
        }
        let red = pair(bytes, 0);
        let green = pair(bytes, 2);
        let blue = pair(bytes, 4);
        let alpha = if bytes.len() == 8 {
            pair(bytes, 6) as f32 / 255.0
        } else {
            // When: bytes carries no trailing alpha pair, so the color is
            // fully opaque.
            1.0
        };
        rgba8_premul_linear(red, green, blue, alpha)
    }

    /// Replace the alpha channel of a premultiplied token.
    ///
    /// Input is assumed to be linear-premultiplied (as produced by [`hex`]).
    /// We first un-premultiply by the existing alpha, then re-premultiply by
    /// the new one.
    pub fn with_alpha(color: [f32; 4], alpha: f32) -> [f32; 4] {
        let alpha = alpha.clamp(0.0, 1.0);
        let old_a = color[3];
        let (linear_red, linear_green, linear_blue) = if old_a > f32::EPSILON {
            (color[0] / old_a, color[1] / old_a, color[2] / old_a)
        } else {
            // When: old_a is effectively zero, so dividing it out would be
            // undefined; start from black and let the new alpha scale it.
            (0.0, 0.0, 0.0)
        };
        [linear_red * alpha, linear_green * alpha, linear_blue * alpha, alpha]
    }

    /// Double HSL saturation for an accent color, preserving lightness and alpha.
    pub fn double_saturate_hex(hex_color: &str) -> [f32; 4] {
        adjust_hsl(hex_color, 0.0, Some(2.0))
    }

    /// Adjust the lightness of a `#RRGGBB`/`#RRGGBBAA` color in HSL space
    /// by `delta` (typically `-0.15`..`+0.15`) and return the result as
    /// linear-sRGB premultiplied `[r,g,b,a]`. `delta > 0` lightens,
    /// `delta < 0` darkens. Clamped to `[0, 1]`.
    pub fn hex_with_lightness_delta(hex_color: &str, delta: f32) -> [f32; 4] {
        adjust_hsl(hex_color, delta, None)
    }

    fn adjust_hsl(
        hex_color: &str,
        lightness_delta: f32,
        saturation_scale: Option<f32>,
    ) -> [f32; 4] {
        const SENTINEL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        let trimmed = hex_color.trim();
        let body = trimmed.strip_prefix('#').unwrap_or(trimmed);
        let bytes = body.as_bytes();
        if bytes.len() != 6 && bytes.len() != 8 {
            // When: bytes is neither a 6- nor 8-digit body, so there is no
            // color to shift; report the opaque-black sentinel.
            return SENTINEL;
        }
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            // When: bytes holds a character outside the hex alphabet, so no
            // channel can be decoded; report the opaque-black sentinel.
            return SENTINEL;
        }
        #[inline]
        fn nyb(digit: u8) -> u8 {
            match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => 0,
            }
        }
        #[inline]
        fn pair(digits: &[u8], index: usize) -> u8 {
            (nyb(digits[index]) << 4) | nyb(digits[index + 1])
        }
        let red = pair(bytes, 0) as f32 / 255.0;
        let green = pair(bytes, 2) as f32 / 255.0;
        let blue = pair(bytes, 4) as f32 / 255.0;
        let alpha = if bytes.len() == 8 {
            pair(bytes, 6) as f32 / 255.0
        } else {
            // When: bytes carries no trailing alpha pair, so the color is
            // fully opaque.
            1.0
        };

        // sRGB → HSL (sRGB-space lightness; this is the perceptual knob
        // designers expect for "+5%/-8% lightness" — *not* a linear-light
        // operation).
        let (hue, saturation, lightness) = srgb_to_hsl(red, green, blue);
        let saturation =
            saturation_scale.map_or(saturation, |scale| (saturation * scale).clamp(0.0, 1.0));
        let lightness = (lightness + lightness_delta).clamp(0.0, 1.0);
        let (adjusted_red, adjusted_green, adjusted_blue) = hsl_to_srgb(hue, saturation, lightness);

        // Now re-encode through the same path as `hex()` (sRGB→linear,
        // premultiplied).
        let linear_red = srgb_to_linear_f(adjusted_red);
        let linear_green = srgb_to_linear_f(adjusted_green);
        let linear_blue = srgb_to_linear_f(adjusted_blue);
        [linear_red * alpha, linear_green * alpha, linear_blue * alpha, alpha]
    }

    /// sRGB (0..1) → HSL as (hue, saturation, lightness), each in 0..1. Standard formula.
    fn srgb_to_hsl(red: f32, green: f32, blue: f32) -> (f32, f32, f32) {
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let lightness = (max + min) * 0.5;
        if (max - min).abs() < f32::EPSILON {
            // When: max and min coincide, the color is a pure grey with no
            // hue or saturation to recover.
            return (0.0, 0.0, lightness);
        }
        let spread = max - min;
        let saturation = if lightness > 0.5 {
            spread / (2.0 - max - min)
        } else {
            // When: lightness sits in the darker half, so the spread is normalized
            // against max + min rather than its reflection about white.
            spread / (max + min)
        };
        let hue = if (max - red).abs() < f32::EPSILON {
            ((green - blue) / spread) + if green < blue { 6.0 } else { 0.0 }
        } else if (max - green).abs() < f32::EPSILON {
            // When: `green` is the max channel, so hue comes from the green sector,
            // offsetting the blue-red spread by 2.
            ((blue - red) / spread) + 2.0
        } else {
            // When: neither `red` nor `green` is the max channel, so blue leads and
            // hue comes from the blue sector, offset by 4.
            ((red - green) / spread) + 4.0
        } / 6.0;
        (hue, saturation, lightness)
    }

    /// HSL (0..1) → sRGB (0..1).
    fn hsl_to_srgb(hue: f32, saturation: f32, lightness: f32) -> (f32, f32, f32) {
        if saturation.abs() < f32::EPSILON {
            // When: saturation is effectively zero, the color is grey, so every channel
            // equals the lightness and no hue sector applies.
            return (lightness, lightness, lightness);
        }
        let upper = if lightness < 0.5 {
            lightness * (1.0 + saturation)
        } else {
            // When: lightness sits in the lighter half, so the upper bound compresses
            // toward white as lightness + saturation * (1 - lightness), not scaled from black.
            lightness + saturation - lightness * saturation
        };
        let lower = 2.0 * lightness - upper;
        let hue_to_rgb = |lower: f32, upper: f32, mut channel_hue: f32| -> f32 {
            if channel_hue < 0.0 {
                channel_hue += 1.0;
            }
            if channel_hue > 1.0 {
                channel_hue -= 1.0;
            }
            if channel_hue < 1.0 / 6.0 {
                // When: channel_hue falls in the first sixth of the wheel, the channel is
                // still climbing from `lower` toward `upper`.
                return lower + (upper - lower) * 6.0 * channel_hue;
            }
            if channel_hue < 0.5 {
                // When: channel_hue falls in the second sixth, the channel is held at its
                // peak `upper` across the plateau.
                return upper;
            }
            if channel_hue < 2.0 / 3.0 {
                // When: channel_hue falls in the third sector, the channel descends from
                // `upper` back toward `lower`.
                return lower + (upper - lower) * (2.0 / 3.0 - channel_hue) * 6.0;
            }
            lower
        };
        (
            hue_to_rgb(lower, upper, hue + 1.0 / 3.0),
            hue_to_rgb(lower, upper, hue),
            hue_to_rgb(lower, upper, hue - 1.0 / 3.0),
        )
    }

    // --- Token accessors -------------------------------------------------
    //
    // These are `pub fn` (not `pub const`) because the sRGB→linear transform
    // involves `f32::powf`, which is not stable in const context. The
    // compiler inlines and folds each call.
    //
    // DEPRECATED: these constants are baked Tokyo Night values. New code
    // should derive chrome colors from the active theme via
    // [`UiPalette::from_theme`] (see crate root).

    /// `#0B0E14` fully opaque — base window background.
    #[allow(non_snake_case)]
    #[deprecated(
        note = "Use UiPalette::from_theme(theme).bg_base — chrome now follows the active theme"
    )]
    #[inline]
    pub fn BG_BASE() -> [f32; 4] {
        hex("#0B0E14FF")
    }
    /// `#10131A` @ 0.92 — elevated chrome (tab bar, overlays).
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BG_ELEVATED() -> [f32; 4] {
        hex("#10131AEB")
    }
    /// `#111520` fully opaque — modal/surface backgrounds.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BG_SURFACE() -> [f32; 4] {
        hex("#111520FF")
    }
    /// `#FFFFFF` @ 0.06 — hover overlay.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BG_HOVER() -> [f32; 4] {
        hex("#FFFFFF0F")
    }
    /// `#7AA2F7` @ 0.14 — active/selected tint.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BG_ACTIVE() -> [f32; 4] {
        hex("#7AA2F724")
    }
    /// `#FFFFFF` @ 0.08 — subtle separator/border.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BORDER_SUBTLE() -> [f32; 4] {
        hex("#FFFFFF14")
    }
    /// `#FFFFFF` @ 0.12 — emphasised border.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BORDER_STRONG() -> [f32; 4] {
        hex("#FFFFFF1F")
    }
    /// `#7AA2F7` @ 0.65 — focused element ring.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn BORDER_FOCUS() -> [f32; 4] {
        hex("#7AA2F7A6")
    }
    /// `#DDE6FF` — primary text.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn TEXT_PRIMARY() -> [f32; 4] {
        hex("#DDE6FFFF")
    }
    /// `#A9B1D6` — secondary text.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn TEXT_SECONDARY() -> [f32; 4] {
        hex("#A9B1D6FF")
    }
    /// `#7F849C` — muted text.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn TEXT_MUTED() -> [f32; 4] {
        hex("#7F849CFF")
    }
    /// `#565F89` — faint text (placeholders, hints).
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn TEXT_FAINT() -> [f32; 4] {
        hex("#565F89FF")
    }
    /// `#7AA2F7` — primary accent (blue).
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn ACCENT_BLUE() -> [f32; 4] {
        hex("#7AA2F7FF")
    }
    /// `#BB9AF7` — secondary accent (purple).
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn ACCENT_PURPLE() -> [f32; 4] {
        hex("#BB9AF7FF")
    }
    /// `#FF9E64` — tertiary accent (orange).
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn ACCENT_ORANGE() -> [f32; 4] {
        hex("#FF9E64FF")
    }
    /// `#F7768E` — destructive/danger.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn DANGER() -> [f32; 4] {
        hex("#F7768EFF")
    }
    /// `#05070D` @ 0.28 — modal scrim.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn SCRIM() -> [f32; 4] {
        hex("#05070D47")
    }
    /// `#7AA2F7` @ 0.26 — text selection highlight.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn SELECTION() -> [f32; 4] {
        hex("#7AA2F742")
    }
    /// `#E0AF68` @ 0.28 — search match highlight.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn SEARCH_MATCH() -> [f32; 4] {
        hex("#E0AF6847")
    }
    /// `#FF9E64` @ 0.42 — current search match highlight.
    #[allow(non_snake_case)]
    #[deprecated(note = "Use UiPalette::from_theme(theme) — chrome now follows the active theme")]
    #[inline]
    pub fn SEARCH_CURRENT() -> [f32; 4] {
        hex("#FF9E646B")
    }
}

/// Corner-radius scale.
pub mod radius {
    pub const SM: f32 = 6.0;
    pub const MD: f32 = 10.0;
    pub const LG: f32 = 14.0;
    pub const XL: f32 = 16.0;
}

/// Drop-shadow presets.
pub mod shadow {
    /// A drop-shadow specification (offset + blur + spread + premultiplied color).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct ShadowSpec {
        pub offset_x: f32,
        pub offset_y: f32,
        pub blur: f32,
        pub spread: f32,
        pub color: [f32; 4],
    }

    /// Small lift — hover states on tabs and buttons.
    pub const SM: ShadowSpec = ShadowSpec {
        offset_x: 0.0,
        offset_y: 1.0,
        blur: 2.0,
        spread: 0.0,
        // #00000033 — premultiplied: rgb = 0, a = 0.2
        color: [0.0, 0.0, 0.0, 0.2],
    };
    /// Medium lift — popovers and command palette.
    pub const MD: ShadowSpec = ShadowSpec {
        offset_x: 0.0,
        offset_y: 6.0,
        blur: 18.0,
        spread: 0.0,
        // #00000055 — a ≈ 0.333
        color: [0.0, 0.0, 0.0, 0.333],
    };
    /// Large lift — modal dialogs.
    pub const LG: ShadowSpec = ShadowSpec {
        offset_x: 0.0,
        offset_y: 18.0,
        blur: 48.0,
        spread: 0.0,
        // #00000080 — a = 0.5
        color: [0.0, 0.0, 0.0, 0.5],
    };
}

/// Spacing scale (in CSS pixels, unscaled).
pub mod spacing {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// Motion / easing tokens.
pub mod motion {
    /// 90 ms — micro-interactions (hover state).
    pub const FAST_MS: u32 = 90;
    /// 140 ms — standard chrome transitions.
    pub const BASE_MS: u32 = 140;
    /// 200 ms — modal enter/leave.
    pub const SLOW_MS: u32 = 200;

    /// Evaluate cubic-bezier `y(t)` with `P0=(0,0)`, `P3=(1,1)` and the
    /// given inner control-point y-coordinates.
    ///
    /// The CSS `cubic-bezier(x1, y1, x2, y2)` curve is parametric in
    /// `t ∈ [0, 1]`; here we treat the input `progress` directly as the curve
    /// parameter rather than solving for it from `x`. For the easing curves
    /// below this matches game-engine convention; the visual difference vs.
    /// the browser's `x`-solving form is imperceptible for animations on the
    /// 90–200 ms timescale used by SonicTerm chrome.
    #[inline]
    fn bezier_y(progress: f32, first_control_y: f32, second_control_y: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        let remaining = 1.0 - progress;
        3.0 * remaining * remaining * progress * first_control_y
            + 3.0 * remaining * progress * progress * second_control_y
            + progress * progress * progress
    }

    /// `cubic-bezier(0.16, 1, 0.3, 1)` — "spring-out".
    ///
    /// Decelerates aggressively with a soft overshoot feel; canonical curve
    /// for popovers and overlays appearing.
    #[inline]
    pub fn ease_spring_out(progress: f32) -> f32 {
        bezier_y(progress, 1.0, 1.0)
    }

    /// `cubic-bezier(0.2, 0, 0, 1)` — "ease-out-quint".
    ///
    /// Smooth deceleration; canonical curve for tab/pane motion.
    #[inline]
    pub fn ease_out_quint(progress: f32) -> f32 {
        bezier_y(progress, 0.0, 1.0)
    }
}

/// Typography ramps and platform UI fonts.
pub mod typography {
    /// A typographic ramp: pixel size, line-height in pixels, weight (100–900).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct TypeRamp {
        pub size_px: f32,
        pub line_px: f32,
        pub weight: u16,
    }

    /// Heading 1 — 20/28 @ 700.
    pub const H1: TypeRamp = TypeRamp { size_px: 20.0, line_px: 28.0, weight: 700 };
    /// Heading 2 — 16/24 @ 650.
    pub const H2: TypeRamp = TypeRamp { size_px: 16.0, line_px: 24.0, weight: 650 };
    /// Body — 13/20 @ 500.
    pub const BODY: TypeRamp = TypeRamp { size_px: 13.0, line_px: 20.0, weight: 500 };
    /// Body Strong — 13/20 @ 650.
    pub const BODY_STRONG: TypeRamp = TypeRamp { size_px: 13.0, line_px: 20.0, weight: 650 };
    /// Caption — 11/16 @ 500.
    pub const CAPTION: TypeRamp = TypeRamp { size_px: 11.0, line_px: 16.0, weight: 500 };
    /// Keycap — 11/16 @ 600.
    pub const KEYCAP: TypeRamp = TypeRamp { size_px: 11.0, line_px: 16.0, weight: 600 };

    /// Platform system UI font family.
    pub fn system_ui_family() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            ".AppleSystemUIFont"
        }
        #[cfg(target_os = "windows")]
        {
            "Segoe UI Variable Display"
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            "Inter"
        }
    }
}

#[cfg(test)]
#[path = "ui_tokens_tests.rs"]
mod ui_tokens_tests;
