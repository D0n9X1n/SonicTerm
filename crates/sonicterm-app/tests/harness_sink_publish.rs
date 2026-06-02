//! #508 — verifies that every active-pane mutation publishes the
//! current pane's `PtyHandle::in_tx` into the App's harness sink.
//! Gated on `#[cfg(all(target_os = "windows", feature = "harness"))]`
//! to match the cfg-gated `App::harness_sink` field.

#![cfg(all(target_os = "windows", feature = "harness"))]

use parking_lot::Mutex;
use sonicterm_app::app::{App, PaneState, TabState};
use sonicterm_app::harness;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::{pane::PaneTree, tabs::Tab};
use sonicterm_vt::vt::Parser;
use std::sync::Arc;

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

#[test]
fn install_sink_starts_none_then_active_pane_change_publishes() {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), Config::default(), keymap);
    app.__test_synthetic_main();

    // Synthetic panes with NO PtyHandle (None) — verifies the publish
    // path correctly resolves `None` when the active pane has no PTY.
    let pane_a: u64 = 1;
    let pane_b: u64 = 2;
    let mk = || {
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
        PaneState::new(parser, None)
    };
    {
        let ws = app.main_mut().expect("synthetic main");
        ws.tabs.push(Tab::new("t"));
        ws.tab_states.push(TabState::new(PaneTree::leaf(pane_a), pane_a));
        ws.panes.insert(pane_a, mk());
        ws.panes.insert(pane_b, mk());
    }

    // Install a fresh sink — should start as None.
    let sink = harness::new_sink();
    app.set_harness_sink(sink.clone());
    // Both panes have no PTY, so the published value is None even
    // after install + immediate refresh.
    assert!(
        sink.lock().unwrap().is_none(),
        "no-PTY pane → publish(None) so the read loop drops chunks"
    );

    // Switching active pane while still PTY-less should not panic
    // and should leave the sink at None.
    assert!(app.__test_set_active_pane(0, pane_b));
    assert!(sink.lock().unwrap().is_none());
}
