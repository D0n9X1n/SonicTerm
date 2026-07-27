//! Does the governor get charged in a session nobody is watching?
//!
//! Retention sampling emits a `memory` log line, and the whole pass sat behind
//! `enabled!(target: "memory", DEBUG)`. Reclamation was lifted above that gate
//! because freeing memory is not a diagnostic. Charging is not a diagnostic
//! either: it is what puts a pane's retention into the ledger every limit is
//! enforced against.
//!
//! Measured with the charging step below the gate and no subscriber installed
//! — the shipped default: a pane holding 979,096 bytes was charged 0, every
//! owner's usage stayed empty, and the governor reported zero process bytes.
//! The governor was inert unless the user happened to be running
//! `memory=debug`.
//!
//! These tests install **no** subscriber, which is the condition under test.
//! They enter through `__test_sample_pane_retention_now`, the seam that runs
//! the real gated path — a test entering below the gate cannot see this defect,
//! which is why the existing charging tests all passed while it was live.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// With no subscriber installed, a pane's retention still reaches the ledger.
#[test]
fn a_pane_is_charged_when_no_memory_subscriber_is_installed() {
    // The condition under test: nothing has installed a `memory` subscriber,
    // so the gate inside the sampling pass is closed.
    assert!(
        !tracing::enabled!(target: "memory", tracing::Level::DEBUG),
        "this test is meaningless with a memory subscriber installed — it exists to \
         measure the shipped default, where the gate is closed"
    );

    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    app.__test_reconcile_pane_owners();

    let pane_ids = app.__test_child_pane_ids(child).expect("the child window exists");
    let pane_id = *pane_ids.first().expect("one pane");

    // The real production entry point, gate included.
    let logged = app.__test_sample_pane_retention_now();

    let measured = app.__test_pane_retention(child, pane_id).expect("an uncontended pane measures");
    let charges = app.__test_pane_charges(child, pane_id).expect("the pane holds charges");
    let charged_bytes: usize = charges.values().map(|amount| amount.bytes).sum();

    println!(
        "MEASURED with no subscriber:\n  \
         sampling reported logging = {logged}\n  \
         pane retention bytes      = {}\n  \
         pane charged bytes        = {charged_bytes}",
        measured.total().bytes
    );

    assert!(
        charged_bytes > 0,
        "the pane retains {} bytes and is charged {charged_bytes}. Charging sits behind \
         the `memory` log gate, so in a session with no subscriber — the shipped \
         default — nothing reaches the ledger and every governor limit has no figure to \
         apply itself to.",
        measured.total().bytes
    );
}

/// The charged figure agrees with what the pane measures, gate or no gate.
///
/// Agreement rather than magnitude: a charge that lands but disagrees with the
/// seam it came from sends an operator reading the ledger to the wrong
/// subsystem.
#[test]
fn the_charge_matches_the_measurement_with_no_subscriber() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one"]);
    app.__test_reconcile_pane_owners();

    let pane_ids = app.__test_child_pane_ids(child).expect("the child window exists");
    let pane_id = *pane_ids.first().expect("one pane");

    app.__test_sample_pane_retention_now();

    let measured = app.__test_pane_retention(child, pane_id).expect("an uncontended pane measures");
    let charges = app.__test_pane_charges(child, pane_id).expect("the pane holds charges");

    for (class, amount) in sonicterm_app::app::retention::seam_classes(&measured) {
        if amount.bytes == 0 {
            continue;
        }
        let charged = charges.get(&class).copied().unwrap_or_default();
        assert_eq!(
            charged.bytes, amount.bytes,
            "{class:?} measures {} bytes and is charged {} — the ledger and the seam \
             must agree",
            amount.bytes, charged.bytes
        );
    }
}
