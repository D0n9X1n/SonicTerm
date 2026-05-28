use std::path::PathBuf;

use sonic_app::app::App;
use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_ui::prefs::{Control, PrefsState};

fn hex() -> Hex {
    Hex("#000000".to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    }
}

fn synth_theme() -> Theme {
    Theme {
        name: "test".to_string(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    }
}

fn empty_keymap() -> Keymap {
    Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() }
}

#[test]
fn prefs_toggle_hover_state_propagates_to_toggle_interaction() {
    let theme = synth_theme();
    let config = Config::default();
    let prefs = PrefsState::new(config.clone(), PathBuf::from("sonic-test.toml"), theme.clone());

    let (toggle_id, x, y) = prefs
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Toggle(t) => Some((t.id, t.rect.x + 1.0, t.rect.y + 1.0)),
            _ => None,
        })
        .expect("general prefs view includes a toggle");

    let mut app = App::new(theme, config, empty_keymap());
    app.install_prefs_state_for_test(prefs);

    assert!(app.set_toggle_hovered_for_test(x, y), "moving over toggle must change hover state");
    assert!(
        app.toggle_interaction_for_test(toggle_id).expect("toggle interaction present").hovered,
        "toggle InteractionState.hovered must be true after mouse-over"
    );
}
