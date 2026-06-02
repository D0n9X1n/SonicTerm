//! Semantics for `Action::CloseActivePaneOrTab` (Cmd+W).
//!
//! iTerm2/wezterm-style: when the active tab has more than one pane,
//! Cmd+W closes the focused pane and leaves the tab open; only when the
//! active tab has a single pane does the action collapse to the
//! whole-tab close. This test pins down the three observable
//! transitions plus the redraw-bump contract that PR #200 added to
//! every `run_action` caller.

use std::sync::Arc;

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Keymap, Meta},
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
    App::new(synth_theme(), Config::default(), empty_keymap())
}

#[test]
fn single_tab_single_pane_closes_the_tab() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    assert_eq!(app.__test_tab_count(), 1);
    assert_eq!(app.__test_pane_count_in_tab(0), Some(1));

    app.run_action(&Action::CloseActivePaneOrTab);

    assert_eq!(
        app.__test_tab_count(),
        0,
        "with one pane, CloseActivePaneOrTab must fall back to CloseTab"
    );
}

#[test]
fn single_tab_with_two_panes_closes_the_pane_not_the_tab() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_split_active_right();
    assert_eq!(app.__test_tab_count(), 1);
    assert_eq!(app.__test_pane_count_in_tab(0), Some(2), "split should have produced 2 panes");

    app.run_action(&Action::CloseActivePaneOrTab);

    assert_eq!(app.__test_tab_count(), 1, "tab must survive while another pane remains");
    assert_eq!(app.__test_pane_count_in_tab(0), Some(1), "one pane should be left after close");
}

#[test]
fn three_tabs_active_tab_split_closes_only_the_pane() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");
    app.__test_seed_tab("charlie");
    // `charlie` is the freshly-seeded active tab (push activates).
    app.__test_split_active_right();
    assert_eq!(app.__test_tab_count(), 3);
    assert_eq!(app.__test_pane_count_in_tab(2), Some(2));

    app.run_action(&Action::CloseActivePaneOrTab);

    assert_eq!(app.__test_tab_count(), 3, "tab count must stay at 3");
    assert_eq!(app.__test_pane_count_in_tab(2), Some(1), "the active tab loses one pane");
}

#[test]
fn close_active_pane_or_tab_fires_redraw_via_menubar_drain() {
    // The redraw bump is the #200 regression guard: every `run_action`
    // dispatch that mutates visible state must be followed by a
    // `request_redraw` so the frame reflects the new shape on the same
    // tick. The menubar bridge path is the one that historically forgot
    // it (Ctrl/Cmd+W on macOS). PR #271 follow-up audit (Haiku finding):
    // earlier this test only asserted state mutation, not the redraw
    // bump itself. We now read `App::redraw_request_count` before/after
    // a single drain and assert it goes up by exactly 1 — the real
    // counter assertion the audit asked for.
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_split_active_right();
    assert_eq!(app.__test_pane_count_in_tab(0), Some(2));

    let before = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);

    let _ = sonicterm_app::menubar_bridge::push_action(Action::CloseActivePaneOrTab);
    app.__test_drain_menubar_actions();

    let after = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        after - before,
        1,
        "exactly one redraw must be requested per drain batch \
         (before={before}, after={after}); PR #200 guard"
    );

    assert_eq!(app.__test_tab_count(), 1, "tab survives the pane close");
    assert_eq!(
        app.__test_pane_count_in_tab(0),
        Some(1),
        "exactly one pane closed per dispatch (no double-fire regression)"
    );

    // Silence unused-import warning on platforms where the bridge path
    // is the only consumer of `Arc` in tests.
    let _ = Arc::new(());
}
