use std::time::{Duration, Instant};

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonicterm_ui::tabs::CommandStatus;
use sonicterm_vt::vt::CommandEvent;

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
        name: "test".into(),
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
fn command_events_are_polled_for_background_tab() {
    let mut app = App::new(synth_theme(), Config::default(), empty_keymap());
    app.__test_seed_tab("active");
    let background_pane = app.__test_seed_tab("background");
    app.run_action(&sonicterm_cfg::keymap::Action::PrevTab);

    let started = Instant::now() - Duration::from_secs(6);
    app.__test_push_pane_command_event(background_pane, CommandEvent::CmdStart, started, None);
    app.poll_command_events_for_all_tabs();

    assert_eq!(app.__test_command_status_for_tab(1), Some(CommandStatus::Running(started)));
    assert_eq!(app.__test_tab_badge(1, Instant::now()), Some("…"));
}
