//! Phase B2 PR-B4 (#365) — final cleanup regressions.
//!
//! Pins:
//!   * `App.main_hidden` was folded into `WindowState.hidden`. The new
//!     accessor `App::main_is_hidden()` is the single read path; the
//!     legacy `__test_main_hidden` / `__test_set_main_hidden` shims route
//!     through it.
//!   * `App.focused_child` was deleted. The remaining "is a torn-out
//!     child OS-frontmost?" question is answered by
//!     `frontmost_kind() == FrontmostKind::Child(_)`, and the test-only
//!     `__test_set_focused_child` / `__test_focused_child` shims now
//!     drive / read `frontmost_window` so the existing regression tests
//!     (`tearout_newtab_routing`, `multi_window_frontmost_routing`,
//!     etc.) continue to pin the same behaviour.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use winit::window::WindowId;

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
fn theme() -> Theme {
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
fn make_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(theme(), Config::default(), keymap)
}

/// PR-B4: `main_hidden` was folded into `WindowState.hidden`. The
/// accessor must default to `true` before a main `WindowState` exists,
/// flip to `false` once a synthetic main is installed (matches the
/// `do_resumed` shape in production), and obey the test-only setter.
#[test]
fn main_hidden_round_trips_through_window_state() {
    let mut app = make_app();
    assert!(
        app.__test_main_hidden(),
        "before any main WindowState exists, main_is_hidden() must report true (operationally \
         indistinguishable from 'main has been hidden')"
    );
    // The `__test_set_main_hidden` helper installs a synthetic main as
    // a side-effect via the unified test seed; assert the field round
    // trips both ways through `WindowState.hidden`.
    app.__test_set_main_hidden(false);
    assert!(!app.__test_main_hidden(), "setter must clear the WindowState.hidden latch");
    app.__test_set_main_hidden(true);
    assert!(app.__test_main_hidden(), "setter must set the WindowState.hidden latch");
}

/// PR-B4: `focused_child` was deleted; the test-only shim now drives
/// `frontmost_window` instead. Stale ids (no matching `WindowState`)
/// must still classify as "no frontmost child" so callers fall back to
/// main — exactly the contract the old `focused_child` carried.
#[test]
fn focused_child_shim_drives_frontmost_window() {
    let mut app = make_app();
    // No frontmost recorded → child-shim read is None.
    app.__test_set_focused_child(None);
    assert_eq!(app.__test_focused_child(), None);
    // Stale id → frontmost_kind() classifies as None → child-shim
    // returns None (so NewTab falls back to main; see
    // `tearout_newtab_routing::new_tab_with_stale_focused_child_falls_back_to_main_and_clears_focus`).
    let stale = WindowId::dummy();
    app.__test_set_focused_child(Some(stale));
    assert_eq!(
        app.__test_focused_child(),
        None,
        "a stale frontmost id (no matching WindowState) must classify as no-frontmost-child, \
         matching the old focused_child stale-id semantics",
    );
}

/// PR-B4: `should_exit_pure` is unchanged (still takes `main_hidden:
/// bool`) — pin the truth table so a future refactor doesn't break it
/// silently.
#[test]
fn should_exit_pure_truth_table_unchanged() {
    // main has tabs + visible, no children → keep running.
    assert!(!App::should_exit_pure(1, false, 0));
    // main hidden, no children → exit.
    assert!(App::should_exit_pure(1, true, 0));
    // main drained, children alive → keep running.
    assert!(!App::should_exit_pure(0, false, 1));
    // everything gone → exit.
    assert!(App::should_exit_pure(0, false, 0));
    assert!(App::should_exit_pure(0, true, 0));
}
