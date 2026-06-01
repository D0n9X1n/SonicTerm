//! Issue #438 regression guard: `App::cancel_drag_session()` must
//! wipe drag residue on EVERY window (main + every child), not just
//! the main one. Before the fix, the OS-drag end / cancel paths
//! (`handle_os_drag_ended` → `cancel_drag_session`) bypassed the
//! normal winit `MouseInput::Released` handlers that clear the
//! per-window renderer's `drag_chip` overlay, so a stale grey
//! rectangle was left floating in empty pane space after a tab
//! tear-out gesture ended without a "real" mouse-up event.
//!
//! These tests cover the App-observable half of the fix (per-window
//! `pressed_tab` / `mouse_down` / `drag_session` / `drag_target`
//! state). The renderer `drag_chip` clearing is also performed by the
//! production code path inside `cancel_drag_session`, but the headless
//! test seam constructs windows with `renderer: None`, so the
//! `if let Some(r) = ws.renderer.as_mut()` branch is a no-op here. The
//! cross-platform §13 GUI smoke (skipped for this cleanup-only PR per
//! the issue) would catch a renderer regression visually.

use sonicterm_app::app::App;
use sonicterm_core::{
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
    let mut app = App::new(synth_theme(), Config::default(), empty_keymap());
    app.__test_synthetic_main();
    app
}

#[test]
fn cancel_drag_session_clears_pressed_tab_and_mouse_down_on_child_windows() {
    // The pre-#438 implementation only wrote `pressed_tab = None`
    // / `mouse_down = false` through `main_mut()`, so a child window
    // that had recorded a tab press would carry the stale values
    // forward and the very next pointer event in that child would
    // believe a drag was still in flight.
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["c1", "c2"]);

    // Seed BOTH main and child with drag residue.
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(1), true, false));

    // Sanity: residue is observable before the cancel.
    assert_eq!(app.__test_pressed_tab(), Some(0));
    assert!(app.__test_mouse_down());
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(Some(1)));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(true));

    let _ = app.cancel_drag_session();

    // Main is cleared (pre-#438 behavior, kept).
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
    // Child is ALSO cleared (the #438 fix).
    assert_eq!(
        app.__test_child_pressed_tab(child_id),
        Some(None),
        "#438: cancel_drag_session must clear child pressed_tab"
    );
    assert_eq!(
        app.__test_child_mouse_down(child_id),
        Some(false),
        "#438: cancel_drag_session must clear child mouse_down"
    );
}

#[test]
fn cancel_drag_session_clears_drag_session_on_child_windows() {
    // Verifies the existing per-window drag_session / drag_target
    // sweep already in cancel_drag_session keeps working in the
    // multi-window seed, AND that the return value signals "had a
    // session" when a child (not main) was the carrier.
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["only"]);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(0), true, true));

    assert_eq!(app.__test_child_has_drag_session(child_id), Some(true));

    let had = app.cancel_drag_session();
    assert!(had, "cancel_drag_session must return true when a child carried a drag session");

    assert_eq!(
        app.__test_child_has_drag_session(child_id),
        Some(false),
        "#438: child drag_session must be cleared"
    );
    assert_eq!(
        app.__test_child_has_drag_target(child_id),
        Some(false),
        "#438: child drag_target must be cleared"
    );
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(None));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(false));
}

#[test]
fn cancel_drag_session_is_idempotent_across_all_windows() {
    // Calling cancel_drag_session twice in a row on a multi-window
    // App must not panic and must leave every window in the
    // already-cleared state.
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["x"]);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(0), true, true));
    let _ = app.cancel_drag_session();
    let had = app.cancel_drag_session();
    assert!(!had, "second cancel finds nothing to cancel");
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(None));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(false));
    assert_eq!(app.__test_child_has_drag_session(child_id), Some(false));
}
