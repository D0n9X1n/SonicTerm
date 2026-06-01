//! Migrated from `crates/sonicterm-ui/src/ui_tokens.rs` inline `mod tests`.

use sonicterm_ui::ui_tokens::*;

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
        assert!((b - same[i]).abs() < 1e-3, "channel {i} drifted: base={} same={}", b, same[i]);
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

fn test_theme(active_fg: &str, bg: &str, fg: &str) -> sonicterm_cfg::theme::Theme {
    use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
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
/// palette / tabs render in gruvbox colors, not Tokyo Night.
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
