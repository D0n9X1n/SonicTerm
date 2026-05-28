//! Regression test for v0.6 user report:
//! "split window 这个功能是坏的，没有能够形成两个可以输入的 windows".
//!
//! Two complaints under one bug:
//!
//! 1. Splitting a pane spawned the second pane state but the user had
//!    no way to focus it without an undocumented keyboard shortcut. The
//!    fix wires click-to-focus into the main window's left-mouse-press
//!    handler so any click inside an inactive pane's rect activates it.
//!    This test pins the underlying state transition (active_pane id
//!    flips when set via the test hook) and the structural invariant
//!    (splitting grows the panes map and the tab's tree to 2 leaves).
//!
//! 2. The full per-pane grid render is v0.4 work tracked separately;
//!    inactive panes still show only their border + cursor outline.
//!    Click-to-focus on its own restores user-visible interactivity by
//!    letting the user swap which pane is the rendered/typeable one,
//!    matching the manual GUI smoke in CLAUDE.md §13.

use sonic_app::app::App;
use sonic_core::{
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

fn make_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn split_active_grows_panes_map_and_flips_focus_to_new_leaf() {
    let mut app = make_app();
    let original = app.__test_seed_tab("first");
    let before_panes = app.__test_pane_ids().len();
    let before_active = app.__test_active_pane_in_tab(0);
    assert_eq!(before_active, Some(original));

    app.__test_split_active_right();

    let after_panes = app.__test_pane_ids().len();
    assert_eq!(
        after_panes,
        before_panes + 1,
        "split must spawn a second PaneState (was the v0.6 silent regression)",
    );
    let after_active = app.__test_active_pane_in_tab(0);
    assert!(after_active.is_some());
    assert_ne!(
        before_active, after_active,
        "split focuses the NEW pane, matching wezterm/tmux behaviour",
    );
}

#[test]
fn click_to_focus_flips_active_pane_back_to_original() {
    let mut app = make_app();
    let original = app.__test_seed_tab("first");
    app.__test_split_active_right();
    let new_focus = app.__test_active_pane_in_tab(0).expect("post-split active pane");
    assert_ne!(original, new_focus);

    // Simulate the click-to-focus production path: the window_event
    // handler resolves the pane under the cursor and calls the same
    // state mutation this hook exposes.
    assert!(app.__test_set_active_pane(0, original));
    assert_eq!(
        app.__test_active_pane_in_tab(0),
        Some(original),
        "click-to-focus must let the user return to the originally-active pane",
    );
}
