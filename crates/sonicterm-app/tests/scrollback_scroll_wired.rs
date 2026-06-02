//! #412 — wheel + Scroll-keymap actions write to `PaneState.viewport_top_abs`.
//!
//! The scrollbar drag path (#410) is the only PRE-existing writer of this
//! field; pre-#412 a mouse wheel was silently consumed by `WindowEvent` and
//! `Action::Scroll(_)` was a "not yet wired up" stub. These tests lock the
//! wiring in by exercising `scroll_pane` (the canonical mutator both call
//! sites converge on) and the keymap dispatch arm.

use sonicterm_app::app::App;
use sonicterm_cfg::{
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

fn build_app_with_pane(scrollback_lines: u32) -> (App, u64, u64) {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), Config::default(), keymap);
    let pane_id = app.__test_seed_tab("only");
    let sb = app.__test_grow_pane_scrollback(pane_id, scrollback_lines);
    (app, pane_id, sb)
}

fn vt_abs(app: &App, pane_id: u64) -> Option<u64> {
    app.__test_pane_viewport_top_abs(pane_id).expect("pane present")
}

#[test]
fn scroll_pane_decreases_viewport_top_abs_by_delta() {
    let (mut app, pane_id, sb) = build_app_with_pane(200);
    // Park at live tail (None).
    assert_eq!(vt_abs(&app, pane_id), None);
    app.scroll_pane(pane_id, -3);
    let new_top = vt_abs(&app, pane_id);
    assert!(new_top.is_some(), "wheel-up must engage explicit viewport_top_abs");
    assert_eq!(new_top.unwrap(), sb - 3);
}

#[test]
fn scroll_pane_clamps_at_top_of_scrollback() {
    let (mut app, pane_id, _sb) = build_app_with_pane(50);
    app.scroll_pane(pane_id, i32::MIN);
    assert_eq!(vt_abs(&app, pane_id), Some(0));
}

#[test]
fn scroll_pane_at_or_past_live_tail_snaps_to_none() {
    let (mut app, pane_id, _sb) = build_app_with_pane(100);
    app.scroll_pane(pane_id, -10);
    assert!(vt_abs(&app, pane_id).is_some());
    app.scroll_pane(pane_id, i32::MAX);
    assert_eq!(vt_abs(&app, pane_id), None);
}

#[test]
fn keymap_scroll_lineup_dispatches_through_scroll_pane() {
    use sonicterm_cfg::keymap::{Action, ScrollAction};
    let (mut app, pane_id, sb) = build_app_with_pane(150);
    assert!(app.run_action(&Action::Scroll(ScrollAction::LineUp)));
    assert_eq!(vt_abs(&app, pane_id), Some(sb - 1));
}

#[test]
fn keymap_scroll_pageup_uses_viewport_rows() {
    use sonicterm_cfg::keymap::{Action, ScrollAction};
    let (mut app, pane_id, sb) = build_app_with_pane(150);
    let rows = app.__test_pane_viewport_rows(pane_id).unwrap() as u64;
    assert!(app.run_action(&Action::Scroll(ScrollAction::PageUp)));
    assert_eq!(vt_abs(&app, pane_id), Some(sb - rows));
}

#[test]
fn keymap_scroll_pagedown_moves_toward_tail() {
    use sonicterm_cfg::keymap::{Action, ScrollAction};
    let (mut app, pane_id, _sb) = build_app_with_pane(150);
    app.scroll_pane(pane_id, -100);
    let before = vt_abs(&app, pane_id).unwrap();
    assert!(app.run_action(&Action::Scroll(ScrollAction::PageDown)));
    if let Some(v) = vt_abs(&app, pane_id) {
        assert!(v > before, "PageDown should advance toward live tail");
    }
    // else: snapped to live tail — also fine
}

#[test]
fn keymap_scroll_totop_jumps_to_zero() {
    use sonicterm_cfg::keymap::{Action, ScrollAction};
    let (mut app, pane_id, _sb) = build_app_with_pane(150);
    assert!(app.run_action(&Action::Scroll(ScrollAction::ToTop)));
    assert_eq!(vt_abs(&app, pane_id), Some(0));
}

#[test]
fn keymap_scroll_tobottom_jumps_to_tail() {
    use sonicterm_cfg::keymap::{Action, ScrollAction};
    let (mut app, pane_id, _sb) = build_app_with_pane(150);
    app.scroll_pane(pane_id, -50);
    assert!(app.run_action(&Action::Scroll(ScrollAction::ToBottom)));
    assert_eq!(vt_abs(&app, pane_id), None);
}

#[test]
fn scroll_pane_wheel_past_top_clamps_at_zero() {
    let (mut app, pane_id, _sb) = build_app_with_pane(80);
    app.scroll_pane(pane_id, -10_000);
    assert_eq!(vt_abs(&app, pane_id), Some(0));
}

#[test]
fn scroll_pane_wheel_past_bottom_clamps_at_tail() {
    let (mut app, pane_id, _sb) = build_app_with_pane(80);
    app.scroll_pane(pane_id, -5);
    assert!(vt_abs(&app, pane_id).is_some());
    app.scroll_pane(pane_id, 10_000);
    assert_eq!(vt_abs(&app, pane_id), None);
}
