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

/// The chrome background (tab bar, status strip, titlebar band) is never lighter than the
/// terminal in any bundled theme: a theme's own `tab.bar_bg` when it is darker than the
/// background, else the background darkened 4%; every bundled theme's text reads on that shade.
#[test]
fn chrome_background_is_never_lighter_than_the_terminal() {
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let colors = &theme.colors;
        let background = color::hex(&colors.background.0);
        let bar = color::hex(&colors.tab.bar_bg.0);
        let expected = if relative_luminance(bar) < relative_luminance(background) {
            // When: the theme's bar color is darker than the terminal, the bars use it.
            bar
        } else {
            color::hex_with_lightness_delta(&colors.background.0, CHROME_SHADE_DELTA)
        };
        let palette = UiPalette::from_theme(&theme);
        assert_eq!(palette.bg_base, expected, "{}", path.display());
        assert!(
            relative_luminance(palette.bg_base) < relative_luminance(background),
            "{}: the bars are darker than the terminal",
            path.display()
        );
        checked += 1;
    }
    assert_eq!(checked, 9, "the nine bundled themes");
}

/// A theme whose `tab.bar_bg` repeats the background, as Apollo and the bundled wezterm theme do,
/// gets the background darkened 4%, not its lighter `ansi.black`.
#[test]
fn chrome_background_darkens_a_background_the_bar_color_repeats() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#141617".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#141617".into());
    theme.colors.ansi.black = sonicterm_cfg::theme::Hex("#1d2021".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#141617", -0.04));
}

/// A `tab.bar_bg` lighter than the background, such as solarized-dark's, is skipped, so the bars
/// never stand out lighter than the terminal.
#[test]
fn chrome_background_skips_a_lighter_bar_color() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#002b36".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#073642".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#002b36", -0.04));
}

/// A darker `tab.bar_bg` the active tab title fades into is skipped: with `tab.bar_bg` and
/// `tab.active_fg` both `#080000`, the active tab and a focused band's values would vanish there.
/// The title fades on every dark shade too, so the bars take the deeper shade, the last fallback.
#[test]
fn chrome_background_skips_a_color_the_active_title_fades_into() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#080000".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#080000".into());
    let palette = UiPalette::from_theme(&theme);
    assert_ne!(palette.bg_base, color::hex("#080000"), "a bar the title fades into is skipped");
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#202020", -0.08));
}

/// When the active tab title would fall under 4.5:1 on the 4% shade, the bars take the deeper 8%
/// shade, on which it still reads: `#7c7c7c` on a `#202020` background reads at 4.7:1 there.
#[test]
fn chrome_background_deepens_when_the_active_title_fades_on_the_shade() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.ansi.black = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#7c7c7c".into());
    let palette = UiPalette::from_theme(&theme);
    let deep = color::hex_with_lightness_delta("#202020", -0.08);
    assert_eq!(palette.bg_base, deep);
    let active = color::hex("#7c7c7c");
    assert!(contrast_linear(active, palette.bg_base) >= CHROME_ACTIVE_CONTRAST);
}

/// A light theme that leaves `tab.bar_bg` at its dark default does not get dark bars under its
/// dark text: a bar color the foreground cannot be read on is skipped, and the bars are the
/// background darkened 4%, still darker than the terminal.
#[test]
fn chrome_background_skips_palette_colors_the_text_cannot_be_read_on() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#fafafa".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#202020".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_delta("#fafafa", -0.04));
}
