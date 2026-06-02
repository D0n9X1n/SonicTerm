//! Epic #289 Phase B — regression test for tearing out the LAST tab.
//!
//! Spec §B3: when the source window's only tab is torn out, the
//! source window must be closed (or for the main window: hidden via
//! the existing drained-main path). The torn tab becomes its own
//! new top-level window.
//!
//! Pure-logic coverage only — `tear_out_apply_source_side(0)` after
//! `detach_tab_state(0)` is the testable surface. The real
//! "new window appears and is frontmost" assertion is covered by the
//! §13 GUI smoke step in the PR body.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn lone_tab_detach_drains_main() {
    // After detach_tab_state(0) on a 1-tab main, main has 0 tabs.
    let mut app = synth_app();
    let p = app.__test_seed_tab("only");
    let (_tab, _state, panes) = app.detach_tab_state(0).expect("detach idx 0");
    assert!(panes.contains_key(&p), "the only pane MOVED into the detached tuple");
    assert_eq!(app.__test_tab_count(), 0);
    assert!(app.__test_pane_ids().is_empty(), "source App has zero panes after lone-tab detach");
}

#[test]
fn lone_tab_source_side_apply_with_no_child_windows_is_safe_noop_on_hide() {
    // When main is drained AND there are no child windows, the
    // source-side helper must NOT hide main (that would leave the
    // user with zero visible windows). It also must NOT crash on the
    // empty-tabs path.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    let _ = app.detach_tab_state(0);
    assert_eq!(app.__test_tab_count(), 0);
    app.tear_out_apply_source_side(0);
    assert!(
        !app.__test_main_hidden(),
        "no child windows → must not hide main (would leave zero visible windows)"
    );
}

#[test]
fn tear_out_apply_child_source_side_drops_drained_child() {
    // After tearing out a child's only tab, the child window entry
    // must be removed from `App.windows`. We can't construct a
    // real WindowState in tests (renderer requires a GPU surface),
    // but we CAN verify the contract on the missing-id path: a stale
    // WindowId is a no-op (no panic, no spurious insertion).
    let mut app = synth_app();
    let before = app.child_window_count();
    app.tear_out_apply_child_source_side(WindowId::dummy(), 0);
    assert_eq!(app.child_window_count(), before, "stale child id must be a safe no-op");
}
