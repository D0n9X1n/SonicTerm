//! Phase B2 PR-B3c (#365) — input-cluster scalars (`selection`,
//! `copy_mode`, `modifiers`) live on `WindowState`, not `App`.
//!
//! Pins:
//! - selection/copy_mode/modifiers accessible via `self.main()` / `main_mut()`.
//! - The `App::selection_set` / `App::copy_mode_set` helpers route into
//!   the main window's `WindowState`.
//! - `App::main_modifiers()` reads from the main `WindowState`, falling
//!   back to `ModifiersState::empty()` when no main has been seeded yet.
//! - Multi-window: two synthetic `WindowState`s own independent
//!   selection / copy_mode / modifiers.

use std::collections::HashMap;
use std::time::Instant;

use sonicterm_app::app::{App, WindowRole, WindowState};
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabs::TabBar;
use winit::keyboard::ModifiersState;

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
fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), Config::default(), keymap);
    // Install the synthetic main entry so `main_mut()` / `main()` resolve.
    app.__test_synthetic_main();
    app
}

fn make_synth_ws() -> WindowState {
    WindowState {
        role: WindowRole::Terminal,
        window: None,
        renderer: None,
        tabs: TabBar::new(),
        tab_states: Vec::new(),
        panes: HashMap::new(),
        cursor_pos: (0.0, 0.0),
        mouse_down: false,
        selection: None,
        copy_mode: None,
        modifiers: ModifiersState::empty(),
        last_render: Instant::now(),
        hover_link: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        scale_factor: 1.0,
        ime: ImeState::new(),
        ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
        hovered_url: None,
        hidden: false,
        scrollbar_drag: None,
        scrollbar_vis: std::collections::HashMap::new(),
    }
}

#[test]
fn selection_owned_by_main_window_state() {
    let mut app = synth_app();
    assert!(
        app.main().expect("synthetic main").selection.is_none(),
        "default selection is None on a freshly-seeded main",
    );
    app.selection_set(Some(Selection::new(3, 4)));
    let sel = app.main().expect("main present").selection;
    assert!(sel.is_some(), "selection_set(Some(_)) must land on main WindowState");
    app.selection_set(None);
    assert!(
        app.main().expect("main present").selection.is_none(),
        "selection_set(None) must clear the main WindowState selection",
    );
}

#[test]
fn copy_mode_owned_by_main_window_state() {
    let mut app = synth_app();
    assert!(app.main().expect("synthetic main").copy_mode.is_none(), "default copy_mode is None",);
    let st = CopyModeState::new_at((0, 0));
    app.copy_mode_set(Some(st));
    assert!(
        app.main().expect("main present").copy_mode.is_some(),
        "copy_mode_set(Some(_)) must land on main WindowState",
    );
    app.copy_mode_set(None);
    assert!(
        app.main().expect("main present").copy_mode.is_none(),
        "copy_mode_set(None) must clear the main WindowState copy_mode",
    );
}

#[test]
fn modifiers_accessor_reads_from_main_window_state() {
    let mut app = synth_app();
    assert_eq!(
        app.main_modifiers(),
        ModifiersState::empty(),
        "default modifiers is empty on a freshly-seeded main",
    );
    if let Some(ws) = app.main_mut() {
        ws.modifiers = ModifiersState::SUPER;
    }
    assert_eq!(
        app.main_modifiers(),
        ModifiersState::SUPER,
        "main_modifiers() must reflect mutations to WindowState.modifiers",
    );
}

#[test]
fn main_modifiers_returns_empty_when_no_main() {
    // Construct an `App` without seeding a synthetic main entry.
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let app = App::new(synth_theme(), Config::default(), keymap);
    assert_eq!(
        app.main_modifiers(),
        ModifiersState::empty(),
        "main_modifiers() must default to empty when no main is installed",
    );
}

#[test]
fn multi_window_each_owns_its_input_cluster() {
    let mut a = make_synth_ws();
    let mut b = make_synth_ws();

    a.selection = Some(Selection::new(1, 1));
    b.selection = None;
    assert!(a.selection.is_some());
    assert!(b.selection.is_none(), "selection is per-window");

    a.copy_mode = Some(CopyModeState::new_at((0, 0)));
    b.copy_mode = None;
    assert!(a.copy_mode.is_some());
    assert!(b.copy_mode.is_none(), "copy_mode is per-window");

    a.modifiers = ModifiersState::SUPER;
    b.modifiers = ModifiersState::SHIFT;
    assert_eq!(a.modifiers, ModifiersState::SUPER);
    assert_eq!(b.modifiers, ModifiersState::SHIFT, "modifiers is per-window");
}

#[test]
fn shadow_snapshot_no_longer_carries_input_cluster() {
    // #404: ShadowMainSnapshot deleted entirely; the input-cluster
    // fields (selection/copy_mode/modifiers) live on WindowState and
    // the snapshot helper no longer exists. Stub retained for the
    // historical anchor name.
    let _ = synth_app();
}
