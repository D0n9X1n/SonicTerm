//! Phase B2 PR-B3 (#365): scalar ownership migrated from `App.*` onto
//! `WindowState`. Pins:
//!   1. Default scalars on the seeded main `WindowState`.
//!   2. Mutating through `main_mut()` is observable through `main()`
//!      and through the existing `__test_mouse_down` / `__test_pressed_tab`
//!      seams.
//!   3. Multi-window isolation: each `WindowState` has its own copy.
//!
//! The "deleted-App-field" invariant is enforced at compile time —
//! every read/write below goes through `app.main()` / `app.main_mut()`
//! / `app.__test_*`; if the App field crept back it would shadow these
//! and the multi-window isolation test would fail.

use sonicterm_app::app::App;
use sonicterm_cfg::{
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
fn main_window_has_default_scalars() {
    let app = make_app();
    let ws = app.main().expect("main WindowState present");
    assert_eq!(ws.cursor_pos, (0.0, 0.0));
    assert!(!ws.mouse_down);
    assert_eq!(ws.pressed_tab, None);
}

#[test]
fn mutate_through_main_mut_is_observable() {
    let mut app = make_app();
    {
        let ws = app.main_mut().expect("main");
        ws.cursor_pos = (42.0, 17.0);
        ws.mouse_down = true;
        ws.pressed_tab = Some(3);
    }
    let ws = app.main().expect("main");
    assert_eq!(ws.cursor_pos, (42.0, 17.0));
    assert!(ws.mouse_down);
    assert_eq!(ws.pressed_tab, Some(3));
}

#[test]
fn test_setters_routed_through_window_state() {
    let mut app = make_app();
    app.__test_set_mouse_down(true);
    app.__test_set_pressed_tab(Some(7));
    assert!(app.__test_mouse_down());
    assert_eq!(app.__test_pressed_tab(), Some(7));
    // And these MUST be reflected on the main WindowState.
    let ws = app.main().expect("main");
    assert!(ws.mouse_down);
    assert_eq!(ws.pressed_tab, Some(7));
}

#[test]
fn child_windows_carry_independent_scalars() {
    let mut app = make_app();
    {
        let ws = app.main_mut().expect("main");
        ws.cursor_pos = (10.0, 20.0);
        ws.mouse_down = true;
        ws.pressed_tab = Some(1);
    }
    // Synthesize a child window — it gets its own WindowState (default
    // scalars) and the main is untouched.
    let child_id = app.__test_seed_child_window(&["one"]);
    // We don't expose `windows` directly outside the crate; assert via
    // the public child accessors and the existing test seams.
    assert_eq!(app.__test_child_pane_count(child_id), Some(1));
    // Main's scalars survive the child seed.
    let main_ws = app.main().expect("main");
    assert_eq!(main_ws.cursor_pos, (10.0, 20.0));
    assert!(main_ws.mouse_down);
    assert_eq!(main_ws.pressed_tab, Some(1));
}

#[test]
fn shadow_main_in_sync_after_b3() {
    // #404: shadow-sync infrastructure deleted; this test is now a
    // no-op stub kept only to preserve the historical anchor name.
    // The underlying invariant (scale_factor/hovered_url live on
    // WindowState) is enforced statically by the field absence on App,
    // and dynamically by `shadow_main_snapshot_deleted.rs`.
    let _ = make_app();
}

#[test]
fn cancel_drag_session_clears_main_window_state_fields() {
    let mut app = make_app();
    app.__test_set_pressed_tab(Some(2));
    app.__test_set_mouse_down(true);
    assert_eq!(app.__test_pressed_tab(), Some(2));
    assert!(app.__test_mouse_down());
    let _ = app.cancel_drag_session();
    // PR-B3: cancel_drag_session now writes through main_mut().
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
}
