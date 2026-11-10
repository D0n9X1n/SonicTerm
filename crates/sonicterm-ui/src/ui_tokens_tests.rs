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
/// palette: the theme's `tab.bar_bg` when it differs from the terminal background, else
/// `ansi.black` when that differs, so the bars stay apart from the terminal. solarized-dark's bar
/// color (`#073642`, also its `ansi.black`) reads only 4.1:1 under its foreground, below the 4.5:1
/// text floor, so it alone keeps the derived darker shade.
#[test]
fn chrome_background_is_a_theme_palette_color() {
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let colors = &theme.colors;
        let derived = path.file_stem().is_some_and(|stem| stem == "solarized-dark");
        let expected = if derived {
            // When: derived, no palette candidate reads at 4.5:1 under the foreground.
            color::hex_with_lightness_delta(&colors.background.0, -0.08)
        } else if colors.tab.bar_bg.0 != colors.background.0 {
            color::hex(&colors.tab.bar_bg.0)
        } else {
            // When: bar_bg repeats the background, ansi.black separates the bars instead.
            color::hex(&colors.ansi.black.0)
        };
        let palette = UiPalette::from_theme(&theme);
        assert_eq!(palette.bg_base, expected, "{}", path.display());
        assert_ne!(palette.bg_base, color::hex(&colors.background.0), "{}", path.display());
        checked += 1;
    }
    assert_eq!(checked, 9, "the nine bundled themes");
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
