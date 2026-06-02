//! User-reported: with 3 tabs open, Cmd+W closes tabs down to 1, then
//! Cmd+W on the last tab "did nothing — neither closes the tab nor
//! quits the app." Spec: the Cmd+W chain on the main window must
//! drive the app to exit when the last tab closes AND
//! `quit_on_last_window_close == true` (the new default).
//!
//! This file exercises the keymap dispatcher (`run_action`) — the same
//! code path the keyboard and the macOS menubar bridge both take —
//! and pins the deferred-exit contract: after the third dispatch the
//! tabs vec is empty AND `pending_exit` is set. The event-loop drain
//! (`do_about_to_wait`) consumes the flag and calls `el.exit()`; that
//! drain requires a real `ActiveEventLoop` and is verified by the §13
//! GUI smoke instead of in a unit test.

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

#[test]
fn cmd_w_chain_closes_three_tabs_then_marks_app_for_exit() {
    // Default config: `quit_on_last_window_close = true` (traditional
    // terminal). 1 main window + 3 tabs + 0 children.
    let cfg = Config::default();
    assert!(
        cfg.quit_on_last_window_close,
        "default must be true; if this fails the flip in default_quit_on_last_window_close \
         was reverted and the rest of the spec no longer holds"
    );
    let mut app = App::new(synth_theme(), cfg, empty_keymap());
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);
    assert!(!app.__test_pending_exit(), "fresh app must not have pending_exit");

    // Cmd+W → close pane-or-tab. First two close tabs, app stays alive.
    app.run_action(&Action::CloseActivePaneOrTab);
    assert_eq!(app.__test_tab_count(), 2);
    assert!(!app.__test_pending_exit(), "2 tabs left → not pending exit");

    app.run_action(&Action::CloseActivePaneOrTab);
    assert_eq!(app.__test_tab_count(), 1);
    assert!(!app.__test_pending_exit(), "1 tab left → not pending exit");

    // Cmd+W on the last tab: tabs vec drains AND pending_exit is set.
    // `do_about_to_wait` calls `el.exit()` on the next event-loop turn.
    app.run_action(&Action::CloseActivePaneOrTab);
    assert_eq!(app.__test_tab_count(), 0, "last Cmd+W must close the final tab");
    assert!(
        app.__test_pending_exit(),
        "after final Cmd+W with quit_on_last_window_close=true the app MUST mark \
         itself for exit so do_about_to_wait can call el.exit()"
    );
}
