use sonicterm_cfg::{
    config::AccessibilityConfig,
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

fn colors() -> AnsiColors {
    AnsiColors {
        black: Hex("#000000".to_string()),
        red: Hex("#ff0000".to_string()),
        green: Hex("#00ff00".to_string()),
        yellow: Hex("#ffff00".to_string()),
        blue: Hex("#0000ff".to_string()),
        magenta: Hex("#ff00ff".to_string()),
        cyan: Hex("#00ffff".to_string()),
        white: Hex("#ffffff".to_string()),
    }
}

fn theme() -> Theme {
    Theme {
        name: "test".to_string(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: Hex("#282828".to_string()),
            foreground: Hex("#ebdbb2".to_string()),
            cursor: Hex("#fabd2f".to_string()),
            cursor_text: Hex("#282828".to_string()),
            selection_bg: Hex("#504945".to_string()),
            selection_fg: Hex("#ebdbb2".to_string()),
            ansi: colors(),
            bright: colors(),
            tab: TabColors {
                bar_bg: Hex("#1d2021".to_string()),
                active_bg: Hex("#282828".to_string()),
                active_fg: Hex("#fabd2f".to_string()),
                inactive_bg: Hex("#3c3836".to_string()),
                inactive_fg: Hex("#a89984".to_string()),
                hover_bg: Hex("#504945".to_string()),
                hover_fg: Hex("#ebdbb2".to_string()),
                close_button_fg: Hex("#fb4934".to_string()),
            },
        },
    }
}

#[test]
fn high_contrast_overrides_foreground_and_background() {
    let mut theme = theme();
    theme.apply_accessibility(&AccessibilityConfig {
        high_contrast: true,
        ..AccessibilityConfig::default()
    });

    assert_eq!(theme.colors.foreground.rgb(), Some((255, 255, 255)));
    assert_eq!(theme.colors.background.rgb(), Some((0, 0, 0)));
}

#[test]
fn disabled_high_contrast_preserves_theme_colors() {
    let mut theme = theme();
    theme.apply_accessibility(&AccessibilityConfig::default());

    assert_eq!(theme.colors.foreground.rgb(), Some((0xeb, 0xdb, 0xb2)));
    assert_eq!(theme.colors.background.rgb(), Some((0x28, 0x28, 0x28)));
}
