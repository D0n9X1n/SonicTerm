//! Regression test for the user-reported bug: new windows (Cmd+N,
//! tear-out) opened a tab labeled "shell N" instead of the cwd-based
//! wezterm-style title that the main window's tabs show.
//!
//! Root cause: the wezterm-style title formatter
//! (`sonicterm_ui::tab_title::format_tab_title`) was only invoked on the
//! main-window redraw path in `app/window_event.rs`. Child windows
//! created via `Cmd+N` or tear-out have their own redraw handler in
//! `app/child_window.rs`, which previously skipped the title-update
//! step — so the literal "shell N" placeholder set at spawn time was
//! never replaced.
//!
//! Fix: extracted `refresh_active_tab_title` in `app/mod.rs` and call
//! it from both redraw paths so OSC 7 cwd / foreground-process /
//! OSC 0|2 title flow into every window's tab bar uniformly.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_core::{grid::Grid, vt::Parser};
use sonicterm_ui::tabs::{Tab, TabBar};

use sonicterm_app::app::{refresh_active_tab_title, PaneState};

fn parser_with_cwd(cwd_path: &str) -> Parser {
    let mut p = Parser::new(Grid::new(80, 24));
    // OSC 7 ; file://host/<path> ST — shell-reported cwd.
    let payload = format!("\x1b]7;file://localhost{cwd_path}\x1b\\");
    let _ = p.advance(payload.as_bytes());
    assert_eq!(p.cwd(), Some(cwd_path));
    p
}

fn synth_pane_state(parser: Arc<Mutex<Parser>>) -> PaneState {
    // No PTY → foreground-process probe will simply return None and
    // the title falls through to the cwd-only branch of
    // `format_tab_title`. That is the exact code path we want to
    // exercise here: cwd present, proc_name absent.
    PaneState::new(parser, None)
}

#[test]
fn child_window_redraw_replaces_fallback_with_cwd_title() {
    // Simulate the buggy state a child window starts in: TabBar has
    // one tab whose title is the literal "shell 2" placeholder set by
    // `spawn_tab_in_child`. The parser has just received an OSC 7
    // cwd. The first redraw should rename the tab to a cwd-derived
    // title and drop the "shell N" string.
    let parser_arc = Arc::new(Mutex::new(parser_with_cwd("/Users/d0n9x1n")));
    let mut pane = synth_pane_state(parser_arc.clone());
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell 2"));

    let parser_guard = parser_arc.lock();
    let applied = refresh_active_tab_title(&mut tabs, &mut pane, &parser_guard, 0);

    let new_title = tabs.active().map(|t| t.title.clone()).unwrap_or_default();
    assert!(applied.is_some(), "title must update on first redraw, got: {new_title:?}");
    assert_ne!(new_title, "shell 2", "fallback 'shell N' must be replaced");
    assert!(
        new_title.contains("d0n9x1n") || new_title.contains("Users"),
        "title should reflect cwd path, got: {new_title:?}",
    );
}

#[test]
fn refresh_is_idempotent_no_double_apply() {
    // Calling the helper twice with the same parser state must produce
    // a "no change" result the second time. Render path ticks ~26×/s
    // while the cursor blinks; reapplying the same title every tick
    // would spam the tab-bar layout cache.
    let parser_arc = Arc::new(Mutex::new(parser_with_cwd("/tmp/foo")));
    let mut pane = synth_pane_state(parser_arc.clone());
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell 1"));

    let parser_guard = parser_arc.lock();
    let first = refresh_active_tab_title(&mut tabs, &mut pane, &parser_guard, 0);
    assert!(first.is_some(), "first call applies");
    let second = refresh_active_tab_title(&mut tabs, &mut pane, &parser_guard, 0);
    assert!(second.is_none(), "second call is a no-op (same inputs)");
}
