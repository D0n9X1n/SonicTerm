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
/// dark grey background `#141617` and gruvbox fg2 `#d5c4a1` foreground.
/// Regression guard for the visual parity fix.
#[test]
fn wezterm_theme_matches_actual_wezterm_colors() {
    let t = Theme::load(&bundled("wezterm.toml")).expect("load wezterm.toml");
    assert_eq!(t.colors.background.rgb(), Some((0x14, 0x16, 0x17)), "bg");
    assert_eq!(t.colors.foreground.rgb(), Some((0xd5, 0xc4, 0xa1)), "fg");
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

#[test]
fn hex_parser() {
    assert_eq!(Hex("#1a2b3c".to_string()).rgb(), Some((0x1a, 0x2b, 0x3c)));
    assert_eq!(Hex("bogus".to_string()).rgb(), None);
}
