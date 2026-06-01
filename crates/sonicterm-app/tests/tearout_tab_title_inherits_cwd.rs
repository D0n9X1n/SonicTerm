//! Regression test for tear-out: a tab torn out into a new window
//! must keep its cwd-based wezterm-style title — not regress to the
//! literal "shell N" placeholder.
//!
//! The tear-out path lands in `app/child_window.rs`'s redraw handler,
//! which now uses the shared `refresh_active_tab_title` helper (see
//! sibling test `new_window_tab_title_uses_cwd.rs`). This test pins
//! the contract from the tear-out angle: a parser whose cwd was
//! already set in the source window keeps producing a cwd-derived
//! title after the redraw refresh runs in the destination window.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_core::{grid::Grid, vt::Parser};
use sonicterm_ui::tabs::{Tab, TabBar};

use sonicterm_app::app::{refresh_active_tab_title, PaneState};

fn parser_with_cwd(cwd_path: &str) -> Parser {
    let mut p = Parser::new(Grid::new(80, 24));
    let payload = format!("\x1b]7;file://localhost{cwd_path}\x1b\\");
    let _ = p.advance(payload.as_bytes());
    p
}

#[test]
fn tearout_destination_window_keeps_cwd_title() {
    // Source window had a tab with cwd "/tmp/foo" — its parser is
    // moved into the new window via tear-out. The new window's first
    // redraw constructs a fresh TabBar with the "shell 2" placeholder
    // and must immediately replace it with the cwd-derived title.
    let parser_arc = Arc::new(Mutex::new(parser_with_cwd("/tmp/foo")));
    let mut pane = PaneState::new(parser_arc.clone(), None);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell 2"));

    let g = parser_arc.lock();
    let _ = refresh_active_tab_title(&mut tabs, &mut pane, &g, 0);

    let title = tabs.active().map(|t| t.title.clone()).unwrap_or_default();
    assert_ne!(title, "shell 2", "tear-out destination must not stay on 'shell N'");
    assert!(
        title.contains("foo") || title.contains("tmp"),
        "tear-out destination title should derive from cwd /tmp/foo, got: {title:?}",
    );
}

#[test]
fn tearout_with_no_cwd_falls_back_gracefully() {
    // Defensive: if neither OSC 7 cwd nor OSC 0/2 title is set (e.g.,
    // shell without shell-integration scripts), the helper still must
    // not panic. The title may legitimately stay on the "shell N"
    // placeholder until OSC 7 fires — that's acceptable because the
    // helper returns None (no change) rather than overwriting with
    // garbage.
    let parser_arc = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    let mut pane = PaneState::new(parser_arc.clone(), None);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell 3"));

    let g = parser_arc.lock();
    let _applied = refresh_active_tab_title(&mut tabs, &mut pane, &g, 0);
    // Whatever the helper returns, the call must not panic. The
    // active title is allowed to remain "shell 3" in this degenerate
    // case (no OSC 7 yet, no OSC 0/2, no fg-proc info on this
    // PTY-less synthetic pane).
    let _ = tabs.active().map(|t| t.title.clone());
}
