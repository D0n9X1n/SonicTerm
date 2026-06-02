//! Issue #539 regression suite — source-aware action dispatch.
//!
//! The bug: when a Ctrl+T fires in window A but `App::frontmost_window`
//! was set to B (e.g. focus event for B was scheduled but A's
//! KeyboardInput was processed first, or any other race in the
//! frontmost-tracking pipeline), the chord opens a new tab in B.
//!
//! The fix: keyboard call sites pass the `WindowId` from the
//! `WindowEvent::KeyboardInput` event into the new
//! [`App::run_action_for_window`] helper, which classifies the source
//! id (NOT the cached frontmost). Source-less callers (menubar,
//! palette execution, overlay dismissal) keep using `run_action`,
//! which still routes via the cached frontmost.
//!
//! ## Coverage gap (same as `multi_window_frontmost_routing.rs`)
//!
//! End-to-end "two real child windows, A and B, type Ctrl+T in A,
//! observe tab in A" requires a live winit event loop AND a wgpu
//! surface — both impossible inside `cargo test`. The tests here
//! exercise the routing CONDITION at the helper-API boundary:
//! `run_action_for_window` derives its routing decision from the
//! `source_window_id` parameter, not from `App::frontmost_window`.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap, Meta};
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

fn make_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

/// The core bug repro: `frontmost_window` is set to a stale/foreign id
/// (simulating "B is in the cache because of a race"), but the keyboard
/// chord fires in the actual main window (source = main_window_id).
///
/// `run_action_for_window` must classify by `source_window_id`, NOT by
/// the stale cached frontmost — so the new tab lands in main.
///
/// With the OLD `run_action(&Action::NewTab)` call site, the cached
/// `frontmost_window` was consulted; if it pointed to a live child the
/// new tab would land THERE instead of in the key-source window. This
/// test pins down the new contract.
#[test]
fn new_tab_routes_to_source_window_ignoring_stale_frontmost() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    let main_id = app.__test_main_window_id().expect("__test_seed_tab seeds synthetic main");
    let before = app.__test_main_tab_count();

    // Simulate the race: frontmost is set to a different (stale) id
    // — this is what `run_action(&NewTab)` would observe and would
    // attempt to route to. The source-aware helper must IGNORE this.
    app.__test_set_frontmost_window(Some(WindowId::dummy()));

    // The chord fires in MAIN window — pass main_id as source.
    let handled = app.run_action_for_window(&Action::NewTab, main_id);
    assert!(handled, "action must be reported handled");

    assert_eq!(
        app.__test_main_tab_count(),
        before + 1,
        "Ctrl+T sourced from main must add a tab to MAIN, regardless of stale frontmost cache",
    );
}

#[test]
fn close_tab_routes_to_source_window_ignoring_stale_frontmost() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    let main_id = app.__test_main_window_id().expect("main id");
    let before = app.__test_main_tab_count();
    assert_eq!(before, 2);

    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    app.run_action_for_window(&Action::CloseTab, main_id);

    assert_eq!(
        app.__test_main_tab_count(),
        before - 1,
        "Ctrl+W sourced from main must close in MAIN, not retry stale frontmost",
    );
}

/// When the source id is unknown (neither main nor any live child),
/// classification falls through to `FrontmostKind::None` and the
/// dispatcher uses the main-window fallback path — same safe default
/// as `run_action` would have produced with no frontmost set.
#[test]
fn unknown_source_id_falls_back_to_main() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    let before = app.__test_main_tab_count();

    // A bogus source id (no live window) MUST NOT panic and MUST
    // still apply the action somewhere observable — main is the safe
    // default.
    let handled = app.run_action_for_window(&Action::NewTab, WindowId::dummy());
    assert!(handled);
    assert_eq!(
        app.__test_main_tab_count(),
        before + 1,
        "unknown source id → fall back to main, action still takes effect",
    );
}

/// Non-routed arms (clipboard, theme, etc.) delegate to `run_action`.
/// `NewWindow` belongs in that bucket — it correctly creates a new
/// top-level window regardless of source. Just exercise the helper
/// with one such variant to lock in the delegation path.
#[test]
fn non_routed_action_delegates_to_run_action() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    let before = app.__test_main_tab_count();
    let main_id = app.__test_main_window_id().expect("main id");
    // `ReloadConfig` is a no-op on the test config (no on-disk file),
    // but it must NOT panic and must NOT mutate tab state.
    let handled = app.run_action_for_window(&Action::ReloadConfig, main_id);
    assert!(handled);
    assert_eq!(app.__test_main_tab_count(), before);
}
