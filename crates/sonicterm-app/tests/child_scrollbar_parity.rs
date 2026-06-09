//! #pane-scrollbar parity tests for torn-out CHILD windows.
//!
//! The main window briefly shows its auto-hide scrollbar whenever the user
//! scrolls (wheel / view_top jump) via `mark_scrollbar_active`. Torn-out
//! child windows must do the same so scrollback feels identical regardless
//! of which window a tab lives in. These drive the production child path
//! (`set_child_pane_view_top`, exposed through `__test_child_set_pane_view_top`)
//! and assert the child's per-pane visibility state lights up — the
//! regression that left child windows' scrollbars inert after tear-out.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn seeded_child() -> (App, winit::window::WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let id = app.__test_seed_child_window(&["torn"]);
    let pane = app.__test_child_active_pane(id).expect("seeded child has an active pane");
    (app, id, pane)
}

#[test]
fn child_view_top_jump_marks_scrollbar_active() {
    let (mut app, id, pane) = seeded_child();
    // Before any scroll, the pane has no recorded scrollbar activity.
    assert_eq!(
        app.__test_child_scrollbar_active(id, pane),
        None,
        "no scrollbar_vis entry exists until the pane scrolls"
    );

    // Scroll up into the scrollback (view_top below the live tail). This is
    // the same call the wheel / track-page / drag paths route through.
    app.__test_child_set_pane_view_top(id, pane, 5, 100);

    assert_eq!(
        app.__test_child_scrollbar_active(id, pane),
        Some(true),
        "a child scroll must light the auto-hide scrollbar like the main window"
    );
}

#[test]
fn child_scrollbar_active_is_pane_scoped() {
    // Splitting gives two panes; scrolling one must not mark the other's
    // scrollbar active (each pane owns its own visibility state).
    let (mut app, id, left) = seeded_child();
    assert!(app.__test_child_split_active_right(id), "split should succeed");
    let right = app.__test_child_active_pane(id).expect("split focuses new pane");
    assert_ne!(left, right);

    app.__test_child_set_pane_view_top(id, right, 3, 100);

    assert_eq!(app.__test_child_scrollbar_active(id, right), Some(true), "scrolled pane is active");
    assert_eq!(
        app.__test_child_scrollbar_active(id, left),
        None,
        "the un-scrolled sibling pane must NOT be marked active"
    );
}
