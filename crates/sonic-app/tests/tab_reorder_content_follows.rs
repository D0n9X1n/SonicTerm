//! Regression tests for drag-reorder active-index re-anchoring
//! (user report 2026-05).
//!
//! Bug: after drag-reorder, the tab BAR order updated but the
//! `active` index stayed pinned to its old slot. If the dragged tab
//! was a NON-active tab that crossed the active slot, the active
//! `Tab` instance now lived at a different index — so the bar
//! highlighted "wrong tab" and clicking the (newly-relocated)
//! original active tab showed someone else's grid/scrollback.
//!
//! Each test pins the identity of the active tab via its stable
//! `TabId` and asserts that `bar.tabs()[bar.active_index()].id`
//! still refers to the SAME tab after reorder.

use sonic_shared::tabs::{Tab, TabBar, TabId};

fn bar_with_n(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("T{i}")));
    }
    b
}

fn active_id(b: &TabBar) -> TabId {
    b.tabs()[b.active_index()].id
}

#[test]
fn reorder_first_tab_to_end_keeps_content_mapped() {
    let mut b = bar_with_n(3);
    b.activate(0);
    let original = active_id(&b);
    b.reorder(0, 2);
    // The active tab itself moved to slot 2 → active index follows.
    assert_eq!(b.active_index(), 2);
    assert_eq!(active_id(&b), original);
}

#[test]
fn reorder_unrelated_tab_doesnt_disturb_active() {
    let mut b = bar_with_n(4);
    b.activate(3);
    let original = active_id(&b);
    // Move tab 0 → 1: both ends are entirely to the left of active(3).
    b.reorder(0, 1);
    assert_eq!(b.active_index(), 3);
    assert_eq!(active_id(&b), original);
}

#[test]
fn reorder_past_active_decrements_active_idx() {
    let mut b = bar_with_n(3);
    b.activate(1);
    let original = active_id(&b);
    // Tab 0 (left of active) moves to slot 2 (right of active) → the
    // active tab slides one slot left: 1 → 0.
    b.reorder(0, 2);
    assert_eq!(b.active_index(), 0);
    assert_eq!(active_id(&b), original);
}

#[test]
fn reorder_into_active_shifts_active_right() {
    let mut b = bar_with_n(3);
    b.activate(0);
    let original = active_id(&b);
    // Tab 2 (right of active) moves to slot 0 (left of / onto active)
    // → active tab slides right: 0 → 1.
    b.reorder(2, 0);
    assert_eq!(b.active_index(), 1);
    assert_eq!(active_id(&b), original);
}
