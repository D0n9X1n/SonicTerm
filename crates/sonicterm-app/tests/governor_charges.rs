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

/// Serialises every test that enters the production sampling path.
///
/// An empirically effective guard whose exact cause is open. Run in parallel,
/// these tests flake at roughly one failed run in two hundred; serialised, the
/// failure has not been observed. Because the rate is that low, a short green
/// streak establishes nothing — twenty clean runs are the expected outcome
/// whether or not the fault is present.
///
/// It surfaces as the admitted-level test seeing its gate closed, so it fails
/// naming the charging path, which is not where the fault is. It is also rare
/// enough that instrumenting the assertion makes it stop reproducing, so the
/// obvious next step measures nothing.
///
/// What is established: `enabled!` consults process-global state — the static
/// max level, and the callsite's cached `Interest` — before it reaches the
/// thread-local dispatcher, and `tracing::subscriber::with_default` scopes the
/// subscriber but not that state. Both tests already install theirs through
/// `with_default` and flake anyway, so scoping is not the remedy and this lock
/// must not be dropped in favour of it.
///
/// What is ruled out, recorded so it is not investigated twice: the max level
/// never fell below DEBUG under a live scoped subscriber; the cached
/// `Interest` never went stale under the same conditions; and opposing
/// subscribers resolve to `sometimes`, which does consult the thread-local
/// dispatcher. Each was tested directly and refuted.
///
/// The rule is therefore unconditional — **hold this across any call that
/// enters the production sampling path**, whether or not the test installs a
/// subscriber and whether or not it asserts on the gate. A test cannot judge
/// from its own body whether it is affected, and an unidentified cause is
/// itself reason not to carve out exceptions.
///
/// [`App::__test_sample_pane_retention_now`] is that path.
static SAMPLING_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the sampling-gate lock for the rest of the current test.
///
/// Recovers a poisoned lock rather than propagating it: poisoning means some
/// other test panicked while holding it, and failing every later test would
/// bury the one real failure under a pile of noise.
fn serialised_sampling() -> std::sync::MutexGuard<'static, ()> {
    SAMPLING_GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

    let _serialised = serialised_sampling();
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

/// And charges at the default level too, where it also has to.
///
/// The gate governs the *log lines*, not the charging. A session at the
/// default level holds exactly as much memory as one running `memory=debug`,
/// and the governor's limits are enforced against charged figures — so a
/// governor that only charges when someone is watching is a governor that is
/// inert in every shipped session.
///
/// The objection this replaces was that charging unconditionally trades one
/// defect for a permanent cost. Measured, release build, eight panes:
/// **5.95 µs per sample**. Sampling runs once per 30 seconds, so the cost is
/// **714 µs per hour** — the walk is not what makes a session expensive.
#[test]
fn the_production_sampling_path_charges_at_the_default_level() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

    let _serialised = serialised_sampling();
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child window").first().expect("pane");

    let subscriber = Registry::default()
        .with(EnvFilter::try_new(sonicterm_logging::DEFAULT_FILTER).expect("valid filter"));

    let sampled =
        tracing::subscriber::with_default(subscriber, || app.__test_sample_pane_retention_now());

    assert!(!sampled, "the default level must not emit the memory log lines");
    let charged = app.__test_pane_charge_total(child, pane_id).expect("pane present");
    assert!(
        charged > 0,
        "the default level must still charge: a pane retains the same bytes whether or \
         not anyone is reading the log, and every governor limit is applied to the \
         charged figure"
    );
}

