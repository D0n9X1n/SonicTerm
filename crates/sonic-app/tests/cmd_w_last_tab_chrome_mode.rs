//! Regression: Cmd+W on the last tab with the opt-in
//! `quit_on_last_window_close = false` (Chrome/Firefox-style on
//! macOS) must NOT request app exit — instead the main window is
//! hidden and the process stays alive in the dock.
//!
//! Pairs with `cmd_w_last_tab_main_quits.rs` which pins the default
//! (true → exit) side. Together they pin the configurability of the
//! behavior. The actual el.exit-vs-hide decision lives in
//! `App::drain_pending_exit`; here we verify the gating predicate
//! `should_exit_on_last_window_close` matches the user's intent and
//! that the keymap dispatcher still marks `pending_exit` regardless
//! of the config (the config gates what the drain does with the flag,
//! not whether the flag is set).

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

fn chrome_mode_app_with_one_tab() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let cfg = Config { quit_on_last_window_close: false, ..Config::default() };
    let mut app = App::new(synth_theme(), cfg, keymap);
    app.__test_seed_tab("tab-1");
    app
}

#[cfg(target_os = "macos")]
#[test]
fn cmd_w_on_last_tab_chrome_mode_does_not_request_exit() {
    let mut app = chrome_mode_app_with_one_tab();
    assert!(
        !app.config_for_test().quit_on_last_window_close,
        "precondition: opt-in chrome mode (false)",
    );
    assert_eq!(app.__test_main_tab_count(), 1);
    assert!(!app.__test_pending_exit());

    app.run_action(&Action::CloseActivePaneOrTab);

    // Tab is closed AND pending_exit is set — same as the default
    // case. The config gates what the drain does, not whether the
    // dispatcher sets the flag.
    assert_eq!(app.__test_main_tab_count(), 0, "the tab MUST be closed");
    assert!(
        app.__test_pending_exit(),
        "the keymap dispatcher MUST set pending_exit regardless of \
         config; the drain helper consults the config to decide \
         exit-vs-hide",
    );

    // Critical: the predicate that drain_pending_exit consults MUST
    // return false in this mode → drain will call hide_main_window
    // instead of el.exit(). Process stays alive in the dock.
    assert!(
        !App::should_exit_on_last_window_close(app.config_for_test()),
        "macOS + quit_on_last_window_close=false → predicate MUST \
         return false so drain_pending_exit hides the window \
         instead of exiting the event loop (Chrome-style)",
    );
}

#[cfg(target_os = "macos")]
#[test]
fn chrome_mode_explicit_opt_in_takes_effect() {
    // Belt-and-suspenders coverage at the predicate seam.
    let cfg = Config { quit_on_last_window_close: false, ..Config::default() };
    assert!(
        !App::should_exit_on_last_window_close(&cfg),
        "explicit opt-in to chrome mode MUST propagate through the \
         exit-decision predicate",
    );
}
