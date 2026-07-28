//! Owners follow their panes.
//!
//! A `PaneState` carries its `owner` field when it moves between windows
//! during tab tear-out, and a pane's owner is created *below its window's*
//! owner. So a moved pane stays charged to the window it left unless
//! something re-parents it: the source window reports memory for a pane it no
//! longer has, and the destination reports none for a pane it does.
//!
//! Attribution is the whole point of the hierarchy. A per-pane figure is
//! already available without it; what the tree adds is "what does this window
//! hold", and a misattributed pane makes exactly that answer wrong.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// A new pane is owned before the sampler's interval elapses.
///
/// Owners were assigned only by the 30-second retention sampler, so a pane
/// created between samples was unaccounted for up to that long — and anything
/// reserving against its owner had nothing to reserve against.
#[test]
fn a_new_pane_is_owned_without_waiting_for_the_sampler() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);

    // No forced sample: this is the state a pane is in immediately after the
    // window that holds it becomes live.
    let owned = app.__test_child_pane_owner_count(child);

    assert_eq!(
        owned,
        Some(1),
        "a pane must have an owner as soon as its window does, not on the next \
         30-second sample; until then its memory is attributed to nothing"
    );
}

/// A window arrives in the hierarchy owned, with its panes parented below it.
///
/// A window is inserted with its panes already populated, so the owner has to
/// be registered at the insertion itself. Skip it and nothing later recovers:
/// both reconciliation and re-attribution pass over a window whose owner is
/// `None`, so its panes never get owners either and the entire subtree stays
/// invisible to the hierarchy for as long as the window is open — not
/// uncharged, absent. That is the state a user meets when they open a new
/// window, enable `memory=debug`, and find its panes missing from the report.
///
/// Asserts the parent edge, not just that owners exist. A pane owner that
/// exists but hangs off the wrong parent answers "what does this window hold"
/// incorrectly, which is the question the hierarchy is for.
#[test]
fn a_window_is_owned_and_parents_its_panes_when_it_is_inserted() {
    let mut app = app();

    // Insertion is the whole exercise: no reconcile, no forced sample. This is
    // the state a window is in the instant it becomes live.
    let window = app.__test_seed_child_window(&["one", "two"]);

    let window_owner =
        app.__test_window_owner(window).expect("a window must own a record the moment it exists");

    let panes = app.__test_child_pane_ids(window).expect("the window exists");
    assert_eq!(panes.len(), 2, "precondition: the window was seeded with two panes");

    for pane in panes {
        let pane_owner = app
            .__test_pane_owner(window, pane)
            .expect("a pane in an owned window must have an owner, not wait for a sample");
        let parent = app.__test_owner_snapshot(pane_owner).expect("the owner has a record").parent;
        assert_eq!(
            parent,
            Some(window_owner),
            "pane {pane} must be parented to the window that holds it; the hierarchy \
             reports it under {parent:?} instead"
        );
    }
}

/// A pane moved between windows is charged to the window it lands in.
#[test]
fn a_moved_pane_is_charged_to_its_new_window() {
    let mut app = app();
    let source = app.__test_seed_child_window(&["moving"]);
    let destination = app.__test_seed_child_window(&["stationary"]);
    app.__test_force_retention_sample();

    let moving_pane =
        *app.__test_child_pane_ids(source).expect("source exists").first().expect("one pane");
    let source_owner = app.__test_window_owner(source).expect("source registered");
    let destination_owner = app.__test_window_owner(destination).expect("destination registered");

    let before_source =
        app.__test_owner_snapshot(source_owner).expect("snapshots").owner_amount.bytes;
    assert!(before_source > 0, "precondition: the source window holds charges");

    // Move the pane and let accounting settle.
    assert!(
        app.__test_move_pane_between_windows(source, destination, moving_pane),
        "the pane moves"
    );
    app.__test_force_retention_sample();

    let after_source =
        app.__test_owner_snapshot(source_owner).expect("snapshots").owner_amount.bytes;
    let after_destination =
        app.__test_owner_snapshot(destination_owner).expect("snapshots").owner_amount.bytes;

    assert_eq!(
        after_source, 0,
        "the source window must stop reporting a pane it no longer has; it still \
         reports {after_source} bytes"
    );
    assert!(
        after_destination >= before_source,
        "the destination window must report the pane it now holds: {after_destination} \
         is not at least the {before_source} bytes that moved"
    );
}
