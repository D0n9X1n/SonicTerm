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

/// Contrast of `text` on `colors`' chrome background, and on its previous color, the floor.
fn contrast_and_floor(theme: &Theme, text: &str, floor_cap: f32) -> (f32, f32) {
    let text = color::hex(text);
    let palette = UiPalette::from_theme(theme);
    let previous = previous_chrome_background(&theme.colors);
    (contrast_linear(text, palette.bg_base), contrast_linear(text, previous).min(floor_cap))
}

/// In every bundled theme the chrome background (tab bar, status strip, titlebar band) is darker
/// than the terminal, and its foreground and active tab title read at least as well as they did
/// on the previous chrome color.
#[test]
fn chrome_background_is_darker_than_the_terminal_in_every_bundled_theme() {
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let colors = &theme.colors;
        let palette = UiPalette::from_theme(&theme);
        let background = color::hex(&colors.background.0);
        assert!(
            relative_luminance(palette.bg_base) < relative_luminance(background),
            "{}: the bars are darker than the terminal",
            path.display()
        );
        for (text, cap) in [
            (&colors.foreground.0, CHROME_FOREGROUND_CONTRAST),
            (&colors.tab.active_fg.0, CHROME_ACTIVE_CONTRAST),
        ] {
            let (now, floor) = contrast_and_floor(&theme, text, cap);
            assert!(now >= floor, "{}: {text} {now} under {floor}", path.display());
        }
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
}

/// A `tab.bar_bg` lighter than the background, such as solarized-dark's, is skipped, so the bars
/// stay below the terminal.
#[test]
fn chrome_background_skips_a_lighter_bar_color() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#002b36".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#073642".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_scale("#002b36", 0.95));
}

/// No reviewed theme reads worse than on the previous chrome color: for each, the foreground
/// and the active tab title read at least as well, up to 3:1 and 4.5:1. They cover a dim
/// foreground, a dim active title on a near-black background, a light theme whose previous bars
/// were its lighter `bar_bg`, a dark theme whose title needs a deeper shade, and a foreground-
/// colored `bar_bg` under a white title.
#[test]
fn chrome_background_never_reads_worse_than_before() {
    let fixtures = [
        ("#101010", "#5a5a5a", "#ffffff", "#101010", "#101010"),
        ("#101010", "#ffffff", "#767676", "#101010", "#101010"),
        ("#fafafa", "#202020", "#767676", "#ffffff", "#1d2021"),
        ("#202020", "#ffffff", "#7c7c7c", "#202020", "#202020"),
        ("#fafafa", "#202020", "#ffffff", "#202020", "#1d2021"),
        ("#202020", "#ffffff", "#080000", "#080000", "#202020"),
    ];
    for (background, foreground, active, bar, black) in fixtures {
        let mut theme = Theme::default();
        theme.colors.background = sonicterm_cfg::theme::Hex(background.into());
        theme.colors.foreground = sonicterm_cfg::theme::Hex(foreground.into());
        theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex(active.into());
        theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex(bar.into());
        theme.colors.ansi.black = sonicterm_cfg::theme::Hex(black.into());
        let label = format!("{background} {foreground} {active} {bar}");
        let (fg_now, fg_floor) = contrast_and_floor(&theme, foreground, CHROME_FOREGROUND_CONTRAST);
        assert!(fg_now >= fg_floor, "{label}: foreground {fg_now} under {fg_floor}");
        let (act_now, act_floor) = contrast_and_floor(&theme, active, CHROME_ACTIVE_CONTRAST);
        assert!(act_now >= act_floor, "{label}: active title {act_now} under {act_floor}");
    }
}

/// A dark theme's light active title that misses its floor on the 5% shade gets a deeper shade it
/// reads on: `#7c7c7c` on `#202020` falls short down to 60% lightness and reads at 40%.
#[test]
fn chrome_background_deepens_until_the_active_title_reads() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.ansi.black = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#7c7c7c".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex_with_lightness_scale("#202020", 0.4));
}

/// A light theme whose previous bars were its own lighter `tab.bar_bg`, because no darker color
/// kept the active title readable, keeps that color: `#767676` reads at 4.5:1 on `#ffffff` and on
/// no darker shade of `#fafafa`.
#[test]
fn chrome_background_keeps_the_previous_color_when_no_darker_one_reads() {
    let mut theme = Theme::default();
    theme.colors.background = sonicterm_cfg::theme::Hex("#fafafa".into());
    theme.colors.foreground = sonicterm_cfg::theme::Hex("#202020".into());
    theme.colors.tab.bar_bg = sonicterm_cfg::theme::Hex("#ffffff".into());
    theme.colors.tab.active_fg = sonicterm_cfg::theme::Hex("#767676".into());
    let palette = UiPalette::from_theme(&theme);
    assert_eq!(palette.bg_base, color::hex("#ffffff"));
}
