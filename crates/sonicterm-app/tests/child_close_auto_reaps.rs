//! PR #302 follow-up regression: every code path that drains a
//! child window's tab vec to zero MUST auto-reap the now-empty
//! child window. Haiku review on PR #302 flagged that the × button
//! path was fixed but Cmd+W (`Action::CloseTab` → `close_active_tab_in_child`)
//! and `CloseActivePaneOrTab` on a single-pane child still left a
//! ghost frame.
//!
//! Fix shape: the reap was moved INTO `close_tab_at_in_child`
//! (single source of truth) so all three callers
//! (`close_tab_at_in_child` direct, `close_active_tab_in_child`,
//! `close_active_pane_or_tab_in_child`-when-degraded) inherit the
//! behavior automatically. transfer_tab's separate
//! `windows.remove(&id)` source-empty branch is unchanged because it
//! already handled the drain itself.
//!
//! Coverage limits: a real `WindowState` requires a wgpu surface +
//! winit window, neither available in `cargo test`. The pure-logic
//! contract we CAN pin here is the missing-id no-op semantics for
//! each entry point — these prove the callers don't panic and don't
//! mutate main when the child is already gone (e.g. raced by a
//! previous close). The actual "ghost window disappears after Cmd+W
//! on last tab" behavior is covered by §13 GUI smoke in the PR body.
//!
//! See `crates/sonicterm-app/src/app/child_window.rs::close_tab_at_in_child`
//! for the implementation that all three paths now flow through.

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Keymap, Meta},
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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

/// Cmd+W path: `close_active_tab_in_child` on a stale child id must
/// be a no-op AND must NOT mutate main's tab vec. This pins the
/// degrade-gracefully contract that the auto-reap follow-up depends
/// on (the helper is now called transitively by the keymap
/// dispatcher; an early panic here would crash the app).
#[test]
fn cmd_w_on_missing_child_is_noop() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let main_before = app.__test_main_tab_count();
    let children_before = app.child_window_count();
    let ok = app.__test_invoke_close_active_tab_in_child(WindowId::dummy());
    assert!(!ok, "missing-child Cmd+W must return false");
    assert_eq!(app.__test_main_tab_count(), main_before, "must not mutate main");
    assert_eq!(app.child_window_count(), children_before, "must not invent a child");
}

/// CloseActivePaneOrTab path: same contract — degrades to
/// `close_active_tab_in_child` on a single-pane child, then to a
/// no-op when the child id is stale.
#[test]
fn close_active_pane_or_tab_on_missing_child_is_noop() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let main_before = app.__test_main_tab_count();
    let children_before = app.child_window_count();
    let ok = app.__test_invoke_close_active_pane_or_tab_in_child(WindowId::dummy());
    assert!(!ok);
    assert_eq!(app.__test_main_tab_count(), main_before);
    assert_eq!(app.child_window_count(), children_before);
}

/// Direct `close_tab_at_in_child` (× button path): missing id must
/// not panic and must not invent a `windows` entry via auto-reap.
/// `reap_empty_child` is only invoked when the close actually
/// drained a real entry, so the missing-id case bails before the
/// reap and the windows count is preserved.
#[test]
fn close_tab_at_on_missing_child_skips_auto_reap() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let children_before = app.child_window_count();
    let ok = app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 0);
    assert!(!ok);
    assert_eq!(app.child_window_count(), children_before);
}
