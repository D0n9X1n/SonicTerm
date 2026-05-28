//! Regression test for v0.6 user report:
//! "拖拽形成新的窗口后，再新的窗口按 ctrl+t 还是在原来的窗口打开新tab".
//!
//! After tearing a tab into a new child window, Cmd+T pressed in the
//! new window used to open a tab in the ORIGINAL (main) window. The
//! fix routes `Action::NewTab` through `App::focused_child`:
//!
//!   * `focused_child == None`            → tab lands in main (default)
//!   * `focused_child == Some(real_id)`   → tab lands in that child
//!   * `focused_child == Some(stale_id)`  → fallback to main + clear
//!
//! The "real_id" path needs a live wgpu surface and is exercised by the
//! manual GUI smoke test in CLAUDE.md §13. This test pins the routing
//! decision itself plus the fallback / stale-id behaviour, which were
//! the actual broken code path before the fix.

use sonic_app::app::App;
use sonic_core::{
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
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn new_tab_lands_in_main_when_no_child_is_focused() {
    let mut app = make_app();
    let before = app.__test_main_tab_count();
    app.__test_set_focused_child(None);
    app.run_action(&Action::NewTab);
    assert_eq!(
        app.__test_main_tab_count(),
        before + 1,
        "with no focused child, NewTab must add a tab to main",
    );
}

#[test]
fn new_tab_with_stale_focused_child_falls_back_to_main_and_clears_focus() {
    let mut app = make_app();
    let before = app.__test_main_tab_count();
    // Synthesise a `focused_child` id that points at no child window.
    // This is the "child was closed mid-dispatch" race the fix guards
    // against — without the fallback, the action would silently drop.
    let stale = WindowId::dummy();
    app.__test_set_focused_child(Some(stale));
    app.run_action(&Action::NewTab);
    assert_eq!(
        app.__test_main_tab_count(),
        before + 1,
        "stale focused_child must fall back to main, not drop the action",
    );
    assert_eq!(
        app.__test_focused_child(),
        None,
        "stale focused_child must be cleared on fallback so the next action does not retry it",
    );
}
