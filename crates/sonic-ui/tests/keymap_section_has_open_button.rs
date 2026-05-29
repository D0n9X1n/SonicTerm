use std::path::PathBuf;

use sonic_cfg::{
    config::Config,
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_ui::prefs::{ButtonAction, Category, Control, PrefsState};

fn test_theme() -> Theme {
    let h = |s: &str| Hex(s.to_string());
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: h("#1d2021"),
            foreground: h("#ebdbb2"),
            cursor: h("#ebdbb2"),
            cursor_text: h("#1d2021"),
            selection_bg: h("#3c3836"),
            selection_fg: h("#ebdbb2"),
            ansi: AnsiColors {
                black: h("#000000"),
                red: h("#cc241d"),
                green: h("#98971a"),
                yellow: h("#d79921"),
                blue: h("#458588"),
                magenta: h("#b16286"),
                cyan: h("#689d6a"),
                white: h("#a89984"),
            },
            bright: AnsiColors {
                black: h("#928374"),
                red: h("#fb4934"),
                green: h("#b8bb26"),
                yellow: h("#fabd2f"),
                blue: h("#83a598"),
                magenta: h("#d3869b"),
                cyan: h("#8ec07c"),
                white: h("#ebdbb2"),
            },
            tab: TabColors {
                bar_bg: h("#1d2021"),
                active_bg: h("#3c3836"),
                active_fg: h("#fabd2f"),
                inactive_bg: h("#1d2021"),
                inactive_fg: h("#a89984"),
                hover_bg: h("#3c3836"),
                hover_fg: h("#d5c4a1"),
                close_button_fg: h("#a89984"),
            },
        },
    }
}

#[test]
fn keymap_section_has_open_button() {
    let mut state = PrefsState::new(Config::default(), PathBuf::from("sonic.toml"), test_theme());
    state.set_category(Category::Keymap);

    let button = state
        .controls
        .iter()
        .find_map(|control| match control {
            Control::Button(button) => Some(button),
            _ => None,
        })
        .expect("Keymap section should render a button");

    assert_eq!(button.label, "Open keymap file");
    assert_eq!(button.action, Some(ButtonAction::OpenKeymapFile));
}
