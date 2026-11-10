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
/// background, else the background at 95% of its lightness; every bundled theme reads on it.
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
            color::hex_with_lightness_scale(&colors.background.0, CHROME_SHADE_SCALE)
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
/// gets the background at 95% of its lightness: just darker than the terminal, not its lighter
/// `ansi.black`.
#[test]
fn chrome_background_darkens_a_background_the_bar_color_repeats() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#141617".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#141617".into());
    theme.colors.ansi.black = sonicterm_cfg::theme::Hex("#1d2021".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_scale("#141617", 0.95));
    let background = color::hex("#141617");
    assert!(relative_luminance(palette.bg_base) < relative_luminance(background));
}

/// A `tab.bar_bg` lighter than the background, such as solarized-dark's, is skipped, so the bars
/// never stand out lighter than the terminal.
#[test]
fn chrome_background_skips_a_lighter_bar_color() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#002b36".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#073642".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_scale("#002b36", 0.95));
}

/// A darker `tab.bar_bg` the active tab title fades into is skipped: with `tab.bar_bg` and
/// `tab.active_fg` both `#080000`, the active tab and a focused band's values would vanish there.
/// The title cannot read on the shade either, so the bars keep the background itself.
#[test]
fn chrome_background_skips_a_color_the_active_title_fades_into() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#080000".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#080000".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex("#202020"));
}

/// The chrome never reads worse than the terminal: when the active tab title would fall under
/// 4.5:1 on every darker candidate (`#767676` on a `#fafafa` theme and its 95% shade), the bars
/// keep the background, so the title reads exactly as it does on the terminal.
#[test]
fn chrome_background_keeps_the_background_when_no_shade_reads() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#fafafa".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#767676".into());
    let active = color::hex("#767676");
    let shade = color::hex_with_lightness_scale("#fafafa", CHROME_SHADE_SCALE);
    assert!(contrast_linear(active, shade) < CHROME_ACTIVE_CONTRAST, "setup: the shade fails");
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex("#fafafa"));
}

/// A light theme that leaves `tab.bar_bg` at its dark default does not get dark bars under its
/// dark text: a bar color the foreground cannot be read on is skipped, and the bars are the
/// background at 95% of its lightness, still darker than the terminal.
#[test]
fn chrome_background_skips_palette_colors_the_text_cannot_be_read_on() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#fafafa".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#202020".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_scale("#fafafa", 0.95));
}
