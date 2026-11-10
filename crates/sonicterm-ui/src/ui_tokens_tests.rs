use super::*;

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.000_01, "{actual} != {expected}");
}

/// Hex conversion decodes alpha and stores premultiplied linear channels.
#[test]
fn hex_decodes_and_premultiplies_alpha() {
    let color = color::hex("#ff000080");
    let alpha = 128.0 / 255.0;
    close(color[0], alpha);
    close(color[1], 0.0);
    close(color[2], 0.0);
    close(color[3], alpha);
}

/// Malformed colors return the documented opaque-black sentinel.
#[test]
fn malformed_hex_is_opaque_black() {
    assert_eq!(color::hex("#xyz"), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(color::hex("#12345g"), [0.0, 0.0, 0.0, 1.0]);
}

/// Replacing alpha unpremultiplies first, preserving the source color ratio.
#[test]
fn with_alpha_preserves_unpremultiplied_color() {
    let adjusted = color::with_alpha(color::hex("#ff000080"), 0.25);
    close(adjusted[0], 0.25);
    close(adjusted[1], 0.0);
    close(adjusted[2], 0.0);
    close(adjusted[3], 0.25);
}

/// Theme palette fields come from the documented theme channels and alpha tokens.
#[test]
fn palette_derives_from_theme_channels() {
    let theme = Theme::default();
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_elevated, color::hex(&theme.colors.background.0));
    assert_eq!(palette.text_primary, color::hex(&theme.colors.foreground.0));
    close(palette.selection[3], 0.26);
    close(palette.border_focus[3], 0.65);
}

/// The chrome background (tab bar, status strip, titlebar band) is a color from the theme's own
/// palette in every bundled theme: its `tab.bar_bg` when that differs from the terminal
/// background, else its `ansi.black`, so the bars stay apart from the terminal and are never the
/// near-black derived shade.
#[test]
fn chrome_background_is_a_theme_palette_color() {
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let colors = &theme.colors;
        let expected = if colors.tab.bar_bg.0 != colors.background.0 {
            &colors.tab.bar_bg.0
        } else {
            // When: bar_bg repeats the background, ansi.black separates the bars instead.
            &colors.ansi.black.0
        };
        let palette = UiPalette::from_theme(&theme);
        assert_eq!(palette.bg_base, color::hex(expected), "{}", path.display());
        let derived = color::hex_with_lightness_delta(&colors.background.0, -0.08);
        assert_ne!(palette.bg_base, derived, "{}: not the near-black shade", path.display());
        checked += 1;
    }
    assert_eq!(checked, 9, "the nine bundled themes");
}

/// A palette color the active tab title fades into is skipped: with `ansi.black` and
/// `tab.active_fg` both `#800000`, the active tab and a focused band's values would vanish, so
/// the bars keep the derived shade.
#[test]
fn chrome_background_skips_a_color_the_active_title_fades_into() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#101010".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#101010".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.ansi.black = sonicterm_cfg::theme::Hex("#800000".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#800000".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#101010", -0.08));
}

/// A palette color at or below the derived shade, such as a black `tab.bar_bg`, is skipped, so
/// the bars are never black; the next palette color is used.
#[test]
fn chrome_background_skips_black() {
    let mut theme = Theme::default();
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#000000".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex(&theme.colors.ansi.black.0));
}

/// A theme whose `tab.bar_bg` and `ansi.black` both repeat the background keeps the derived,
/// slightly darker shade, so its bars still stand apart from the terminal.
#[test]
fn chrome_background_falls_back_to_a_shade_when_the_palette_has_none() {
    let mut theme = Theme::default();
    let background = theme.colors.background.clone();
    theme.colors.tab.bar_bg = background.clone();
    theme.colors.ansi.black = background.clone();
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta(&background.0, -0.08));
}

/// A light theme that leaves `tab.bar_bg` and `ansi.black` at their dark defaults does not get
/// dark bars under its dark text: candidates the foreground cannot be read on are skipped.
#[test]
fn chrome_background_skips_palette_colors_the_text_cannot_be_read_on() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#fafafa".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#202020".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#fafafa", -0.08));
}
