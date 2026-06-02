//! Epic #289 Phase E — Haiku follow-up on PR #297, companion to
//! `new_window_from_empty_state.rs`.
//!
//! Pins the OTHER half of the contract: dispatching
//! `Action::NewWindow` while at least one terminal window already
//! exists must ALSO set `pending_new_window` (i.e. the fix is not
//! conditional on the windows-empty branch). This is the normal
//! Cmd+N case from a focused live window.

use sonicterm_app::app::App;
use sonicterm_cfg::{
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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn new_window_when_main_has_tabs_sets_pending_flag() {
    // Simulate "main window has a tab, user presses Cmd+N". The
    // existing `__test_seed_tab` helper installs a tab in the main
    // App's tab bar so this is the realistic "live session" state.
    let mut app = synth_app();
    app.__test_seed_tab("alpha");
    let main_tabs_before = app.__test_main_tab_count();
    let windows_before = app.__test_windows_len();
    assert!(!app.__test_pending_new_window(), "precondition: pending_new_window starts false",);

    app.run_action(&Action::NewWindow);

    assert!(
        app.__test_pending_new_window(),
        "Action::NewWindow with an existing main tab MUST set \
         pending_new_window — the fix is unconditional, not gated on \
         the windows-empty branch",
    );
    // NewWindow must not steal a tab from main, and must not (yet)
    // mutate the windows map — that happens in the event-loop drain.
    assert_eq!(app.__test_main_tab_count(), main_tabs_before, "main tabs unchanged");
    assert_eq!(
        app.__test_windows_len(),
        windows_before,
        "windows map not mutated until drain runs with a live event loop"
    );
}
