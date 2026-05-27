//! UI design tokens — the cross-cutting foundation for all P0 visual work.
//!
//! These tokens are the single source of truth for chrome colors, radii,
//! shadows, spacing, motion curves, and typography.
//!
//! As of the theme-driven UI work, chrome colors are derived from the
//! active terminal [`Theme`] via [`UiPalette::from_theme`] — the palette
//! / prefs / tab bar inherit the user's chosen colors instead of being
//! locked to Tokyo Night. The previous Tokyo-Night-derived constants
//! (`color::ACCENT_BLUE`, `color::BG_BASE`, etc.) remain available but
//! `#[deprecated]` for backward compatibility.
//!
//! Colors are stored as **linear-sRGB premultiplied `[r, g, b, a]`** so they
//! can be uploaded to wgpu without further conversion. The [`color::hex`]
//! helper performs the sRGB→linear transform and the premultiply step.

use sonic_core::theme::Theme;

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
        let p = &theme.colors;
        let accent = color::hex(&p.tab.active_fg.0);
        let bg_elevated = color::hex(&p.background.0);
        let bg_base = color::hex_with_lightness_delta(&p.background.0, -0.08);
        let bg_surface = color::hex_with_lightness_delta(&p.background.0, 0.05);
        let fg = color::hex(&p.foreground.0);
        let text_secondary = color::hex_with_lightness_delta(&p.foreground.0, -0.15);
        let muted = color::hex(&p.bright.black.0);
        let text_faint = color::hex_with_lightness_delta(&p.bright.black.0, -0.15);

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
            danger: color::hex(&p.ansi.red.0),
            accent_orange: color::hex(&p.bright.yellow.0),
            accent_purple: color::hex(&p.ansi.magenta.0),
            scrim: color::with_alpha(color::hex("#000000"), 0.28),
            selection: color::with_alpha(accent, 0.26),
            search_match: color::with_alpha(color::hex(&p.ansi.yellow.0), 0.28),
            search_current: color::with_alpha(color::hex(&p.bright.yellow.0), 0.42),
        }
    }
}

impl From<&Theme> for UiPalette {
    fn from(theme: &Theme) -> Self {
        Self::from_theme(theme)
    }
}

