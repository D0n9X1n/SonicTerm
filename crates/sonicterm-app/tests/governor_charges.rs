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

/// The gated production path charges; the test seam alone never proved it.
///
/// Every test in this file reaches charging through
/// `__test_force_retention_sample`, which calls `charge_pane_owners` directly.
/// Production reaches it only through `sample_pane_retention`, which returns
/// immediately unless `enabled!(target: "memory", DEBUG)` holds.
///
/// No configured log level named the `memory` target, so that gate was closed
/// in every shipped session and **nothing was ever charged**. These tests all
/// passed throughout, because they enter below the gate — they verified the
/// charge logic, which was never the part that was broken.
///
/// This one enters above it.
#[test]
fn the_production_sampling_path_charges_when_the_memory_target_is_admitted() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child window").first().expect("pane");

    // The filter a `level = "debug"` session actually gets, not a directive
    // written here. Writing one would prove the gate opens when the target is
    // admitted and say nothing about whether any configured level admits it —
    // which was the whole defect.
    let debug_filter = sonicterm_logging::filter_for_level(sonicterm_logging::LogLevel::Debug);
    let subscriber =
        Registry::default().with(EnvFilter::try_new(debug_filter).expect("valid filter"));

    let sampled =
        tracing::subscriber::with_default(subscriber, || app.__test_sample_pane_retention_now());

    assert!(sampled, "with the memory target admitted, a due sample must run");
    let charged = app.__test_pane_charge_total(child, pane_id).expect("pane present");
    assert!(charged > 0, "the production path must charge, not only measure");
}

/// And stays inert at the default level.
///
/// The gate is load-bearing in both directions: it keeps a walk over every
/// pane out of an ordinary session. A fix that charged unconditionally would
/// trade one defect for a permanent cost.
#[test]
fn the_production_sampling_path_stays_inert_at_the_default_level() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child window").first().expect("pane");

    let subscriber = Registry::default()
        .with(EnvFilter::try_new(sonicterm_logging::DEFAULT_FILTER).expect("valid filter"));

    let sampled =
        tracing::subscriber::with_default(subscriber, || app.__test_sample_pane_retention_now());

    assert!(!sampled, "the default level must not run the sampling walk");
    assert_eq!(app.__test_pane_charge_total(child, pane_id), Some(0), "and must not charge");
}

/// The committed budget is derived from the seam caps, not chosen.
///
/// This is what makes it safe to have at all. The objection to a governor
/// limit was that two limits which must agree and are maintained separately
/// drift, and the one that stops agreeing keeps reporting itself as enforced.
/// A limit *computed from* the caps cannot disagree with them: change a cap and
/// this moves with it.
#[test]
fn the_committed_budget_is_derived_from_the_seam_caps() {
    use sonicterm_app::app::{PANE_COMMITTED_BUDGET_BYTES, PANE_SEAM_CAP_SUM_BYTES};

    // Recomputed here from the caps themselves. If someone replaces the derived
    // constant with a literal, these stop agreeing.
    let expected = (sonicterm_grid::grid::MAX_GRID_CELLS as usize
        * std::mem::size_of::<sonicterm_types::Cell>())
        + 64 * 1024 * 1024
        + sonicterm_grid::hyperlink::MAX_HYPERLINK_METADATA_BYTES
        + sonicterm_vt::vt::MAX_MEDIA_PAYLOAD_BYTES
        + sonicterm_vt::vt::MAX_ESCAPE_SEQUENCE_BYTES
        + (sonicterm_app::app::MAX_PANE_COMMAND_EVENTS * 40);

    assert_eq!(
        PANE_SEAM_CAP_SUM_BYTES, expected,
        "the seam-cap sum must be the sum of the caps, not a restatement of one"
    );
    // Both are compile-time constants, so these are decided before the test
    // runs. Stated as const assertions rather than runtime ones so the build
    // fails rather than the suite — a backstop below the caps it backstops is
    // not a test failure, it is a design that cannot ship.
    const _: () = assert!(
        PANE_COMMITTED_BUDGET_BYTES > PANE_SEAM_CAP_SUM_BYTES,
        "the backstop must sit above the caps it backstops, or it becomes the enforcer"
    );
    const _: () = assert!(
        PANE_COMMITTED_BUDGET_BYTES <= PANE_SEAM_CAP_SUM_BYTES * 4,
        "and stay a small multiple, or it bounds nothing in practice"
    );
}

/// A pane behaving correctly must never approach the backstop.
///
/// The property that makes this a tripwire rather than a second enforcement
/// point. If ordinary operation came near it, it would start refusing what the
/// user asked for — which is the failure mode the whole design avoids.
#[test]
fn a_correctly_behaving_pane_stays_far_below_the_committed_budget() {
    use sonicterm_app::app::PANE_COMMITTED_BUDGET_BYTES;

    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child window").first().expect("pane");

    // Fill the pane the way a long session does: scrollback, links, styling.
    for round in 0..4_000 {
        app.__test_advance_child_pane_parser(
            child,
            pane_id,
            format!(
                "\x1b[3{}mline {round} with styling and \
                 \x1b]8;;https://example.com/{round}\x07a link\x1b]8;;\x07\r\n",
                round % 8
            )
            .as_bytes(),
        );
    }

    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    let charged = app.__test_pane_charge_total(child, pane_id).expect("pane present");
    assert!(charged > 0, "precondition: the pane charged something");
    assert!(
        charged < PANE_COMMITTED_BUDGET_BYTES / 2,
        "a correctly behaving pane charged {charged} against a {PANE_COMMITTED_BUDGET_BYTES}-byte \
         backstop; the backstop must never be near ordinary operation or it enforces"
    );
}

/// The budget is a real limit the ledger holds, not a number in a doc comment.
///
/// Reads it back from the governor rather than from the constant, so a limit
/// that is computed correctly and never installed fails here.
#[test]
fn the_governor_actually_holds_pane_owners_to_the_budget() {
    use sonicterm_app::app::PANE_COMMITTED_BUDGET_BYTES;

    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    app.__test_reconcile_pane_owners();

    let limit = app.__test_pane_owner_limit(child).expect("a pane owner exists");
    assert_eq!(
        limit, PANE_COMMITTED_BUDGET_BYTES,
        "the governor must hold pane owners to the derived budget; a limit that is \
         computed and never installed is a comment"
    );
}
