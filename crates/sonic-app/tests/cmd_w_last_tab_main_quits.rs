//! Regression: Cmd+W on the last tab of the last (main) window with the
//! default config must request app exit.
//!
//! Bug as reported by the user: on macOS with a single window + single
//! tab, pressing Cmd+W did nothing visible. Pre-fix code paths:
//!   1. `Action::CloseActivePaneOrTab` ran `close_tab_at(i)`.
//!   2. The keymap dispatcher did NOT check `tabs.is_empty()` afterwards
//!      (only the tabbar-mouse-close path did), so neither
//!      `el.exit()` nor `hide_main_window()` was ever called.
//!   3. The config default for `quit_on_last_window_close` was `false`
//!      (Chrome-style), so even if the check had been wired up the
//!      app would have stayed in the dock.
//!
//! Fix: (a) flip the default to `true` (traditional terminal behavior,
//! matching Terminal.app / iTerm2 / Alacritty / WezTerm); (b) after
//! `close_tab_at` empties the main window's tab list AND no torn-out
//! child windows are alive, set `pending_exit`. The next event-loop
//! tick consumes the flag via `drain_pending_exit(el)` which calls
//! `el.exit()` (when `should_exit_on_last_window_close(&config)`).
//!
//! We can't construct a real `ActiveEventLoop` in a unit test, so we
//! assert against the `pending_exit` flag and the predicate
//! `should_exit_on_last_window_close` — together they fully cover the
//! seam (`drain_pending_exit` is a 3-line forwarder).

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

fn synth_app_with_one_tab(config: Config) -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), config, keymap);
    app.__test_seed_tab("tab-1");
    app
}

#[test]
fn cmd_w_on_last_tab_with_default_config_marks_pending_exit() {
    // Default config: quit_on_last_window_close = true (traditional
    // terminal behavior). 1 main window (no child windows) + 1 tab.
    let mut app = synth_app_with_one_tab(Config::default());
    assert_eq!(app.__test_main_tab_count(), 1, "precondition: one tab");
    assert!(!app.__test_pending_exit(), "precondition: pending_exit starts clear");

    // Dispatch Cmd+W → CloseActivePaneOrTab on the last (and only) tab.
    app.run_action(&Action::CloseActivePaneOrTab);

    assert_eq!(app.__test_main_tab_count(), 0, "the tab MUST be closed");
    assert!(
        app.__test_pending_exit(),
        "Cmd+W on the last tab with no child windows MUST set \
         pending_exit so drain_pending_exit(el) calls el.exit() on \
         the next event-loop tick (user bug: pre-fix this was a no-op)",
    );
}

#[cfg(target_os = "macos")]
#[test]
fn default_config_resolves_to_exit_on_last_window_close() {
    // The Cmd+W → pending_exit flow combined with the
    // `should_exit_on_last_window_close` predicate is what produces
    // the visible "app quits" behavior. Pin the predicate at the
    // default-config level so a future change to either side
    // (default flip or predicate semantics) regresses loudly.
    let cfg = Config::default();
    assert!(cfg.quit_on_last_window_close, "default must be true (traditional terminal)");
    assert!(
        App::should_exit_on_last_window_close(&cfg),
        "macOS + default → MUST exit on last window close",
    );
}
