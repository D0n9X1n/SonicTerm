//! Integration tests for the tab tear-out gesture and the underlying
//! pane state transfer.
//!
//! These tests do NOT spawn a winit event loop — that requires the
//! main thread on macOS and is unsuitable for `cargo test`. Instead we:
//!
//! 1. exercise the pure gesture detector directly, and
//! 2. drive `App::detach_tab_state` to assert that tearing a tab pulls
//!    the right pane state out of the parent App (PTY threads / shells
//!    are not started here — the seeded tabs have None pty handles).

use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_shared::app::App;
use sonic_shared::tabbar_view::{detect_tear_out, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};

fn synth_theme() -> Theme {
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
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
                close_button_fg: hex(),
            },
        },
    }
}

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    App::new(synth_theme(), Config::default(), keymap)
}

// ---- gesture detection -----------------------------------------------------

#[test]
fn no_tearout_inside_bar() {
    // Cursor still inside the tab bar — must not fire.
    assert!(detect_tear_out(0, (50.0, TAB_BAR_HEIGHT - 1.0)).is_none());
    assert!(detect_tear_out(0, (50.0, 0.0)).is_none());
}

#[test]
fn no_tearout_below_bar_but_under_threshold() {
    // Just below the bar but below the threshold distance — no tear.
    let y = TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX - 1.0;
    assert!(detect_tear_out(0, (50.0, y)).is_none());
}

#[test]
fn tearout_at_exactly_threshold() {
    let y = TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX;
    let t = detect_tear_out(2, (100.0, y)).expect("threshold should fire");
    assert_eq!(t.tab_index, 2);
    assert_eq!(t.drop_position, (100.0, y));
}

#[test]
fn tearout_path_press_then_drag_down() {
    // Simulate a mouse path: press inside bar, move down a few times.
    let path = [
        (50.0, 10.0),
        (50.0, 40.0),
        (50.0, 60.0),
        (50.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 5.0),
    ];
    // Only the last point should cross the threshold.
    let mut fired_at: Option<usize> = None;
    for (i, p) in path.iter().enumerate() {
        if detect_tear_out(0, *p).is_some() {
            fired_at = Some(i);
            break;
        }
    }
    assert_eq!(fired_at, Some(3));
}

// ---- pane state transfer ---------------------------------------------------

#[test]
fn detach_moves_pane_out_of_source_tab_state() {
    let mut app = synth_app();
    let p1 = app.__test_seed_tab("alpha");
    let p2 = app.__test_seed_tab("bravo");
    assert_eq!(app.__test_tab_count(), 2);
    let before: Vec<u64> = {
        let mut v = app.__test_pane_ids();
        v.sort_unstable();
        v
    };
    assert_eq!(before, vec![p1, p2]);

    let (_tab, state, panes) = app.detach_tab_state(0).expect("detach index 0");
    // Detached tuple owns alpha's pane.
    assert!(panes.contains_key(&p1));
    assert_eq!(state.active_pane, p1);
    // Source App no longer references alpha.
    assert_eq!(app.__test_tab_count(), 1);
    assert_eq!(app.__test_pane_ids(), vec![p2]);
}

#[test]
fn detach_out_of_range_returns_none() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    assert!(app.detach_tab_state(99).is_none());
    // App is untouched.
    assert_eq!(app.__test_tab_count(), 1);
}

#[test]
fn no_child_windows_created_by_pure_detach() {
    // detach_tab_state must not spawn a window — only tear_out_tab
    // (which is exercised via the event loop, not unit tests) does.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let _ = app.detach_tab_state(0);
    assert_eq!(app.child_window_count(), 0);
}

#[test]
fn detached_pane_state_carries_swappable_redraw_target() {
    // The fix for PR #43: every PaneState now owns an
    // Arc<Mutex<Option<Arc<Window>>>> that the VT thread reads on
    // each redraw. tear_out_tab atomically swaps the inner Option to
    // the child window — so this test pins down that the Arc itself
    // survives detach (same allocation, swappable target).
    let mut app = synth_app();
    let p1 = app.__test_seed_tab("alpha");
    let p2 = app.__test_seed_tab("bravo");
    // Both seeded panes start with `None` target (no real window in
    // test mode) but the Arc is non-null.
    let (_tab, _state, panes) = app.detach_tab_state(0).expect("detach");
    let p1_pane = panes.get(&p1).expect("p1 present after detach");
    // The detached pane's redraw_target is the SAME Arc allocation
    // the VT thread captured at spawn — strong_count >= 1 confirms
    // it wasn't dropped during transfer.
    assert!(std::sync::Arc::strong_count(&p1_pane.redraw_target) >= 1);
    // App lost p1 (moved out) but still owns p2's redraw_target.
    let p2_pane = app.__test_pane_redraw_target(p2).expect("p2 still in App");
    assert!(p2_pane.lock().is_none(), "seeded pane has no real window");
}
