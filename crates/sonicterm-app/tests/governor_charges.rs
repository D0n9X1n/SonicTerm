//! The governor's figures match what the retention seams report.
//!
//! The hierarchy in #934 established ownership without charging anything. This
//! is the next increment: each pane's retention is charged to its owner, so a
//! window's memory total is derivable from the ledger rather than only from a
//! log line.
//!
//! The property under test is **agreement**, not magnitude. Two systems that
//! are supposed to report the same number and are maintained separately will
//! drift, and the drift surfaces as a figure that looks authoritative and is
//! wrong. That is the shape of the charge-lifetime defect, where a cap kept
//! reporting itself as enforced after it had stopped enforcing.

use sonicterm_app::app::retention::seam_classes;
use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// A pane's charges equal what its seams report.
///
/// Asserted per class rather than on the total: a total can agree while two
/// classes are swapped, and a swapped class sends an operator reading the
/// dominant-class line to the wrong subsystem.
#[test]
fn pane_charges_equal_what_the_seams_report() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    let pane_ids = app.__test_child_pane_ids(child).expect("the child window exists");
    let pane_id = *pane_ids.first().expect("one pane");

    let measured = app.__test_pane_retention(child, pane_id).expect("an uncontended pane measures");
    let charged = app.__test_pane_charges(child, pane_id).expect("the pane holds charges");

    for (class, amount) in seam_classes(&measured) {
        let held = charged.get(&class).copied().unwrap_or_default();
        assert_eq!(
            held.bytes, amount.bytes,
            "class {class:?} is charged {} bytes but the seam reports {} — the ledger \
             and the log would disagree about the same pane",
            held.bytes, amount.bytes
        );
    }
}

/// The window owner's total is the sum of its panes.
///
/// This is what the hierarchy exists for: per-pane figures cannot answer "what
/// does this window hold", and that is the question behind closing a window to
/// reclaim memory.
#[test]
fn a_window_total_is_the_sum_of_its_panes() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two", "three"]);
    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    let pane_ids = app.__test_child_pane_ids(child).expect("the child exists");
    let summed: usize = pane_ids
        .iter()
        .filter_map(|id| app.__test_pane_charges(child, *id))
        .flat_map(|charges| charges.into_values())
        .map(|amount| amount.bytes)
        .sum();

    let window_owner = app.__test_window_owner(child).expect("the window registered an owner");
    let snapshot = app.__test_owner_snapshot(window_owner).expect("the owner snapshots");

    assert!(summed > 0, "three live panes must charge something");
    assert_eq!(
        snapshot.owner_amount.bytes, summed,
        "the window owner's total must equal the sum of its panes' charges"
    );
}

/// Charges follow retention downward, not just upward.
///
/// A charge that only grows would report a pane's high-water mark forever,
/// which reads exactly like a leak in the subsystem that actually released the
/// memory.
#[test]
fn charges_shrink_when_a_pane_releases_memory() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    app.__test_reconcile_pane_owners();

    let pane_id = *app.__test_child_pane_ids(child).expect("exists").first().expect("one pane");

    // Grow scrollback, sample, then shrink it and sample again.
    app.__test_set_child_pane_scrollback(child, pane_id, 5_000);
    app.__test_advance_child_pane_parser(
        child,
        pane_id,
        "filler line\r\n".repeat(2_000).as_bytes(),
    );
    app.__test_force_retention_sample();
    let high = app.__test_pane_charge_total(child, pane_id).expect("charged");

    app.__test_set_child_pane_scrollback(child, pane_id, 10);
    app.__test_force_retention_sample();
    let low = app.__test_pane_charge_total(child, pane_id).expect("charged");

    assert!(
        low < high,
        "a pane that released scrollback must charge less: {low} is not below {high}"
    );
}

/// Dropping a pane releases its charges.
///
/// `CommittedReservation::Drop` returns the charge, so there is no teardown
/// site to forget — the same property that made the inline-media charge
/// correct after it was fixed to co-own with the pane.
#[test]
fn dropping_a_pane_releases_its_charges() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    let window_owner = app.__test_window_owner(child).expect("registered");
    let before = app.__test_owner_snapshot(window_owner).expect("snapshots").owner_amount.bytes;
    assert!(before > 0, "precondition: the window holds charges");

    app.__test_invoke_close_tab_at_in_child(child, 0);
    app.__test_force_retention_sample();

    let after = app.__test_owner_snapshot(window_owner).expect("snapshots").owner_amount.bytes;
    assert!(
        after < before,
        "closing a tab must release its pane's charges: {after} is not below {before}"
    );
}

/// Closing the window returns the process total to zero.
///
/// The end-to-end property: every charge opened through the hierarchy is
/// released by it. A residue here means some owner holds bytes no live pane is
/// responsible for, which is the ratchet that eventually reports a process as
/// full when it is empty.
#[test]
fn closing_a_window_returns_the_process_total_to_zero() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    assert!(
        app.__test_governor_snapshot_root().process_amount.bytes > 0,
        "precondition: the process holds charges"
    );

    assert!(app.__test_remove_window(child), "the window is removed");

    let root = app.__test_governor_snapshot_root();
    assert_eq!(
        root.process_amount.bytes, 0,
        "every charge must be released with its window; {} bytes remain attributed to \
         panes that no longer exist",
        root.process_amount.bytes
    );
    assert_eq!(root.release_failures, 0, "and the ledger must stay consistent through teardown");
}
