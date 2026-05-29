//! Epic #289 Phase E — Haiku follow-up on PR #297.
//!
//! Bug: on macOS with `quit_on_last_window_close = false`, after the
//! last terminal window closes the process stays alive (dock icon +
//! native menubar), but `Action::NewWindow` (Cmd+N) was an unwired
//! no-op → the user had no way to spawn a new window and was stuck
//! with a dead-feeling app.
//!
//! Fix: dispatching `Action::NewWindow` now sets the
//! `pending_new_window` flag, which the existing
//! `drain_pending_window_creates(el)` helper consumes by calling
//! `create_new_terminal_window(el)`. This path must work whether or
//! not `self.windows` is empty.
//!
//! This regression test pins the EMPTY-WINDOWS case: starting from a
//! state where the app has zero terminal windows (the post-close-
//! last-window dock-alive case), `Action::NewWindow` must mark the
//! pending flag so the next event-loop tick spawns a window. We can't
//! construct a real `ActiveEventLoop` in a unit test, so we assert
//! against the pending flag — the same testable seam that the prefs
//! window uses (`__test_menubar_dispatch_open_preferences_sets_pending`).

use sonic_app::app::App;
use sonic_core::{
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
fn new_window_from_empty_state_sets_pending_flag() {
    // Simulate the post-close-last-window dock-alive case: the App
    // has no terminal windows (neither main nor child). On macOS
    // with quit_on_last_window_close=false this is a perfectly
    // valid live state.
    let mut app = synth_app();
    assert_eq!(
        app.__test_windows_len(),
        0,
        "precondition: synth_app has no windows in the unified windows map",
    );
    assert!(!app.__test_pending_new_window(), "precondition: pending_new_window starts false",);

    // Dispatch the action. Before the fix this was a tracing::info!
    // no-op; after the fix it sets the pending flag so the next
    // event-loop tick materializes a real window.
    app.run_action(&Action::NewWindow);

    assert!(
        app.__test_pending_new_window(),
        "Action::NewWindow from the empty-windows state MUST set \
         pending_new_window so drain_pending_window_creates spawns a \
         fresh terminal window (Haiku finding on PR #297)",
    );
}
