//! Issue #370 — child-window keyboard handler must run full keymap
//! dispatch, not the narrow EnterCopyMode/EnterQuickSelect special-case
//! that previously leaked NextTab / PrevTab / ActivateTab into PTY bytes.
//!
//! Pre-fix, `crates/sonic-app/src/app/child_window.rs` ~587–602 only
//! matched two action variants; every other binding (Cmd+1, Cmd+2,
//! Cmd+Right, Cmd+Left, Cmd+T, Cmd+W, SplitRight, …) fell into the
//! PTY-byte path. Cmd+T appeared to work only because the macOS menubar
//! bypassed this handler entirely. The fix mirrors the main-window
//! handler in `window_event.rs` ~916: try keymap lookup → run_action
//! first, fall through to PTY only when no binding matches.
//!
//! ## What this test covers
//!
//! 1. The keymap can resolve `cmd+1`, `cmd+2`, `cmd+RightArrow`,
//!    `cmd+LeftArrow` to `ActivateTab(0)`, `ActivateTab(1)`, `NextTab`,
//!    `PrevTab` (the chord → action pipeline the handler depends on).
//! 2. `App::run_action` with those actions, when `frontmost_window` is a
//!    Child(_) id, routes through the per-child mutators rather than
//!    mutating `self.tabs` directly — i.e. the dispatcher arms exist and
//!    the fallback path is reachable. This is the SAME contract pinned
//!    down in `multi_window_frontmost_routing.rs` for the previously-
//!    fixed CloseTab / NewTab cases; we're extending the assertion to
//!    the tab-navigation actions that #370 reported broken.
//!
//! ## Why no real-child end-to-end here
//!
//! A live child `WindowState` requires a winit event loop + wgpu surface;
//! both are unavailable inside `cargo test`. That gap is covered by the
//! CLAUDE.md §13 manual GUI smoke step listed in the PR body
//! ("Cmd+N new window, Cmd+T 3 tabs, Cmd+1 / Cmd+2 / Cmd+→ / Cmd+← all
//! work"). This file pins down everything testable below that line.
//!
//! ## Test floor
//!
//! Adds 5 tests; bumps the floor by +5.

use sonic_app::app::App;
use sonic_core::{
    config::Config,
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
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

fn keymap_with_tab_actions() -> Keymap {
    Keymap {
        meta: Meta { name: "test".into(), version: "0".into() },
        bindings: vec![
            Binding { keys: "cmd+1".into(), action: ActionWrapper(Action::ActivateTab(0)) },
            Binding { keys: "cmd+2".into(), action: ActionWrapper(Action::ActivateTab(1)) },
            Binding { keys: "cmd+RightArrow".into(), action: ActionWrapper(Action::NextTab) },
            Binding { keys: "cmd+LeftArrow".into(), action: ActionWrapper(Action::PrevTab) },
        ],
    }
}

fn make_app() -> App {
    App::new(synth_theme(), Config::default(), keymap_with_tab_actions())
}

// ─── (1) Keymap resolves the chords the child handler now dispatches ──

#[test]
fn keymap_resolves_cmd_1_to_activate_tab_0() {
    let app = make_app();
    let action = app.__test_keymap_lookup("cmd+1");
    assert_eq!(action, Some(Action::ActivateTab(0)), "child handler depends on this resolution",);
}

#[test]
fn keymap_resolves_cmd_2_to_activate_tab_1() {
    let app = make_app();
    assert_eq!(app.__test_keymap_lookup("cmd+2"), Some(Action::ActivateTab(1)));
}

#[test]
fn keymap_resolves_cmd_right_to_next_tab() {
    let app = make_app();
    assert_eq!(app.__test_keymap_lookup("cmd+RightArrow"), Some(Action::NextTab));
}

#[test]
fn keymap_resolves_cmd_left_to_prev_tab() {
    let app = make_app();
    assert_eq!(app.__test_keymap_lookup("cmd+LeftArrow"), Some(Action::PrevTab));
}

// ─── (2) Routing: with a child as frontmost, run_action goes through
//          the per-child mutator path (stale child id → safe fallback to
//          main, no silently-dropped action). Mirrors the existing
//          `frontmost_child_routes_close_tab_away_from_main` pattern in
//          multi_window_frontmost_routing.rs, extended to the tab-nav
//          actions reported in #370.

#[test]
fn tab_nav_actions_route_through_frontmost_and_fall_back_safely() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    let before = app.__test_main_tab_count();

    // Stale child id → per-child mutator returns false → dispatcher
    // clears the stale id AND still runs the action on main (the same
    // contract enforced for CloseTab in the existing suite).
    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    app.run_action(&Action::NextTab);
    assert_eq!(
        app.__test_frontmost_window(),
        None,
        "stale frontmost must be cleared on NextTab fallback (issue #370)",
    );
    assert_eq!(
        app.__test_main_tab_count(),
        before,
        "NextTab is a presentation change — tab count must not change",
    );

    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    app.run_action(&Action::PrevTab);
    assert_eq!(app.__test_frontmost_window(), None, "stale frontmost cleared on PrevTab fallback");

    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    app.run_action(&Action::ActivateTab(0));
    assert_eq!(
        app.__test_frontmost_window(),
        None,
        "stale frontmost cleared on ActivateTab fallback",
    );
}
