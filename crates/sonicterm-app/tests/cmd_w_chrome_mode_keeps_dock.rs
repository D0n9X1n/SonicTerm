//! Chrome-style opt-out: with `quit_on_last_window_close = false`, the
//! Cmd+W chain still drains the main window's tabs to zero, but the
//! app stays alive (dock icon visible on macOS, ready for Cmd+N).
//! `pending_exit` MUST NOT be set in this mode — that's the whole
//! point of the opt-out.
//!
//! On non-macOS hosts `should_exit_on_last_window_close` ignores the
//! config and always returns true; the test is therefore gated on
//! macOS where the Chrome mode is meaningful.

#![cfg(target_os = "macos")]

use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Action, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

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
fn chrome_mode_cmd_w_chain_drains_tabs_but_keeps_app_alive() {
    let cfg = Config { quit_on_last_window_close: false, ..Config::default() };
    let mut app = App::new(synth_theme(), cfg, empty_keymap());
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);

    app.run_action(&Action::CloseActivePaneOrTab);
    app.run_action(&Action::CloseActivePaneOrTab);
    app.run_action(&Action::CloseActivePaneOrTab);

    assert_eq!(app.__test_tab_count(), 0, "all three tabs must be drained");
    assert!(
        !app.__test_pending_exit(),
        "Chrome mode (quit_on_last_window_close=false) MUST NOT set pending_exit \
         — the dock-alive opt-out is the whole point of the flag"
    );
    assert!(app.__test_main_hidden(), "Chrome mode hides the empty main window instead of exiting");
}
