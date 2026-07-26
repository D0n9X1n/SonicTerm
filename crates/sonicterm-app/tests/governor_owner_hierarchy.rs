//! The governor's owner hierarchy mirrors the window/pane tree.
//!
//! `sonicterm-resource` was fully built and had **zero dependents** — a
//! governor, an owner registry, and RAII reservation tokens that nothing in
//! the shipping binary could reach. These tests exist because the first step
//! out of that state is not accounting but *ownership*: until a window and a
//! pane have owners, no later package can charge anything to them.
//!
//! This is deliberately ownership only. Nothing here charges bytes, and the
//! governor's limits are unlimited: enforcement stays with the per-seam caps
//! that are already tested, because two limits that must agree and are
//! maintained separately will drift.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// The governor is reachable from the shipping app at all.
///
/// The whole point of the change: `sonicterm-resource` goes from zero
/// dependents to one, so its root owner exists in a real `App`.
#[test]
fn the_app_holds_a_governor_with_a_process_root() {
    let app = app();
    let snapshot = app.__test_governor_snapshot_root();

    assert_eq!(
        snapshot.process_amount.bytes, 0,
        "ownership registration must not charge anything by itself"
    );
    assert_eq!(
        snapshot.release_failures, 0,
        "a freshly constructed governor must have a consistent ledger"
    );
}

/// A window registers an owner, and closing it releases that owner.
///
/// The release half matters more than the registration half: an owner that is
/// created and never closed ratchets the hierarchy, and because owners are
/// what later packages charge against, a leaked owner would hold charges no
/// live window is responsible for.
#[test]
fn a_window_registers_an_owner_and_releases_it_on_close() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);

    assert!(
        app.__test_window_owner(child).is_some(),
        "an inserted window must register an owner in the hierarchy"
    );

    let owner = app.__test_window_owner(child).expect("registered above");
    assert!(
        app.__test_owner_is_open(owner),
        "precondition: the owner is open while the window lives"
    );

    assert!(app.__test_remove_window(child), "the window is removed");

    // Asserted against the governor, not the window map: the window is gone
    // from the map whether or not its owner was released, so a map-based check
    // passes even when owners leak.
    assert!(
        !app.__test_owner_is_open(owner),
        "a removed window must close its owner; it is still open, so the hierarchy \
         has ratcheted and later charges would attach to a window that no longer \
         exists"
    );
}

/// Panes are reconciled into the hierarchy below their window.
///
/// Registration is by reconciliation rather than at each of the twelve pane
/// insert sites — several of which sit inside borrows with no governor in
/// scope. Threading registration through all of them is the "every call site
/// must remember" pattern that produces the one forgotten site.
#[test]
fn panes_are_reconciled_under_their_window() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two", "three"]);

    // Before reconciliation the panes exist without owners.
    assert_eq!(app.__test_child_pane_count(child), Some(3));
    app.__test_reconcile_pane_owners();

    let owned = app.__test_child_pane_owner_count(child);
    assert_eq!(
        owned,
        Some(3),
        "every pane in a window with an owner must be reconciled into the hierarchy"
    );
}

/// Reconciliation is idempotent.
///
/// It runs on every retention sample, so a second pass that created a second
/// owner per pane would ratchet the hierarchy once per sample — a leak driven
/// by the diagnostic that exists to find leaks.
#[test]
fn reconciling_twice_does_not_create_a_second_owner_per_pane() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two"]);

    app.__test_reconcile_pane_owners();
    let first = app.__test_child_pane_owners(child);
    app.__test_reconcile_pane_owners();
    app.__test_reconcile_pane_owners();
    let after = app.__test_child_pane_owners(child);

    assert_eq!(first, after, "repeated reconciliation must not reassign or duplicate owners");
}

/// Closing a window closes its pane owners too.
///
/// The governor refuses to finish closing a parent with open children, so a
/// pane owner left open would make the window owner's close fail — the
/// invariant that makes a leaked pane owner visible rather than silent.
#[test]
fn closing_a_window_closes_the_pane_owners_below_it() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_reconcile_pane_owners();
    assert_eq!(app.__test_child_pane_owner_count(child), Some(2));

    assert!(app.__test_remove_window(child));

    // A consistent ledger after teardown is the evidence: an unreleased pane
    // owner would have blocked its window owner's close.
    let snapshot = app.__test_governor_snapshot_root();
    assert_eq!(
        snapshot.release_failures, 0,
        "closing a window must leave the ledger consistent, with no owner stranded"
    );
}

