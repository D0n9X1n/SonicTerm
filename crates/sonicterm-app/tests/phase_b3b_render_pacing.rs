//! Phase B2 PR-B3b (#365) — render-pacing scalars (`last_render`,
//! `hover_link`) live on `WindowState`, not `App`.
//!
//! PR #400 follow-up: `cursor_visible` moved off `WindowState` and onto
//! `PaneState` (per-pane Arc, travels with tear-out). The Arc-sharing
//! and per-window/per-pane assertions below are updated accordingly.
//!
//! Pins:
//! - `last_render` accessible via `self.main()`.
//! - `cursor_visible` Arc sharing semantics preserved on PaneState
//!   (mutating through a held clone is observed by the same pane).
//! - `hover_link` defaults to `false` in test mode.
//! - Multi-window/multi-pane: each PaneState owns its own
//!   `cursor_visible` Arc.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sonicterm_app::app::{App, PaneState, WindowRole, WindowState};
use sonicterm_cfg::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::tabs::TabBar;
use sonicterm_vt::vt::Parser;

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
        test_drag_chip_marker: None,
    }
}

fn make_synth_pane() -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    PaneState::new(parser, None)
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
fn cursor_visible_arc_sharing_preserved_on_pane() {
    // PR #400: cursor_visible now lives on PaneState. A clone of the
    // pane's Arc (what the VT thread captures in spawn_pane) MUST
    // observe stores to the same pane's Arc.
    let pane = make_synth_pane();
    let captured: Arc<AtomicBool> = pane.cursor_visible.clone();
    assert!(captured.load(Ordering::Relaxed), "default is visible");
    pane.cursor_visible.store(false, Ordering::Relaxed);
    assert!(
        !captured.load(Ordering::Relaxed),
        "Arc clone captured by VT thread must observe store on the same PaneState",
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

    let ta = Instant::now() - Duration::from_secs(10);
    let tb = Instant::now() - Duration::from_millis(1);
    a.last_render = ta;
    b.last_render = tb;
    assert_ne!(a.last_render, b.last_render, "last_render is per-window");

    a.hover_link = true;
    b.hover_link = false;
    assert!(a.hover_link);
    assert!(!b.hover_link, "hover_link is per-window");
}

#[test]
fn multi_pane_each_owns_its_cursor_visible() {
    // PR #400: cursor_visible is per-pane (lives on PaneState, not
    // WindowState). Two panes must own distinct Arc allocations so the
    // DECTCEM flag tracks each pane independently — and so the Arc
    // travels with the pane when a tab is torn out into a new window.
    let p1 = make_synth_pane();
    let p2 = make_synth_pane();
    assert!(
        !Arc::ptr_eq(&p1.cursor_visible, &p2.cursor_visible),
        "each PaneState owns its own cursor_visible Arc",
    );
    p1.cursor_visible.store(false, Ordering::Relaxed);
    assert!(!p1.cursor_visible.load(Ordering::Relaxed));
    assert!(
        p2.cursor_visible.load(Ordering::Relaxed),
        "p2.cursor_visible must NOT see the store on p1 (no shared Arc)",
    );
}