/// Extension trait wired into `sonic_core::theme::Theme` so call sites can
/// write `theme.ui_palette()`.
pub trait ThemeUiPaletteExt {
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
    fn srgb_to_linear_f(v: f32) -> f32 {
        if v <= 0.040_448_237 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Convert 8-bit sRGB + alpha into linear-sRGB premultiplied `[r,g,b,a]`.
    #[inline]
    fn rgba8_premul_linear(r: u8, g: u8, b: u8, a: f32) -> [f32; 4] {
        let lr = srgb_to_linear_f(r as f32 / 255.0);
        let lg = srgb_to_linear_f(g as f32 / 255.0);
        let lb = srgb_to_linear_f(b as f32 / 255.0);
        let a = a.clamp(0.0, 1.0);
        [lr * a, lg * a, lb * a, a]
    }

    /// Parse `#RRGGBB` or `#RRGGBBAA` into linear-sRGB premultiplied `[r,g,b,a]`.
    ///
    /// Returns opaque black on any parse error (so token usage stays
    /// infallible at call sites).
    pub fn hex(s: &str) -> [f32; 4] {
        const SENTINEL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        let s = s.trim();
        let s = s.strip_prefix('#').unwrap_or(s);
        let bytes = s.as_bytes();
        if bytes.len() != 6 && bytes.len() != 8 {
            return SENTINEL;
        }
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            return SENTINEL;
        }
        #[inline]
        fn nyb(b: u8) -> u8 {
            match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => 0,
            }
        }
        #[inline]
        fn pair(b: &[u8], i: usize) -> u8 {
            (nyb(b[i]) << 4) | nyb(b[i + 1])
        }
        let r = pair(bytes, 0);
        let g = pair(bytes, 2);
        let b = pair(bytes, 4);
        let a = if bytes.len() == 8 { pair(bytes, 6) as f32 / 255.0 } else { 1.0 };
        rgba8_premul_linear(r, g, b, a)
    }

    /// Replace the alpha channel of a premultiplied token.
    ///
    /// Input is assumed to be linear-premultiplied (as produced by [`hex`]).
    /// We first un-premultiply by the existing alpha, then re-premultiply by
    /// the new one.
    pub fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
        let a = a.clamp(0.0, 1.0);
        let old_a = c[3];
        let (lr, lg, lb) = if old_a > f32::EPSILON {
            (c[0] / old_a, c[1] / old_a, c[2] / old_a)
        } else {
            (0.0, 0.0, 0.0)
        };
        [lr * a, lg * a, lb * a, a]
    }

    /// Adjust the lightness of a `#RRGGBB`/`#RRGGBBAA` color in HSL space
    /// by `delta` (typically `-0.15`..`+0.15`) and return the result as
    /// linear-sRGB premultiplied `[r,g,b,a]`. `delta > 0` lightens,
    /// `delta < 0` darkens. Clamped to `[0, 1]`.
    pub fn hex_with_lightness_delta(s: &str, delta: f32) -> [f32; 4] {
        const SENTINEL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        let trimmed = s.trim();
        let body = trimmed.strip_prefix('#').unwrap_or(trimmed);
        let bytes = body.as_bytes();
        if bytes.len() != 6 && bytes.len() != 8 {
            return SENTINEL;
        }
        if !bytes.iter().all(u8::is_ascii_hexdigit) {
            return SENTINEL;
        }
        #[inline]
        fn nyb(b: u8) -> u8 {
            match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => 0,
            }
        }
        #[inline]
        fn pair(b: &[u8], i: usize) -> u8 {
            (nyb(b[i]) << 4) | nyb(b[i + 1])
        }
        let r = pair(bytes, 0) as f32 / 255.0;
        let g = pair(bytes, 2) as f32 / 255.0;
        let b = pair(bytes, 4) as f32 / 255.0;
        let a = if bytes.len() == 8 { pair(bytes, 6) as f32 / 255.0 } else { 1.0 };

        // sRGB → HSL (sRGB-space lightness; this is the perceptual knob
        // designers expect for "+5%/-8% lightness" — *not* a linear-light
        // operation).
        let (h, s_hsl, l) = srgb_to_hsl(r, g, b);
        let l = (l + delta).clamp(0.0, 1.0);
        let (nr, ng, nb) = hsl_to_srgb(h, s_hsl, l);

        // Now re-encode through the same path as `hex()` (sRGB→linear,
        // premultiplied).
        let lr = srgb_to_linear_f(nr);
        let lg = srgb_to_linear_f(ng);
        let lb = srgb_to_linear_f(nb);
        [lr * a, lg * a, lb * a, a]
    }

    /// sRGB (0..1) → HSL (h in 0..1, s/l in 0..1). Standard formula.
    fn srgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) * 0.5;
        if (max - min).abs() < f32::EPSILON {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        let h = if (max - r).abs() < f32::EPSILON {
            ((g - b) / d) + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() < f32::EPSILON {
            ((b - r) / d) + 2.0
        } else {
            ((r - g) / d) + 4.0
        } / 6.0;
        (h, s, l)
    }

    /// HSL (0..1) → sRGB (0..1).
    fn hsl_to_srgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
        if s.abs() < f32::EPSILON {
            return (l, l, l);
        }
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        let hue_to_rgb = |p: f32, q: f32, mut t: f32| -> f32 {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                return p + (q - p) * 6.0 * t;
            }
            if t < 0.5 {
                return q;
            }
            if t < 2.0 / 3.0 {
                return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
            }
            p
        };
        (hue_to_rgb(p, q, h + 1.0 / 3.0), hue_to_rgb(p, q, h), hue_to_rgb(p, q, h - 1.0 / 3.0))
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
    /// `#10131A` @ 0.92 — elevated chrome (tab bar, prefs panel).
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
        pub dx: f32,
        pub dy: f32,
        pub blur: f32,
        pub spread: f32,
        pub color: [f32; 4],
    }

    /// Small lift — hover states on tabs and buttons.
    pub const SM: ShadowSpec = ShadowSpec {
        dx: 0.0,
        dy: 1.0,
        blur: 2.0,
        spread: 0.0,
        // #00000033 — premultiplied: rgb = 0, a = 0.2
        color: [0.0, 0.0, 0.0, 0.2],
    };
    /// Medium lift — popovers and command palette.
    pub const MD: ShadowSpec = ShadowSpec {
        dx: 0.0,
        dy: 6.0,
        blur: 18.0,
        spread: 0.0,
        // #00000055 — a ≈ 0.333
        color: [0.0, 0.0, 0.0, 0.333],
    };
    /// Large lift — modal dialogs.
    pub const LG: ShadowSpec = ShadowSpec {
        dx: 0.0,
        dy: 18.0,
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
    /// `t ∈ [0, 1]`; here we treat the input `t` directly as the curve
    /// parameter rather than solving for it from `x`. For the easing curves
    /// below this matches game-engine convention; the visual difference vs.
    /// the browser's `x`-solving form is imperceptible for animations on the
    /// 90–200 ms timescale used by Sonic chrome.
    #[inline]
    fn bezier_y(t: f32, y1: f32, y2: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let omt = 1.0 - t;
        3.0 * omt * omt * t * y1 + 3.0 * omt * t * t * y2 + t * t * t
    }

    /// `cubic-bezier(0.16, 1, 0.3, 1)` — "spring-out".
    ///
    /// Decelerates aggressively with a soft overshoot feel; canonical curve
    /// for popovers and overlays appearing.
    #[inline]
    pub fn ease_spring_out(t: f32) -> f32 {
        bezier_y(t, 1.0, 1.0)
    }

    /// `cubic-bezier(0.2, 0, 0, 1)` — "ease-out-quint".
    ///
    /// Smooth deceleration; canonical curve for tab/pane motion.
    #[inline]
    pub fn ease_out_quint(t: f32) -> f32 {
        bezier_y(t, 0.0, 1.0)
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
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)]
    fn hex_parses_RRGGBB_and_RRGGBBAA() {
        // #FFFFFF fully opaque → linear (1,1,1,1) premultiplied = (1,1,1,1)
        let white = color::hex("#FFFFFF");
        assert!((white[0] - 1.0).abs() < 1e-4);
        assert!((white[1] - 1.0).abs() < 1e-4);
        assert!((white[2] - 1.0).abs() < 1e-4);
        assert!((white[3] - 1.0).abs() < 1e-4);

        // #000000 fully opaque → (0,0,0,1) — works without leading #
        let black = color::hex("000000");
        assert_eq!(black, [0.0, 0.0, 0.0, 1.0]);

        // #FFFFFF00 → fully transparent; premultiplied RGB collapses to 0.
        let clear = color::hex("#FFFFFF00");
        assert_eq!(clear[3], 0.0);
        assert_eq!(clear[0], 0.0);
        assert_eq!(clear[1], 0.0);
        assert_eq!(clear[2], 0.0);

        // #FFFFFF80 → ~half alpha; premultiplied RGB ≈ a (since linear(1) = 1).
        let half = color::hex("#FFFFFF80");
        let a = 0x80 as f32 / 255.0;
        assert!((half[3] - a).abs() < 1e-4);
        assert!((half[0] - a).abs() < 1e-4);
        assert!((half[1] - a).abs() < 1e-4);
        assert!((half[2] - a).abs() < 1e-4);

        // Bad input → opaque-black sentinel (not a panic).
        assert_eq!(color::hex("nope"), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(color::hex("#12"), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(color::hex(""), [0.0, 0.0, 0.0, 1.0]);

        // sRGB→linear is applied: mid-grey is NOT 0.5 in linear.
        let mid = color::hex("#808080");
        assert!(mid[0] < 0.25, "expected linearised mid-grey < 0.25, got {}", mid[0]);
    }

    #[test]
    fn hex_non_ascii_does_not_panic() {
        // 6 chars / 18 bytes — exact char count of valid hex but multibyte.
        assert_eq!(color::hex("中中中中中中"), [0.0, 0.0, 0.0, 1.0]);
        // 3 chars / 9 bytes — different multibyte boundary.
        assert_eq!(color::hex("中中中"), [0.0, 0.0, 0.0, 1.0]);
        // With '#' prefix too.
        assert_eq!(color::hex("#中中中中中中"), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn hex_invalid_chars_returns_sentinel() {
        assert_eq!(color::hex("#ZZZZZZ"), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(color::hex("GGGGGG"), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(color::hex("#ZZZZZZZZ"), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn with_alpha_replaces_alpha_channel() {
        let opaque_blue = color::hex("#7AA2F7");
        let half = color::with_alpha(opaque_blue, 0.5);
        assert!((half[3] - 0.5).abs() < 1e-5);
        // Since old alpha was 1.0, new premultiplied RGB ≈ 0.5 × original.
        assert!((half[0] - opaque_blue[0] * 0.5).abs() < 1e-5);
        assert!((half[1] - opaque_blue[1] * 0.5).abs() < 1e-5);
        assert!((half[2] - opaque_blue[2] * 0.5).abs() < 1e-5);

        // Round-trip preserves RGB: with_alpha(with_alpha(c, 0.5), 1.0) ≈ c.
        let back = color::with_alpha(half, 1.0);
        assert!((back[0] - opaque_blue[0]).abs() < 1e-4);
        assert!((back[1] - opaque_blue[1]).abs() < 1e-4);
        assert!((back[2] - opaque_blue[2]).abs() < 1e-4);
        assert!((back[3] - 1.0).abs() < 1e-5);

        // Zero alpha collapses RGB entirely.
        let gone = color::with_alpha(opaque_blue, 0.0);
        assert_eq!(gone, [0.0, 0.0, 0.0, 0.0]);

        // Out-of-range alpha is clamped.
        let clamped = color::with_alpha(opaque_blue, 2.0);
        assert!((clamped[3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ease_spring_out_endpoints_0_and_1() {
        assert!((motion::ease_spring_out(0.0) - 0.0).abs() < 1e-6);
        assert!((motion::ease_spring_out(1.0) - 1.0).abs() < 1e-6);

        // Stays in [0, 1] and is monotonic on a 20-sample grid.
        let mut prev = 0.0;
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let v = motion::ease_spring_out(t);
            assert!((0.0..=1.0001).contains(&v), "out of range at t={t}: {v}");
            assert!(v + 1e-5 >= prev, "non-monotonic at t={t}: {v} < {prev}");
            prev = v;
        }

        // ease_out_quint endpoints too.
        assert!((motion::ease_out_quint(0.0) - 0.0).abs() < 1e-6);
        assert!((motion::ease_out_quint(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn typography_constants_have_expected_shape() {
        assert_eq!(typography::BODY.size_px, 13.0);
        assert_eq!(typography::BODY.line_px, 20.0);
        assert_eq!(typography::H1.weight, 700);
        assert!(!typography::system_ui_family().is_empty());
    }

    #[test]
    fn shadow_specs_have_nonzero_blur_and_increasing_depth() {
        const _: () = {
            assert!(shadow::SM.blur > 0.0);
            assert!(shadow::MD.blur > shadow::SM.blur);
            assert!(shadow::LG.blur > shadow::MD.blur);
            assert!(shadow::LG.dy > shadow::SM.dy);
        };
    }

    #[test]
    #[allow(deprecated)]
    fn color_tokens_are_premultiplied_and_in_range() {
        for c in [
            color::BG_BASE(),
            color::BG_ELEVATED(),
            color::BG_HOVER(),
            color::BORDER_FOCUS(),
            color::TEXT_PRIMARY(),
            color::ACCENT_BLUE(),
            color::DANGER(),
            color::SELECTION(),
            color::SEARCH_CURRENT(),
        ] {
            for ch in c {
                assert!((0.0..=1.0001).contains(&ch), "channel out of range: {ch}");
            }
            // Premultiplied invariant: each RGB ≤ alpha.
            assert!(c[0] <= c[3] + 1e-5);
            assert!(c[1] <= c[3] + 1e-5);
            assert!(c[2] <= c[3] + 1e-5);
        }
    }

    /// Backwards-compat: the deprecated Tokyo-Night-derived constants
    /// must still compile and return sane values, so any pre-existing
    /// call site continues to work until migrated.
    #[test]
    #[allow(deprecated)]
    fn deprecated_const_still_compiles() {
        let _bg = color::BG_BASE();
        let _accent = color::ACCENT_BLUE();
        let _danger = color::DANGER();
        // Premultiplied invariant.
        assert!(_accent[0] <= _accent[3] + 1e-5);
    }

    /// `hex_with_lightness_delta(_, 0.0)` should be approximately the
    /// identity transform (modulo the 8-bit → f32 quantisation in
    /// `hex()`).
    #[test]
    fn hex_with_lightness_delta_zero_is_identity() {
        let base = color::hex("#1d2021");
        let same = color::hex_with_lightness_delta("#1d2021", 0.0);
        for (i, b) in base.iter().enumerate() {
            assert!(
                (b - same[i]).abs() < 1e-3,
                "channel {i} drifted: base={} same={}",
                b,
                same[i]
            );
        }
    }

    /// Positive delta lightens (every RGB channel increases or stays);
    /// negative delta darkens.
    #[test]
    fn hex_with_lightness_delta_monotonic() {
        let base = color::hex("#3c3836");
        let lighter = color::hex_with_lightness_delta("#3c3836", 0.20);
        let darker = color::hex_with_lightness_delta("#3c3836", -0.20);
        // Compare luminance (sum of RGB, since alpha is identical = 1).
        let sum = |c: [f32; 4]| c[0] + c[1] + c[2];
        assert!(sum(lighter) > sum(base) + 1e-3, "lighten did not increase RGB sum");
        assert!(sum(darker) < sum(base) - 1e-3, "darken did not decrease RGB sum");
    }

    fn test_theme(active_fg: &str, bg: &str, fg: &str) -> Theme {
        use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
        let h = |s: &str| Hex(s.to_string());
        Theme {
            name: "test".into(),
            appearance: Appearance::Dark,
            colors: Palette {
                background: h(bg),
                foreground: h(fg),
                cursor: h(fg),
                cursor_text: h(bg),
                selection_bg: h("#3c3836"),
                selection_fg: h(fg),
                ansi: AnsiColors {
                    black: h("#000000"),
                    red: h("#cc241d"),
                    green: h("#98971a"),
                    yellow: h("#d79921"),
                    blue: h("#458588"),
                    magenta: h("#b16286"),
                    cyan: h("#689d6a"),
                    white: h("#a89984"),
                },
                bright: AnsiColors {
                    black: h("#928374"),
                    red: h("#fb4934"),
                    green: h("#b8bb26"),
                    yellow: h("#fabd2f"),
                    blue: h("#83a598"),
                    magenta: h("#d3869b"),
                    cyan: h("#8ec07c"),
                    white: h("#ebdbb2"),
                },
                tab: TabColors {
                    bar_bg: h(bg),
                    active_bg: h("#3c3836"),
                    active_fg: h(active_fg),
                    inactive_bg: h(bg),
                    inactive_fg: h("#928374"),
                    hover_bg: h("#32302f"),
                    hover_fg: h("#d5c4a1"),
                    close_button_fg: h("#fb4934"),
                },
            },
        }
    }

    /// Gruvbox Dark Hard's chrome accent must be `#fabd2f` (bright_yellow,
    /// the canonical gruvbox gold), NOT Tokyo Night's `#7AA2F7` blue.
    #[test]
    fn ui_palette_gruvbox_accent_is_bright_yellow() {
        let theme = test_theme("#fabd2f", "#1d2021", "#ebdbb2");
        let p = theme.ui_palette();
        let expected = color::hex("#fabd2f");
        for (i, exp) in expected.iter().enumerate() {
            assert!(
                (p.accent[i] - exp).abs() < 1e-4,
                "accent channel {i} mismatch: got {} expected {}",
                p.accent[i],
                exp
            );
        }
    }

    /// Tokyo Night's chrome accent must still resolve to its canonical
    /// `#7AA2F7` blue — proves the palette tracks the theme.
    #[test]
    fn ui_palette_tokyo_night_accent_is_blue() {
        let theme = test_theme("#7AA2F7", "#1A1B26", "#C0CAF5");
        let p = theme.ui_palette();
        let expected = color::hex("#7AA2F7");
        for (i, exp) in expected.iter().enumerate() {
            assert!(
                (p.accent[i] - exp).abs() < 1e-4,
                "accent channel {i} mismatch: got {} expected {}",
                p.accent[i],
                exp
            );
        }
    }

    /// For a dark theme, the derived `bg_base` (background -8% lightness)
    /// must end up darker than the theme background itself.
    #[test]
    fn ui_palette_dark_themes_select_dark_chrome_bg() {
        let theme = test_theme("#fabd2f", "#1d2021", "#ebdbb2");
        let p = theme.ui_palette();
        let base_bg = color::hex("#1d2021");
        let sum = |c: [f32; 4]| c[0] + c[1] + c[2];
        assert!(
            sum(p.bg_base) <= sum(base_bg) + 1e-4,
            "bg_base should be darker than (or equal to) the theme background"
        );
        // bg_surface should be at least as light as the theme background.
        assert!(
            sum(p.bg_surface) >= sum(base_bg) - 1e-4,
            "bg_surface should be lighter than (or equal to) the theme background"
        );
    }

    /// End-to-end: building a palette from gruvbox-dark-hard's actual
    /// values (as embedded in `assets/themes/gruvbox-dark-hard.toml`)
    /// must yield gold accent on dark gruvbox brown — i.e. the
    /// palette / tabs / prefs render in gruvbox colors, not Tokyo Night.
    #[test]
    fn palette_render_uses_active_theme_accent() {
        let theme = test_theme("#fabd2f", "#1d2021", "#ebdbb2");
        let p = theme.ui_palette();
        // accent gold
        let gold = color::hex("#fabd2f");
        assert!((p.accent[0] - gold[0]).abs() < 1e-4);
        // text on dark gruvbox brown
        let brown = color::hex("#ebdbb2");
        assert!((p.text_primary[0] - brown[0]).abs() < 1e-4);
        // bg around #1d2021
        let bg = color::hex("#1d2021");
        assert!((p.bg_elevated[0] - bg[0]).abs() < 1e-4);
        // accent-tinted active surface must carry accent hue (red > blue
        // for gold), not Tokyo-Night blue (where blue > red).
        assert!(
            p.bg_active[0] > p.bg_active[2],
            "active tint must be gold (R>B), got R={} B={}",
            p.bg_active[0],
            p.bg_active[2]
        );
    }
}
