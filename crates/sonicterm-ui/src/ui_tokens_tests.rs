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

/// In every bundled theme the chrome background (tab bar, status strip, titlebar band) is the
/// theme's own `tab.bar_bg`, unfiltered.
#[test]
fn chrome_background_is_the_bar_color_in_every_bundled_theme() {
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let palette = UiPalette::from_theme(&theme);
        assert_eq!(palette.bg_base, color::hex(&theme.colors.tab.bar_bg.0), "{}", path.display());
        checked += 1;
    }
    assert!(checked > 0, "the bundled themes are read");
}

/// The bundled default theme's bars use its own `tab.bar_bg`, #101010.
#[test]
fn bundled_default_theme_bars_use_its_bar_color() {
    let theme = Theme::bundled_default();
    assert_eq!(theme.colors.tab.bar_bg.0, "#101010");
    assert_eq!(UiPalette::from_theme(&theme).bg_base, color::hex("#101010"));
}

/// The bars take `tab.bar_bg` as the theme sets it, whether it repeats the background, is lighter
/// than it, or is darker: no shade is derived and no contrast filter replaces it.
#[test]
fn chrome_background_takes_the_bar_color_unchanged() {
    for (background, bar) in
        [("#141617", "#141617"), ("#002b36", "#073642"), ("#fafafa", "#ffffff")]
    {
        let mut theme = Theme::default();
        theme.colors.background = sonicterm_cfg::theme::Hex(background.into());
        theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex(bar.into());
        theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
        theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#ffffff".into());
        let palette = UiPalette::from_theme(&theme);
        assert_eq!(palette.bg_base, color::hex(bar), "{background} {bar}");
    }
}
