//! #412 — `App::pane_at_cursor` resolves a logical-px point to the pane
//! whose rect contains it. Used by the `WindowEvent::MouseWheel` arm to
//! target the pane under the cursor (the keymap path always targets the
//! active pane and does not call this).

use parking_lot::Mutex;
use sonicterm_app::app::{App, PaneState, TabState};
use sonicterm_core::{
    config::Config,
    grid::Grid,
    keymap::{Direction, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
    vt::Parser,
};
use sonicterm_ui::{
    pane::{PaneTree, Rect},
    tabs::Tab,
};
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

fn split_app() -> (App, u64, u64) {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), Config::default(), keymap);
    app.__test_synthetic_main();
    let pane_a: u64 = 1;
    let pane_b: u64 = 2;
    let make_pane = || {
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
        PaneState::new(parser, None)
    };
    let mut tree = PaneTree::leaf(pane_a);
    assert!(tree.split(pane_a, Direction::Right, pane_b));
    let ws = app.main_mut().expect("synthetic main");
    ws.tabs.push(Tab::new("test"));
    ws.tab_states.push(TabState::new(tree, pane_b));
    ws.panes.insert(pane_a, make_pane());
    ws.panes.insert(pane_b, make_pane());
    // 1000x700 viewport; cell size irrelevant for hit-testing pane rects.
    app.test_viewport_override = Some((Rect::new(0.0, 0.0, 1000.0, 700.0), 10.0, 20.0));
    (app, pane_a, pane_b)
}

#[test]
fn pane_at_cursor_inside_left_pane() {
    let (app, left, _right) = split_app();
    // Left half: x ∈ [0, 500).
    assert_eq!(app.pane_at_cursor(100.0, 350.0), Some(left));
}

#[test]
fn pane_at_cursor_inside_right_pane() {
    let (app, _left, right) = split_app();
    // Right half: x ∈ [500, 1000).
    assert_eq!(app.pane_at_cursor(750.0, 350.0), Some(right));
}

#[test]
fn pane_at_cursor_outside_any_pane_returns_none() {
    let (app, _left, _right) = split_app();
    // Well past the viewport bottom.
    assert_eq!(app.pane_at_cursor(500.0, 5000.0), None);
    assert_eq!(app.pane_at_cursor(-10.0, 100.0), None);
}
