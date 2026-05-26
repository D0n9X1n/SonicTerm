use sonic_core::theme::*;

fn bundled(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/themes").join(name)
}

#[test]
fn loads_all_bundled_themes() {
    for n in [
        "tokyo-night.toml",
        "dracula.toml",
        "nord.toml",
        "catppuccin-mocha.toml",
        "wezterm.toml",
        "gruvbox-dark-hard.toml",
    ] {
        let t = Theme::load(&bundled(n)).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert!(!t.name.is_empty());
        assert!(t.colors.background.rgb().is_some(), "{n} bg parse");
    }
}

/// Visual parity: tab bar should be flush with body background (no chunky
/// lighter band). Verified for the wezterm-style theme and gruvbox dark hard.
#[test]
fn tab_bar_bg_is_flush_with_body() {
    for n in ["wezterm.toml", "gruvbox-dark-hard.toml"] {
        let t = Theme::load(&bundled(n)).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert_eq!(
            t.colors.tab.bar_bg.rgb(),
            t.colors.background.rgb(),
            "{n}: tab.bar_bg must equal background for visual parity",
        );
    }
}

/// Pin the wezterm theme to the exact colors that match the actual WezTerm
/// app's default appearance. Sampled from a running WezTerm window: warm
/// dark grey background `#141617` and gruvbox cream-tan `#cfbc97` foreground
/// (refined from `#d5c4a1` after side-by-side parity diff vs the user's
/// running WezTerm). Regression guard for the visual parity fix.
#[test]
fn wezterm_theme_matches_actual_wezterm_colors() {
    let t = Theme::load(&bundled("wezterm.toml")).expect("load wezterm.toml");
    assert_eq!(t.colors.background.rgb(), Some((0x14, 0x16, 0x17)), "bg");
    assert_eq!(t.colors.foreground.rgb(), Some((0xcf, 0xbc, 0x97)), "fg");
    assert_eq!(t.colors.tab.bar_bg.rgb(), Some((0x14, 0x16, 0x17)), "tab.bar_bg flush");
    assert_eq!(t.colors.tab.active_bg.rgb(), Some((0x14, 0x16, 0x17)), "tab.active_bg flush");
    assert_eq!(t.colors.tab.inactive_bg.rgb(), Some((0x14, 0x16, 0x17)), "tab.inactive_bg");
}

/// Pin the WezTerm accent palette: gruvbox bright yellow `#fabd2f` on the
/// active tab, neutral gray `#928374` on inactive tabs, and `#d5c4a1` on
/// hover — matching the user's `wezterm.lua` config exactly.
#[test]
fn wezterm_theme_pins_accent_colors() {
    let t = Theme::load(&bundled("wezterm.toml")).expect("load wezterm.toml");
    assert_eq!(t.colors.tab.active_fg.rgb(), Some((0xfa, 0xbd, 0x2f)), "active_fg gold");
    assert_eq!(t.colors.tab.inactive_fg.rgb(), Some((0x92, 0x83, 0x74)), "inactive_fg dim");
    assert_eq!(t.colors.tab.hover_fg.rgb(), Some((0xd5, 0xc4, 0xa1)), "hover_fg cream");
}

/// Pin the ANSI 16-color palette to WezTerm's built-in
/// "Gruvbox dark, hard (base16)" scheme — what the user selects via
/// `color_scheme = "Gruvbox dark, hard (base16)"` in wezterm.lua.
/// Exact-byte parity is the contract; any drift here breaks visual
/// parity with the user's running WezTerm.
#[test]
fn wezterm_theme_pins_gruvbox_hard_base16_ansi_palette() {
    let t = Theme::load(&bundled("wezterm.toml")).expect("load wezterm.toml");
    // Normal: ansi = [base00, base08, base0B, base0A, base0D, base0E, base0C, base05]
    assert_eq!(t.colors.ansi.black.rgb(), Some((0x1d, 0x20, 0x21)), "ansi.black base00");
    assert_eq!(t.colors.ansi.red.rgb(), Some((0xfb, 0x49, 0x34)), "ansi.red base08");
    assert_eq!(t.colors.ansi.green.rgb(), Some((0xb8, 0xbb, 0x26)), "ansi.green base0B");
    assert_eq!(t.colors.ansi.yellow.rgb(), Some((0xfa, 0xbd, 0x2f)), "ansi.yellow base0A");
    assert_eq!(t.colors.ansi.blue.rgb(), Some((0x83, 0xa5, 0x98)), "ansi.blue base0D");
    assert_eq!(t.colors.ansi.magenta.rgb(), Some((0xd3, 0x86, 0x9b)), "ansi.magenta base0E");
    assert_eq!(t.colors.ansi.cyan.rgb(), Some((0x8e, 0xc0, 0x7c)), "ansi.cyan base0C");
    assert_eq!(t.colors.ansi.white.rgb(), Some((0xd5, 0xc4, 0xa1)), "ansi.white base05");
    // Bright: brights = [base03, base08, base0B, base0A, base0D, base0E, base0C, base07]
    assert_eq!(t.colors.bright.black.rgb(), Some((0x66, 0x5c, 0x54)), "bright.black base03");
    assert_eq!(t.colors.bright.red.rgb(), Some((0xfb, 0x49, 0x34)), "bright.red base08");
    assert_eq!(t.colors.bright.green.rgb(), Some((0xb8, 0xbb, 0x26)), "bright.green base0B");
    assert_eq!(t.colors.bright.yellow.rgb(), Some((0xfa, 0xbd, 0x2f)), "bright.yellow base0A");
    assert_eq!(t.colors.bright.blue.rgb(), Some((0x83, 0xa5, 0x98)), "bright.blue base0D");
    assert_eq!(t.colors.bright.magenta.rgb(), Some((0xd3, 0x86, 0x9b)), "bright.magenta base0E");
    assert_eq!(t.colors.bright.cyan.rgb(), Some((0x8e, 0xc0, 0x7c)), "bright.cyan base0C");
    assert_eq!(t.colors.bright.white.rgb(), Some((0xfb, 0xf1, 0xc7)), "bright.white base07");
}

#[test]
fn hex_parser() {
    assert_eq!(Hex("#1a2b3c".to_string()).rgb(), Some((0x1a, 0x2b, 0x3c)));
    assert_eq!(Hex("bogus".to_string()).rgb(), None);
}
