//! Epic #289 Phase A — menubar actions route via frontmost.
//!
//! macOS NSMenu intercepts Cmd+T / Cmd+W / Cmd+\\ before winit sees
//! them and dispatches the bound `Action` through the menubar bridge
//! (`menubar_bridge::push_action` → `drain_menubar_actions` →
//! `run_action`). Per CLAUDE.md §4 PR #200 the drain helper also
//! request_redraw's. Phase A adds the frontmost-window discriminator
//! INSIDE `run_action`, so the menubar path automatically inherits
//! the new routing — this test pins that contract by confirming a
//! menubar-pushed CloseTab still routes correctly when frontmost is
//! a stale/missing child id (must fall back to main, must clear the
//! stale id).
//!
//! End-to-end "frontmost == real child window → menubar CloseTab
//! shrinks child" coverage requires a real wgpu surface and is in the
//! manual GUI smoke step (CLAUDE.md §13 / PR body).

use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Action, Keymap, Meta},
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
fn make_app() -> App {
    App::new(
        synth_theme(),
        Config::default(),
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] },
    )
}

#[test]
fn menubar_close_tab_with_no_frontmost_lands_on_main() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_set_frontmost_window(None);

    let before = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);
    sonicterm_app::menubar_bridge::push_action(Action::CloseTab);
    app.__test_drain_menubar_actions();
    let after = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);

    assert_eq!(app.__test_main_tab_count(), 1, "menubar CloseTab with frontmost=None hits main");
    assert_eq!(after - before, 1, "drain must bump redraw counter exactly once");
}

#[test]
fn menubar_new_tab_with_no_frontmost_lands_on_main() {
    let mut app = make_app();
    let before_tabs = app.__test_main_tab_count();
    app.__test_set_frontmost_window(None);
    app.__test_set_focused_child(None);

    let before = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);
    sonicterm_app::menubar_bridge::push_action(Action::NewTab);
    app.__test_drain_menubar_actions();
    let after = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);

    assert_eq!(app.__test_main_tab_count(), before_tabs + 1);
    assert_eq!(after - before, 1, "drain must still bump redraw counter exactly once");
}

#[test]
fn menubar_close_tab_with_stale_frontmost_falls_back_and_clears() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_set_frontmost_window(Some(WindowId::dummy()));

    sonicterm_app::menubar_bridge::push_action(Action::CloseTab);
    app.__test_drain_menubar_actions();

    assert_eq!(app.__test_main_tab_count(), 1, "stale frontmost id falls back to main");
    assert_eq!(
        app.__test_frontmost_window(),
        None,
        "stale frontmost id is cleared so subsequent actions don't retry it",
    );
}

#[test]
fn menubar_close_active_pane_or_tab_routes_via_frontmost() {
    // Bug #3 in the menubar path: Cmd+W from the NSMenu used to mutate
    // the main window's state regardless of which window the user was
    // looking at. With Phase A, the dispatcher consults frontmost
    // first. We can't drive a live child here so we use the stale-id
    // fallback to confirm the routing checkpoint exists; the
    // routes-to-real-child case is covered by the GUI smoke.
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_split_active_right();
    assert_eq!(app.__test_pane_count_in_tab(0), Some(2));
    app.__test_set_frontmost_window(Some(WindowId::dummy()));

    sonicterm_app::menubar_bridge::push_action(Action::CloseActivePaneOrTab);
    app.__test_drain_menubar_actions();

    assert_eq!(app.__test_pane_count_in_tab(0), Some(1), "fell back to main and closed the pane");
    assert_eq!(
        app.__test_frontmost_window(),
        None,
        "stale id cleared so subsequent dispatches don't retry the dead window",
    );
}