/// Ownership registration charges nothing.
///
/// This change establishes hierarchy only. If it started charging bytes it
/// would be a second accounting system running beside the per-seam caps, and
/// two figures that must agree but are computed independently drift — the
/// defect shape of the charge-lifetime bug where a cap silently stopped
/// capping while still reporting itself as enforced.
#[test]
fn registering_owners_charges_nothing() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two", "three"]);
    app.__test_reconcile_pane_owners();

    let snapshot = app.__test_governor_snapshot_root();
    assert_eq!(
        snapshot.process_amount.bytes, 0,
        "the hierarchy must not charge bytes; enforcement stays with the per-seam caps"
    );
    assert_eq!(snapshot.process_amount.items, 0);
    let _ = child;
}

/// Closing a window through the production path must close its owners.
///
/// `release_window_owner` had exactly one caller: `__test_remove_window`, a
/// `#[doc(hidden)]` test seam. The production close paths call
/// `release_child_window_registries`, which touches three maps and the drag
/// backend and never mentions the governor.
///
/// Measured before this: 100 create/destroy cycles left **200 owners created
/// and 200 still `Open`** — not `Closing`, `Open`. Close was never attempted.
#[test]
fn the_production_close_path_closes_the_owners_it_created() {
    let mut app = app();

    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_reconcile_pane_owners();

    let window_owner = app.__test_window_owner(child).expect("window owner registered");
    let pane_owners: Vec<sonicterm_types::ResourceOwnerId> = app
        .__test_child_pane_owners(child)
        .into_iter()
        .filter_map(sonicterm_types::ResourceOwnerId::new)
        .collect();
    assert!(!pane_owners.is_empty(), "precondition: panes have owners");
    assert!(app.__test_owner_is_open(window_owner), "precondition: the window owner is open");

    // The production close path, not the test seam.
    while app.__test_child_pane_ids(child).is_some_and(|ids| !ids.is_empty()) {
        app.__test_invoke_close_active_pane_in_child(child);
    }
    app.__test_drain_pending_os_teardown();

    for owner in &pane_owners {
        assert!(
            !app.__test_owner_is_open(*owner),
            "pane owner {owner:?} is still open after its window closed"
        );
    }
    assert!(
        !app.__test_owner_is_open(window_owner),
        "the window owner is still open after the window closed"
    );
}

/// Repeated create/destroy must not ratchet the root's child count.
///
/// `OwnerRegistry` exposes `get` and `insert` and **no `remove`**, so every
/// `OwnerRecord` — with its `RwLock`, `Mutex`, and two `EnumMap`s over all
/// resource classes — is retained for the life of the process. An owner that
/// never closes is therefore a genuine leak, not merely a stale counter.
#[test]
fn repeated_window_cycles_do_not_ratchet_open_owners() {
    let mut app = app();
    const CYCLES: usize = 40;

    let mut still_open = 0usize;
    let mut created = 0usize;

    for _ in 0..CYCLES {
        let child = app.__test_seed_child_window(&["one"]);
        app.__test_reconcile_pane_owners();

        let window_owner = app.__test_window_owner(child).expect("window owner");
        let pane_owners: Vec<sonicterm_types::ResourceOwnerId> = app
            .__test_child_pane_owners(child)
            .into_iter()
            .filter_map(sonicterm_types::ResourceOwnerId::new)
            .collect();
        created += 1 + pane_owners.len();

        while app.__test_child_pane_ids(child).is_some_and(|ids| !ids.is_empty()) {
            app.__test_invoke_close_active_pane_in_child(child);
        }
        app.__test_drain_pending_os_teardown();

        if app.__test_owner_is_open(window_owner) {
            still_open += 1;
        }
        still_open += pane_owners.iter().filter(|o| app.__test_owner_is_open(**o)).count();
    }

    assert!(created > 0, "precondition: the cycles created owners");
    assert_eq!(
        still_open, 0,
        "{still_open} of {created} owners are still open after {CYCLES} create/destroy \
         cycles; the registry has no remove, so each one is retained for the life of \
         the process"
    );
}
