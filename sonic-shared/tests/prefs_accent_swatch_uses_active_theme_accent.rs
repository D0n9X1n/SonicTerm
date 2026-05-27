//! Regression for PR #119 review: the Appearance category's
//! "Accent" swatch was hardcoded to `[0x7a, 0xa2, 0xf7, 0xff]`
//! (Tokyo Night blue) in `sonic-shared/src/prefs/state.rs`,
//! so a gruvbox user opening Preferences > Appearance saw a blue
//! swatch instead of the theme's actual accent (gold).
//!
//! The fix gives [`PrefsState`] an active `theme: Theme` field
//! (set at construction and via `set_theme`) and derives the
//! swatch's initial RGBA from the theme's accent source —
//! `theme.colors.tab.active_fg`, the same source
//! [`UiPalette::accent`] reads.

use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::{Category, ColorSwatch, Control, PrefsState};

fn hex(s: &str) -> Hex {
    Hex(s.to_string())
}

fn synth_theme(accent: &str) -> Theme {
    let ansi = AnsiColors {
        black: hex("#000000"),
        red: hex("#cc0000"),
        green: hex("#00cc00"),
        yellow: hex("#cccc00"),
        blue: hex("#0000cc"),
        magenta: hex("#cc00cc"),
        cyan: hex("#00cccc"),
        white: hex("#cccccc"),
    };
    Theme {
        name: "synth".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex("#1d2021"),
            foreground: hex("#ebdbb2"),
            cursor: hex("#ebdbb2"),
            cursor_text: hex("#1d2021"),
            selection_bg: hex("#3c3836"),
            selection_fg: hex("#ebdbb2"),
            ansi: ansi.clone(),
            bright: ansi,
            tab: TabColors {
                bar_bg: hex("#1d2021"),
                active_bg: hex("#3c3836"),
                active_fg: hex(accent),
                inactive_bg: hex("#1d2021"),
                inactive_fg: hex("#a89984"),
                hover_bg: hex("#3c3836"),
                hover_fg: hex("#d5c4a1"),
                close_button_fg: hex("#a89984"),
            },
        },
    }
}

fn find_accent_swatch(state: &PrefsState) -> ColorSwatch {
    for c in &state.controls {
        if let Control::ColorSwatch(sw) = c {
            if sw.label == "Accent" {
                return sw.clone();
            }
        }
    }
    panic!("Appearance category must contain an 'Accent' ColorSwatch");
}

#[test]
fn prefs_accent_swatch_uses_active_theme_accent() {
    // Gruvbox-style accent (#fabd2f) must show up as the Accent
    // swatch initial value, NOT Tokyo Night blue.
    let theme = synth_theme("#fabd2f");
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-prefs-test.toml"), theme);
    state.set_category(Category::Appearance);
    let sw = find_accent_swatch(&state);
    assert_eq!(
        sw.value,
        [0xfa, 0xbd, 0x2f, 0xff],
        "Accent swatch must follow the active theme's accent (gruvbox gold), \
         not the hardcoded Tokyo Night blue [0x7a, 0xa2, 0xf7, 0xff]"
    );

    // And it must NOT be the previously-hardcoded Tokyo Night blue.
    assert_ne!(
        sw.value,
        [0x7a, 0xa2, 0xf7, 0xff],
        "Accent swatch must not be hardcoded to Tokyo Night blue"
    );

    // Live theme switch via set_theme() must propagate to the swatch.
    let tn = synth_theme("#7aa2f7");
    state.set_theme(tn);
    let sw2 = find_accent_swatch(&state);
    assert_eq!(
        sw2.value,
        [0x7a, 0xa2, 0xf7, 0xff],
        "set_theme() must rebuild controls so the swatch tracks the new theme"
    );
}
