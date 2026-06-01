//! Phase B2 PR-B3d (#365) — drag + IME cluster scalars
//! (`drag_session`, `drag_target`, `ime`, `ime_cursor_throttle`) live on
//! `WindowState`, not `App`.
//!
//! Pins:
//! - Each field is accessible per-window via `self.main()` / `main_mut()`.
//! - Multi-window: two synthetic `WindowState`s own independent drag /
//!   IME state, including IME composition isolation.
//! - A tab drag in one window does not touch another window's
//!   `drag_session`.

use std::collections::HashMap;
use std::time::Instant;

use sonicterm_app::app::{App, WindowRole, WindowState};
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonicterm_ui::ime::ImeState;
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
fn drag_session_accessible_via_main() {
    let mut app = synth_app();
    assert!(app.main().expect("main").drag_session.is_none(), "default drag_session = None");
    if let Some(ws) = app.main_mut() {
        ws.drag_session = Some(sonicterm_app::tab_drag::DragSession::new(2, (10.0, 20.0)));
    }
    let s = app.main().expect("main").drag_session;
    assert!(s.is_some(), "drag_session written via main_mut() must round-trip");
    assert_eq!(s.unwrap().press_tab_index, 2);
}

#[test]
fn drag_target_accessible_via_main() {
    let mut app = synth_app();
    assert!(app.main().expect("main").drag_target.is_none(), "default drag_target = None");
    // __test_set_drag_target writes through main_mut().
    let main_id = app.__test_main_window_id().expect("synthetic main id");
    app.__test_set_drag_target(Some(sonicterm_app::tab_drag::DropTarget {
        window: main_id,
        slot: 1,
    }));
    let t = app.main().expect("main").drag_target;
    assert!(t.is_some(), "__test_set_drag_target must land on main WindowState");
    assert_eq!(t.unwrap().slot, 1);
}

#[test]
fn ime_accessible_via_main() {
    let mut app = synth_app();
    assert!(!app.main().expect("main").ime.is_composing(), "default ime not composing");
    if let Some(ws) = app.main_mut() {
        ws.ime.handle_preedit("中", Some((0, 0)));
    }
    assert!(
        app.main().expect("main").ime.is_composing(),
        "preedit must flip ime to composing on main WindowState",
    );
}

#[test]
fn ime_cursor_throttle_accessible_via_main() {
    let mut app = synth_app();
    // First call should fire; subsequent identical cell coordinates should not.
    let first = app.main_mut().expect("main").ime_cursor_throttle.should_update(5, 7);
    let second = app.main_mut().expect("main").ime_cursor_throttle.should_update(5, 7);
    assert!(first, "first should_update at a new cell must fire");
    assert!(!second, "repeated should_update at the same cell must be throttled");
    app.main_mut().expect("main").ime_cursor_throttle.reset();
    let after_reset = app.main_mut().expect("main").ime_cursor_throttle.should_update(5, 7);
    assert!(after_reset, "reset() must re-arm the throttle");
}

#[test]
fn multi_window_owns_independent_drag_session() {
    let mut a = make_synth_ws();
    let mut b = make_synth_ws();
    a.drag_session = Some(sonicterm_app::tab_drag::DragSession::new(0, (1.0, 2.0)));
    assert!(a.drag_session.is_some());
    assert!(b.drag_session.is_none(), "drag_session is per-window — b unaffected by a");
    b.drag_session = Some(sonicterm_app::tab_drag::DragSession::new(7, (9.0, 9.0)));
    assert_eq!(a.drag_session.unwrap().press_tab_index, 0);
    assert_eq!(b.drag_session.unwrap().press_tab_index, 7);
}

#[test]
fn multi_window_owns_independent_drag_target() {
    let mut a = make_synth_ws();
    let b = make_synth_ws();
    let fake_id: winit::window::WindowId =
        unsafe { std::mem::transmute::<u64, winit::window::WindowId>(42) };
    a.drag_target = Some(sonicterm_app::tab_drag::DropTarget { window: fake_id, slot: 0 });
    assert!(a.drag_target.is_some());
    assert!(b.drag_target.is_none(), "drag_target is per-window — b unaffected by a");
}

#[test]
fn multi_window_owns_independent_ime() {
    let mut a = make_synth_ws();
    let mut b = make_synth_ws();
    a.ime.handle_preedit("日", Some((0, 0)));
    assert!(a.ime.is_composing(), "a composing");
    assert!(!b.ime.is_composing(), "b not composing — IME composition is per-window");
    b.ime.handle_preedit("本", Some((0, 0)));
    a.ime.cancel();
    assert!(!a.ime.is_composing(), "cancel on a does not touch b");
    assert!(b.ime.is_composing(), "b still composing after a was cancelled");
}

#[test]
fn multi_window_owns_independent_ime_cursor_throttle() {
    let mut a = make_synth_ws();
    let mut b = make_synth_ws();
    assert!(a.ime_cursor_throttle.should_update(1, 1));
    assert!(!a.ime_cursor_throttle.should_update(1, 1));
    // b throttle is untouched by activity on a.
    assert!(b.ime_cursor_throttle.should_update(1, 1));
}
