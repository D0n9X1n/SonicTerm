//! #pane-geom regression tests for SPLIT panes in a torn-out CHILD window.
//!
//! These guard the bug class the manual tear-out testing kept surfacing:
//! a split pane in a child window kept the FULL child-grid column count
//! instead of its split sub-rect, so the left pane typed / wrapped across
//! the divider into the right pane. The fix sizes each pane to its own
//! `PaneTree::layout` sub-rect via `resize_visible_panes_in_child`; these
//! tests drive that wiring headlessly through the per-window pane-viewport
//! test seam (`__test_set_child_pane_viewport`) — without it the resize
//! helper silently no-ops on `renderer: None` synthetic children, which is
//! exactly why the regression slipped through the existing suite.
//!
//! Viewport: 800x240 logical px at 10x10 cells => a single pane spans the
//! full 80 columns; a left/right split gives each pane ~40 columns.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::pane::Rect;

fn child_with_viewport() -> (App, winit::window::WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let id = app.__test_seed_child_window(&["torn"]);
    // 800x240 @ 10x10 => 80 cols, 24 rows for a single full-width pane.
    assert!(app.__test_set_child_pane_viewport(id, Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    (app, id)
}

#[test]
fn child_split_sizes_each_pane_to_its_sub_rect_not_full_width() {
    let (mut app, id) = child_with_viewport();
    let left = app.__test_child_active_pane(id).expect("seeded child has an active pane");

    // Split right. The new pane becomes active; the old one is the sibling.
    assert!(app.__test_child_split_active_right(id), "split should succeed");
    let right = app.__test_child_active_pane(id).expect("split focuses the new pane");
    assert_ne!(left, right, "split must create a distinct second pane");
    assert_eq!(app.__test_child_pane_count(id), Some(2));

    // The bug: both panes kept 80 cols (full width) and overlapped. The fix:
    // each pane is sized to its ~half-width sub-rect (40 cols), 24 rows.
    let (lc, lr) = app.__test_child_pane_grid_size(id, left).expect("left grid");
    let (rc, rr) = app.__test_child_pane_grid_size(id, right).expect("right grid");
    assert!(lc < 80, "left pane must shrink below full width, got {lc} cols");
    assert!(rc < 80, "right pane must shrink below full width, got {rc} cols");
    assert_eq!(lc, 40, "left pane should take half of 80 cols");
    assert_eq!(rc, 40, "right pane should take half of 80 cols");
    assert_eq!(lr, 24, "rows are unchanged by a vertical split");
    assert_eq!(rr, 24);
}

#[test]
fn child_close_pane_refits_survivor_to_full_width() {
    let (mut app, id) = child_with_viewport();
    let survivor = app.__test_child_active_pane(id).expect("active pane");

    assert!(app.__test_child_split_active_right(id), "split should succeed");
    let closing = app.__test_child_active_pane(id).expect("split focuses new pane");
    assert_ne!(survivor, closing);
    assert_eq!(app.__test_child_pane_grid_size(id, survivor), Some((40, 24)));

    // Re-focus the survivor so close targets the OTHER pane, then close.
    // close_active_pane_or_tab_in_child closes whichever is active; we want
    // to keep `survivor`, so this asserts the survivor reclaims full width
    // regardless of which leaf is closed (the helper picks a remaining leaf).
    assert!(app.__test_invoke_close_active_pane_or_tab_in_child(id));
    assert_eq!(app.__test_child_pane_count(id), Some(1));

    // Whichever single pane remains must be refit to the full 80 columns —
    // this is the "right content reflows left" / "vim reflows" behavior.
    let remaining = app.__test_child_pane_ids(id).expect("ids");
    assert_eq!(remaining.len(), 1);
    let only = remaining[0];
    assert_eq!(
        app.__test_child_pane_grid_size(id, only),
        Some((80, 24)),
        "the surviving pane must reclaim full window width after close"
    );
}