/// The committed budget is derived from the seam caps, not chosen.
///
/// This is what makes it safe to have at all. The objection to a governor
/// limit was that two limits which must agree and are maintained separately
/// drift, and the one that stops agreeing keeps reporting itself as enforced.
/// A limit *computed from* the caps cannot disagree with them: change a cap and
/// this moves with it.
///
/// The sum is only a derivation while every class has been decided about, so
/// this walks the whole enum rather than restating the terms it expects. A
/// class that starts charging a pane and is not given a term fails here, and a
/// class that has not been classified at all fails to compile in
/// `sonicterm-types` before it can reach this test.
#[test]
fn the_committed_budget_is_derived_from_the_seam_caps() {
    use sonicterm_app::app::{PANE_COMMITTED_BUDGET_BYTES, PANE_SEAM_CAP_SUM_BYTES};
    use sonicterm_types::{PaneSeamTerm, ResourceClass};

    // The cap behind each contributing class, recomputed from the seams
    // themselves. If someone replaces a derived constant with a literal, these
    // stop agreeing. `None` means the class is expected to carry no term.
    //
    // A wildcard arm is unavoidable here: `ResourceClass` is `#[non_exhaustive]`,
    // so a match outside `sonicterm-types` cannot omit one. The exhaustiveness
    // that forces a decision therefore lives on `pane_seam_term()`, at the
    // enum's definition site; this asserts the decision made there is honoured
    // by the arithmetic, and the wildcard returns `None` so a newly
    // contributing class arrives here with no term and fails rather than
    // passing silently.
    fn expected_term_bytes(class: ResourceClass) -> Option<usize> {
        match class {
            // One term for the three grid classes: `MAX_GRID_CELLS` bounds
            // visible, history, and saved primary together.
            ResourceClass::GridVisible => Some(
                sonicterm_grid::grid::MAX_GRID_CELLS as usize
                    * std::mem::size_of::<sonicterm_types::Cell>(),
            ),
            ResourceClass::GridHistory | ResourceClass::GridAlternate => Some(0),
            ResourceClass::InlineMediaRetained => Some(64 * 1024 * 1024),
            ResourceClass::ProtocolMetadata => {
                Some(sonicterm_grid::hyperlink::MAX_HYPERLINK_METADATA_BYTES)
            }
            ResourceClass::ParserCapture => Some(
                sonicterm_vt::vt::MAX_MEDIA_PAYLOAD_BYTES
                    + sonicterm_vt::vt::MAX_ESCAPE_SEQUENCE_BYTES,
            ),
            ResourceClass::PtyOutput => Some(sonicterm_io::pty::max_queued_output_ring_bytes()),
            ResourceClass::PtyInput => Some(sonicterm_io::pty::max_pty_queued_input_bytes()),
            _ => None,
        }
    }

    let mut expected = 0usize;
    for index in 0..<ResourceClass as enum_map::Enum>::LENGTH {
        let class = <ResourceClass as enum_map::Enum>::from_usize(index);
        match class.pane_seam_term() {
            PaneSeamTerm::Contributes => {
                let term = expected_term_bytes(class).unwrap_or_else(|| {
                    panic!(
                        "{class:?} contributes to the pane seam-cap sum but this test has no \
                         term for it; the sum is a derivation only while every contributing \
                         class carries the cap its seam enforces"
                    )
                });
                expected += term;
            }
            PaneSeamTerm::ChargedToAnotherOwnerKind | PaneSeamTerm::NotChargedInProduction => {
                assert!(
                    expected_term_bytes(class).is_none(),
                    "{class:?} is excluded from the pane seam-cap sum but this test gives it a \
                     term; the exclusion and the arithmetic must agree"
                );
            }
        }
    }

    assert_eq!(
        PANE_SEAM_CAP_SUM_BYTES, expected,
        "the seam-cap sum must be the sum of the caps, not a restatement of one"
    );
    // The omission this guards against, stated directly: the PTY reader ring is
    // charged to a pane and must be inside the backstop that reads a pane's
    // total. Asserted on the structural ceiling rather than the ring a real
    // shell pins, because the backstop sits above what the seam permits.
    assert!(
        PANE_SEAM_CAP_SUM_BYTES >= sonicterm_io::pty::max_queued_output_ring_bytes(),
        "the sum must cover the PTY output ring it is charged for"
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

/// A stalled capture is reclaimed after two quiet samples.
///
/// The seam for this landed in #940 and nothing called it, so a transfer
/// killed mid-flight pinned its staging until the pane died. This is the pass
/// that was missing.
#[test]
fn a_stalled_capture_is_reclaimed_by_the_sampling_pass() {
    let _serialised = serialised_sampling();
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child").first().expect("pane");

    // An APC introducer plus payload, with no terminator: the shape a killed
    // `imgcat` leaves behind.
    let mut chunk = Vec::with_capacity(2 * 1024 * 1024 + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(2 * 1024 * 1024, b'A');
    app.__test_advance_child_pane_parser(child, pane_id, &chunk);

    let held = app.__test_pane_retention(child, pane_id).expect("measures").parser.bytes;
    assert!(held > 0, "precondition: the capture is holding staging");

    // First sample: records the progress figure, cancels nothing. A capture
    // seen once might simply be slow.
    app.__test_sample_pane_retention_now();
    let after_first = app.__test_pane_retention(child, pane_id).expect("measures").parser.bytes;
    assert_eq!(
        after_first, held,
        "one quiet sample must not cancel; a slow transfer looks identical at this point"
    );

    // Second sample with no bytes in between: now it is stalled.
    app.__test_sample_pane_retention_now();
    let after_second = app.__test_pane_retention(child, pane_id).expect("measures").parser.bytes;
    assert_eq!(after_second, 0, "a capture quiet across two samples must be reclaimed");
}

/// A slow transfer must survive.
///
/// This is the way the reclamation could hurt a user: cancelling a capture
/// that was still arriving, just slowly, destroys an image they are waiting
/// for. Bytes arriving between samples must keep it alive indefinitely.
#[test]
fn a_slow_but_live_transfer_is_never_cancelled() {
    let _serialised = serialised_sampling();
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child").first().expect("pane");

    app.__test_advance_child_pane_parser(child, pane_id, b"\x1b_G");

    // Ten sampling rounds, each with a trickle of bytes in between — far more
    // than the two that would condemn a stalled one.
    for _ in 0..10 {
        app.__test_advance_child_pane_parser(child, pane_id, b"more payload bytes");
        app.__test_sample_pane_retention_now();

        let live = app.__test_pane_capture_count(child, pane_id).expect("pane present");
        assert_eq!(live, 1, "a capture still receiving bytes must never be cancelled");
    }
}

/// Cancelling must not disturb the pane.
///
/// Reclamation frees a buffer; it must not cost the user their scrollback or
/// leave the parser unable to render what comes next.
#[test]
fn reclaiming_a_stalled_capture_leaves_the_pane_usable() {
    let _serialised = serialised_sampling();
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_id = *app.__test_child_pane_ids(child).expect("child").first().expect("pane");

    app.__test_advance_child_pane_parser(child, pane_id, b"before the transfer\r\n");
    let mut chunk = Vec::with_capacity(1024 * 1024 + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(1024 * 1024, b'A');
    app.__test_advance_child_pane_parser(child, pane_id, &chunk);

    app.__test_sample_pane_retention_now();
    app.__test_sample_pane_retention_now();

    // The pane must still print.
    app.__test_advance_child_pane_parser(child, pane_id, b"after the cancel\r\n");
    let grid = app.__test_pane_retention(child, pane_id).expect("measures");
    assert!(grid.grid_visible.bytes > 0, "the pane must still hold its cells");
    assert_eq!(
        app.__test_pane_capture_count(child, pane_id),
        Some(0),
        "and no capture may be left in flight"
    );
}

/// Closing everything must return the process root to zero.
///
/// The last of #880's four clauses, and the one that catches the defect shape
/// this epic exists for: a charge taken and not returned reads as memory in
/// use forever. Nothing before this asserted it — every existing test measures
/// a *live* session, where a non-zero figure is correct.
///
/// Asserted on the process root rather than per owner, because that is where a
/// leak accumulates: an owner that never returns its charge leaves the root
/// non-zero even after the owner record is gone.
#[test]
fn the_process_root_returns_to_zero_after_every_pane_closes() {
    let mut app = app();

    let baseline = app.__test_governor_snapshot_root().process_amount;
    assert_eq!(baseline.bytes, 0, "precondition: a fresh app holds nothing");

    // Three windows, each with panes carrying real content.
    let mut windows = Vec::new();
    for _ in 0..3 {
        let child = app.__test_seed_child_window(&["one", "two"]);
        let pane_ids = app.__test_child_pane_ids(child).expect("child window");
        for pane_id in &pane_ids {
            for round in 0..200 {
                app.__test_advance_child_pane_parser(
                    child,
                    *pane_id,
                    format!("line {round} with \x1b]8;;https://example.com/{round}\x07a link\x1b]8;;\x07\r\n")
                        .as_bytes(),
                );
            }
        }
        windows.push(child);
    }

    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    let charged = app.__test_governor_snapshot_root().process_amount;
    assert!(
        charged.bytes > 0,
        "precondition: a populated session must charge something, or the teardown \
         assertion below passes for the wrong reason"
    );

    // Close every pane in every window, then the windows themselves.
    for window in &windows {
        while app.__test_child_pane_ids(*window).is_some_and(|ids| !ids.is_empty()) {
            app.__test_invoke_close_active_pane_in_child(*window);
        }
    }
    app.__test_drain_pending_os_teardown();
    app.__test_reconcile_pane_owners();

    let after = app.__test_governor_snapshot_root().process_amount;
    assert_eq!(
        after.bytes, 0,
        "the process root holds {} bytes after every pane closed; a charge taken and \
         not returned reads as memory in use for the life of the process",
        after.bytes
    );
    assert_eq!(after.items, 0, "and no items may remain");
}

/// A ledger that cannot release is worse than one that over-charges.
///
/// `release_failures` counts releases the ledger could not apply. Any non-zero
/// value means the process total is permanently over-counted and no owner can
/// reach zero — the figure would look like a leak while the memory was
/// actually freed, sending an operator hunting for something that is not
/// there.
#[test]
fn teardown_leaves_no_unapplied_releases() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    let pane_ids = app.__test_child_pane_ids(child).expect("child window");
    for pane_id in &pane_ids {
        app.__test_advance_child_pane_parser(child, *pane_id, b"content\r\n");
    }
    app.__test_reconcile_pane_owners();
    app.__test_force_retention_sample();

    while app.__test_child_pane_ids(child).is_some_and(|ids| !ids.is_empty()) {
        app.__test_invoke_close_active_pane_in_child(child);
    }
    app.__test_drain_pending_os_teardown();

    assert_eq!(
        app.__test_governor_snapshot_root().release_failures,
        0,
        "a release the ledger could not apply permanently over-counts the process total"
    );
}
