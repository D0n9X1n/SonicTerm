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

#[test]
fn hex_parser() {
    assert_eq!(Hex("#1a2b3c".to_string()).rgb(), Some((0x1a, 0x2b, 0x3c)));
    assert_eq!(Hex("bogus".to_string()).rgb(), None);
}
