//! Phase C2 (PR #295 follow-up): integration test for the
//! [`sonic_app::app::os_drag::TabBarRegistry`] hit-test that the
//! Windows `IDropTarget::Drop` callback (and any other off-thread OS
//! drag backend) uses to resolve a raw screen-coordinate drop into a
//! real `(WindowId, slot)` pair.
//!
//! Asserts:
//!   * A drop with a registered snapshot whose bar contains the point
//!     returns `Some((window, slot))` — never `None` and never the
//!     placeholder `(None, 0)` the old code shipped.
//!   * A drop in the window but outside the bar resolves to `None`
//!     so the caller can route to `DroppedOnEmpty`.
//!   * The slot index matches `TabBarLayout::drop_slot` semantics
//!     (left-of-midpoint → that tab's index; past last midpoint → n).
//!
//! These cover the bug Haiku flagged: the OLE drop callback hard-coded
//! `DroppedOnBar { target_window: None, target_slot: 0 }` regardless of
//! cursor position, so a real cross-window tab drop always landed at
//! slot 0 of the main window.

use sonic_app::app::os_drag::{TabBarRegistry, TabBarSnapshot};

#[test]
fn drop_inside_bar_resolves_to_window_and_slot() {
    let reg = TabBarRegistry::new();
    // Single registered window: outer 100..1100 × 50..650, bar 100..1100 × 50..80.
    // Three tabs at x = [100..400], [400..700], [700..1000]; midpoints at
    // 250, 550, 850.
    reg.publish(TabBarSnapshot {
        window: None, // main window
        window_rect: (100, 50, 1100, 650),
        bar_rect: (100, 50, 1100, 80),
        tab_lefts: vec![100, 400, 700],
        tab_rights: vec![400, 700, 1000],
    });

    // Drop on tab 0 (left of midpoint 250)
    assert_eq!(reg.resolve_screen_pos(150, 60), Some((None, 0)));
    // Drop between midpoint 250 and 550 (slot 1 — left of midpoint 550, past midpoint 250)
    assert_eq!(reg.resolve_screen_pos(300, 60), Some((None, 1)));
    // Drop right of all midpoints → slot 3 (end)
    assert_eq!(reg.resolve_screen_pos(900, 60), Some((None, 3)));
}

#[test]
fn drop_inside_window_outside_bar_resolves_to_none() {
    let reg = TabBarRegistry::new();
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (100, 50, 1100, 650),
        bar_rect: (100, 50, 1100, 80),
        tab_lefts: vec![100],
        tab_rights: vec![400],
    });

    // Inside window, below the bar → None (caller routes to DroppedOnEmpty).
    assert_eq!(reg.resolve_screen_pos(500, 400), None);
    // But any_window_contains still reports true for the same point.
    assert!(reg.any_window_contains(500, 400));
}

#[test]
fn drop_outside_all_windows_resolves_to_none_and_no_window_match() {
    let reg = TabBarRegistry::new();
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (100, 50, 1100, 650),
        bar_rect: (100, 50, 1100, 80),
        tab_lefts: vec![100],
        tab_rights: vec![400],
    });

    assert_eq!(reg.resolve_screen_pos(50, 30), None);
    assert!(!reg.any_window_contains(50, 30));
}

#[test]
fn republish_replaces_previous_snapshot_for_same_window() {
    let reg = TabBarRegistry::new();
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (0, 0, 100, 100),
        bar_rect: (0, 0, 100, 30),
        tab_lefts: vec![0],
        tab_rights: vec![100],
    });
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (0, 0, 200, 100),
        bar_rect: (0, 0, 200, 30),
        tab_lefts: vec![0],
        tab_rights: vec![200],
    });
    assert_eq!(reg.len(), 1, "republish must not accumulate duplicates");
    // New bar extends to 200, so a point at x=150 is inside.
    assert_eq!(reg.resolve_screen_pos(150, 15), Some((None, 1)));
}

#[test]
fn empty_bar_resolves_to_slot_zero() {
    let reg = TabBarRegistry::new();
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (0, 0, 1000, 600),
        bar_rect: (0, 0, 1000, 30),
        tab_lefts: vec![],
        tab_rights: vec![],
    });
    assert_eq!(reg.resolve_screen_pos(500, 15), Some((None, 0)));
}

#[test]
fn remove_clears_window_from_registry() {
    let reg = TabBarRegistry::new();
    reg.publish(TabBarSnapshot {
        window: None,
        window_rect: (0, 0, 100, 100),
        bar_rect: (0, 0, 100, 30),
        tab_lefts: vec![0],
        tab_rights: vec![100],
    });
    assert!(!reg.is_empty());
    reg.remove(None);
    assert!(reg.is_empty());
    assert_eq!(reg.resolve_screen_pos(50, 15), None);
}
