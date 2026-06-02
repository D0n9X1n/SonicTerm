//! Regression: Ctrl/Cmd+W must close a tab on the first press.
//!
//! Pre-fix bug: on macOS, NSMenu intercepted the ⌘W chord and dispatched
//! `Action::CloseTab` through the menubar bridge (see `menubar_bridge.rs`)
//! rather than through the winit `KeyboardInput` arm in `window_event.rs`.
//! The KeyboardInput arm always paired `run_action` with
//! `window.request_redraw()`, but `drain_menubar_actions` did not — so
//! the close mutated `tab_states`/`tabs` correctly but the screen kept
//! showing the stale 3-tab bar until the next unrelated event (a second
//! ⌘W, a mouse move, or PTY output) triggered a repaint. Users
//! experienced this as "I have to press Ctrl+W twice to close a tab."
//!
//! This test exercises the action path the menubar takes — push a
//! `CloseTab` to the static action queue, drain it through
//! `App::run_action` (same code path `drain_menubar_actions` uses), and
//! assert the tab count drops by one after a single dispatch. The
//! companion change in `app/misc.rs::drain_menubar_actions` ensures
//! a `request_redraw` follows so the new state is visible the same tick.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
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

fn make_app() -> App {
    App::new(synth_theme(), Config::default(), empty_keymap())
}

#[test]
fn close_tab_action_drops_one_tab_per_dispatch() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3, "seeded three tabs");

    // Single dispatch — same code path the macOS menubar uses via
    // `drain_menubar_actions` (and the keymap-dispatch path via
    // `window_event` for non-macOS bindings).
    app.run_action(&Action::CloseTab);

    assert_eq!(
        app.__test_tab_count(),
        2,
        "one Action::CloseTab dispatch must remove exactly one tab \
         (regression: pre-fix users needed two presses on macOS)"
    );
}

#[test]
fn menubar_path_close_tab_drains_in_a_single_pass() {
    // Mirrors what NSMenu does on ⌘W: push the action onto the bridge,
    // then run every drained action exactly once. The pre-fix bug was
    // not in this loop — it was the missing `request_redraw` after —
    // but pinning the state-mutation invariant guards against any
    // future regression that batches or defers the close.
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);

    let _ = sonicterm_app::menubar_bridge::push_action(Action::CloseTab);
    for action in sonicterm_app::menubar_bridge::__test_drain() {
        app.run_action(&action);
    }

    assert_eq!(app.__test_tab_count(), 2, "menubar-bridge drain must close one tab per push");
}
