//! Phase B2 PR-B3b (#365) — render-pacing scalars (`last_render`,
//! `cursor_visible`, `hover_link`) live on `WindowState`, not `App`.
//!
//! Pins:
//! - `last_render` accessible via `self.main()`.
//! - `cursor_visible` Arc sharing semantics preserved (mutating through
//!   `main()?.cursor_visible.store(false)` is reflected in any held
//!   `Arc` clone — the VT thread captures one of these clones in
//!   `spawn_pane`).
//! - `hover_link` defaults to `false` in test mode.
//! - Multi-window: each `WindowState` owns its own scalars.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonic_app::app::{App, WindowRole, WindowState};
use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_ui::ime::ImeState;
use sonic_ui::tabs::TabBar;

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
    App::new(synth_theme(), Config::default(), keymap)
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
        modifiers: Default::default(),
        cursor_visible: Arc::new(AtomicBool::new(true)),
        last_render: Instant::now(),
        hover_link: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        scale_factor: 1.0,
        ime: ImeState::new(),
        hovered_url: None,
    }
}

#[test]
fn last_render_accessible_via_main() {
    let mut app = synth_app();
    let t = Instant::now() - Duration::from_millis(500);
    app.set_last_render_for_test(t);
    let got = app.main().expect("synthetic main installed").last_render;
    assert_eq!(got, t, "last_render write via set_last_render_for_test must land on main()");
}

#[test]
fn cursor_visible_arc_sharing_preserved() {
    let mut app = synth_app();
    // Force synthetic main creation.
    app.set_last_render_for_test(Instant::now());

    // Pull the Arc clone that the VT thread would capture in `spawn_pane`.
    let captured: Arc<AtomicBool> = app.main().expect("synthetic main").cursor_visible.clone();
    assert!(captured.load(Ordering::Relaxed), "default is visible");

    // Mutate via `main_mut()?.cursor_visible.store(false)`.
    app.main_mut().expect("synthetic main").cursor_visible.store(false, Ordering::Relaxed);

    // The clone we captured before the mutation MUST observe the new
    // value — that is the Arc-by-pointer sharing semantic the migration
    // promised to preserve.
    assert!(
        !captured.load(Ordering::Relaxed),
        "Arc clone captured by VT thread must observe store via main_mut()?.cursor_visible",
    );
}

#[test]
fn hover_link_defaults_false_in_test_mode() {
    let mut app = synth_app();
    app.set_last_render_for_test(Instant::now());
    let hl = app.main().expect("synthetic main").hover_link;
    assert!(!hl, "hover_link default must be false on a freshly-seeded synthetic main");
}

#[test]
fn multi_window_each_owns_its_render_pacing_scalars() {
    let mut a = make_synth_ws();
    let mut b = make_synth_ws();
    // Two distinct Arc allocations — Arc::ptr_eq must be false.
    assert!(
        !Arc::ptr_eq(&a.cursor_visible, &b.cursor_visible),
        "each WindowState owns its own cursor_visible Arc (per-window blink)",
    );

    let ta = Instant::now() - Duration::from_secs(10);
    let tb = Instant::now() - Duration::from_millis(1);
    a.last_render = ta;
    b.last_render = tb;
    assert_ne!(a.last_render, b.last_render, "last_render is per-window");

    a.hover_link = true;
    b.hover_link = false;
    assert!(a.hover_link);
    assert!(!b.hover_link, "hover_link is per-window");

    // Cursor blink on `a` must not flip `b`.
    a.cursor_visible.store(false, Ordering::Relaxed);
    assert!(!a.cursor_visible.load(Ordering::Relaxed));
    assert!(
        b.cursor_visible.load(Ordering::Relaxed),
        "b.cursor_visible must NOT see the store on a (no shared Arc)",
    );
}
