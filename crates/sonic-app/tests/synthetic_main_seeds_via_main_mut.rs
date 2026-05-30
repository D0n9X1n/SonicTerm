//! Phase B2 PR-B2a (#365): pins the test-only synthetic main
//! `WindowState` seam.
//!
//! Future B2b/c/d will delete the legacy `App.tabs / tab_states /
//! panes` fields outright. To stay future-proof, `__test_seed_tab`
//! and friends must route writes through `self.main_mut()` — which
//! requires a `main_window_id` to be set even though `do_resumed`
//! has never run in the test harness.
//!
//! This file pins:
//! 1. `App::new(...)` yields no main window (test default).
//! 2. `App::__test_synthetic_main()` inserts a synthetic main entry
//!    with `window=None`, `renderer=None`.
//! 3. `App::__test_seed_tab(...)` implicitly seeds the synthetic main
//!    and the new tab is observable via `app.main()?.tabs`.

use sonic_app::app::App;
use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
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

fn make_app() -> App {
    App::new(synth_theme(), Config::default(), empty_keymap())
}

#[test]
fn fresh_app_has_no_main_window_state() {
    let app = make_app();
    assert!(app.main().is_none(), "App::new() must not seed a main WindowState");
}

#[test]
fn explicit_synthetic_main_inserts_window_state_without_winit_window() {
    let mut app = make_app();
    app.__test_synthetic_main();
    let ws = app.main().expect("synthetic main entry inserted");
    assert!(ws.window.is_none(), "synthetic main must not carry an Arc<Window>");
    assert!(ws.renderer.is_none(), "synthetic main must not carry a GpuRenderer");
    assert_eq!(ws.tabs.len(), 0, "synthetic main starts with no tabs");
}

#[test]
fn synthetic_main_is_idempotent() {
    let mut app = make_app();
    app.__test_synthetic_main();
    let id1 = app.__test_main_window_id();
    app.__test_synthetic_main();
    let id2 = app.__test_main_window_id();
    assert_eq!(id1, id2, "second call must not change main_window_id");
}

#[test]
fn seed_tab_routes_through_main_mut() {
    let mut app = make_app();
    let _pane = app.__test_seed_tab("alpha");
    let ws = app.main().expect("seed_tab implicitly seeds synthetic main");
    assert_eq!(ws.tabs.len(), 1, "tab landed in WindowState.tabs (future-proof route)");
    assert_eq!(ws.tab_states.len(), 1, "tab_state landed in WindowState.tab_states");
    assert_eq!(ws.panes.len(), 1, "pane landed in WindowState.panes");
}
