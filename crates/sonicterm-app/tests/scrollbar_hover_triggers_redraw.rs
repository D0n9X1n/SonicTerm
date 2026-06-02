//! Haiku follow-up for PR #411: Auto-mode scrollbar hover must wake the
//! renderer when the pointer crosses the right-edge proximity threshold.
//!
//! The production `CursorMoved` path updates `WindowState::cursor_pos` and
//! then calls the same App helper exercised here. The synthetic viewport keeps
//! the test headless while preserving the pane-layout and redraw-request
//! wiring under test.

use std::sync::atomic::Ordering;

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonicterm_ui::pane::Rect;

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
fn auto_hover_edge_crossings_request_redraw() {
    let mut app = make_app();
    let pane_id = app.__test_seed_tab("shell");
    app.test_viewport_override = Some((Rect::new(0.0, 0.0, 100.0, 100.0), 10.0, 20.0));

    app.main_mut().expect("synthetic main exists").cursor_pos = (95.0, 50.0);
    let before = app.redraw_request_count.load(Ordering::Relaxed);
    assert!(
        app.__test_refresh_scrollbar_hover_from_cursor(),
        "entering the right-edge strip should flip mouse_near_right_edge"
    );
    let after_enter = app.redraw_request_count.load(Ordering::Relaxed);
    assert_eq!(after_enter - before, 1, "hover entry must request one redraw");
    assert!(
        app.main()
            .and_then(|ws| ws.scrollbar_vis.get(&pane_id))
            .map(|s| s.mouse_near_right_edge)
            .unwrap_or(false),
        "pane should be marked near the right edge after entry"
    );

    app.main_mut().expect("synthetic main exists").cursor_pos = (94.0, 50.0);
    assert!(
        !app.__test_refresh_scrollbar_hover_from_cursor(),
        "moving within the already-near strip should not be a transition"
    );
    assert_eq!(
        app.redraw_request_count.load(Ordering::Relaxed),
        after_enter,
        "non-transition hover moves must not spam redraw requests"
    );

    app.main_mut().expect("synthetic main exists").cursor_pos = (10.0, 50.0);
    assert!(
        app.__test_refresh_scrollbar_hover_from_cursor(),
        "leaving the right-edge strip should flip mouse_near_right_edge"
    );
    let after_leave = app.redraw_request_count.load(Ordering::Relaxed);
    assert_eq!(after_leave - after_enter, 1, "hover exit must request one redraw");
    assert!(
        !app.main()
            .and_then(|ws| ws.scrollbar_vis.get(&pane_id))
            .map(|s| s.mouse_near_right_edge)
            .unwrap_or(true),
        "pane should be marked away from the right edge after exit"
    );
}
