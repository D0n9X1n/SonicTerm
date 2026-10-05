use std::sync::{mpsc, Arc};
use std::time::Duration;

use super::*;
use crate::app::App;

/// How long a test waits for a paused writer or another thread before failing an assertion.
const BOUNDED_WAIT: Duration = Duration::from_secs(5);

/// A new App whose gate is decided by `filter` alone, not by any process-wide subscriber.
fn app_under_filter(filter: &str) -> App {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::new(filter));
    sonicterm_logging::test_capture::with_default(subscriber, || {
        App::new(Theme::default(), Config::default(), Keymap::default())
    })
}

/// An App with its gate forced on and one seeded pane that carries real counter handles.
fn counting_app() -> (App, u64) {
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().expect("no window or pane yet");
    let pane_id = app.__test_seed_counting_tab("echo");
    (app, pane_id)
}

/// A target at row 0, column 2, for `a`, under the default identity.
fn target() -> EchoWatchTarget {
    EchoWatchTarget { abs_row: 0, col: 2, character: 'a', identity: EchoRowIdentity::default() }
}

/// The pane's echo watch, shared like the worker's clone.
fn watch_of(app: &App, pane_id: u64) -> Arc<EchoWatch> {
    let pane = app.find_pane(pane_id).expect("seeded pane");
    Arc::clone(&pane.frame_counters.as_ref().expect("counting pane").echo)
}

/// The token an arm must have issued.
fn armed(outcome: ArmOutcome) -> ArmToken {
    match outcome {
        ArmOutcome::Armed(token) => token,
        other => panic!("arm returned {other:?}"),
    }
}

/// The record a take must have returned.
fn traced(outcome: TakeOutcome) -> EchoTrace {
    match outcome {
        TakeOutcome::Trace(trace) => trace,
        other => panic!("take returned {other:?}"),
    }
}

/// Tokens start at 1, strictly increase, are never 0, and once the counter cannot advance an arm
/// reports `Exhausted` instead of reusing a token.
#[test]
fn tokens_are_nonzero_strictly_increasing_and_exhaust() {
    let (mut app, pane_id) = counting_app();
    let first = armed(app.arm_echo_watch(pane_id, target()));
    let second = armed(app.arm_echo_watch(pane_id, target()));
    let third = armed(app.arm_echo_watch(pane_id, target()));
    assert_eq!((first.get(), second.get(), third.get()), (1, 2, 3));
    app.__test_set_next_echo_arm(u64::MAX - 1);
    assert_eq!(armed(app.arm_echo_watch(pane_id, target())).get(), u64::MAX - 1);
    assert_eq!(app.arm_echo_watch(pane_id, target()), ArmOutcome::Exhausted);
    assert_eq!(app.arm_echo_watch(pane_id, target()), ArmOutcome::Exhausted, "stays exhausted");
}

/// A writer that loaded its token, paused while the harness took the record, and then wrote is
/// discarded: the take returned the pre-pause record, and the slot stays taken.
#[test]
fn a_writer_paused_across_a_take_is_discarded_and_the_trace_is_intact() {
    let (mut app, pane_id) = counting_app();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    let appeared_at = Instant::now();
    assert!(watch.record(token.get(), appeared_at, |trace| {
        trace.appearance =
            Some(EchoAppearance { generation: 4, parsed_at: appeared_at, sync_open: false });
    }));
    let (taken_tx, taken_rx) = mpsc::channel();
    let pause_watch = Arc::clone(&watch);
    pause_next_writer(move || {
        taken_tx.send(pause_watch.take(token)).expect("test receiver");
    });
    let applied = watch.record(token.get(), Instant::now(), |trace| trace.lost = true);
    let taken = taken_rx.recv_timeout(BOUNDED_WAIT).expect("the pause point ran");
    let SlotTake::Trace(trace) = taken else { panic!("take at the pause returned {taken:?}") };
    assert_eq!(trace.appearance.map(|appearance| appearance.generation), Some(4));
    assert!(!trace.lost, "the paused write is not in the record");
    assert!(!applied, "the paused write was discarded");
    assert_eq!(app.take_echo_watch(pane_id, token), TakeOutcome::AlreadyTaken);
}

/// A writer that loaded one token and wrote after a re-arm cannot match the new slot: the new
/// token differs, and the new record holds none of the stale write.
#[test]
fn a_writer_paused_across_a_rearm_is_discarded_and_the_tokens_differ() {
    let (mut app, pane_id) = counting_app();
    let first = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    let loaded = watch.armed_token();
    assert_eq!(loaded, first.get(), "the writer observed the first arming");
    let second = armed(app.arm_echo_watch(pane_id, target()));
    assert_ne!(first, second, "a re-arm issues a new token");
    assert!(!watch.record(loaded, Instant::now(), |trace| trace.pre_present = true));
    let trace = traced(app.take_echo_watch(pane_id, second));
    assert!(!trace.pre_present, "the stale writer's fact is not in the new record");
}

/// A fact stamped before the arm instant belongs to an earlier sample and is rejected; one
/// stamped after it is kept.
#[test]
fn a_timestamp_before_armed_at_is_rejected() {
    let (mut app, pane_id) = counting_app();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    let armed_at = watch.lock_slot().trace.expect("armed record").armed_at;
    let early = armed_at.checked_sub(Duration::from_millis(1)).expect("a representable instant");
    assert!(!watch.record(token.get(), early, |trace| trace.identity_changed = true));
    assert!(watch.record(token.get(), armed_at, |trace| trace.lost = true));
    let trace = traced(app.take_echo_watch(pane_id, token));
    assert!(!trace.identity_changed, "the early fact was rejected");
    assert!(trace.lost, "a fact at the arm instant is kept");
}

/// A take with another token reports `Mismatch` and leaves the slot armed, so the right token
/// still takes the intact record.
#[test]
fn a_wrong_token_take_is_mismatch_and_leaves_the_slot() {
    let (mut app, pane_id) = counting_app();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let wrong = ArmToken(token.get() + 100);
    assert_eq!(app.take_echo_watch(pane_id, wrong), TakeOutcome::Mismatch);
    assert_eq!(watch_of(&app, pane_id).armed_token(), token.get(), "still armed");
    assert_eq!(traced(app.take_echo_watch(pane_id, token)).target, target());
}

/// The first take returns the record; a second take of the same token reports `AlreadyTaken`
/// and changes nothing.
#[test]
fn a_second_take_is_already_taken_and_leaves_the_slot() {
    let (mut app, pane_id) = counting_app();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let trace = traced(app.take_echo_watch(pane_id, token));
    assert!(trace.shown, "the seeded pane is the main window's active pane");
    assert_eq!(app.take_echo_watch(pane_id, token), TakeOutcome::AlreadyTaken);
    assert_eq!(app.take_echo_watch(pane_id, token), TakeOutcome::AlreadyTaken);
}

/// Arming publishes the token to writers; taking stores 0, so writers pay one load again.
#[test]
fn take_disarms_the_watch() {
    let (mut app, pane_id) = counting_app();
    let watch = watch_of(&app, pane_id);
    assert_eq!(watch.armed_token(), 0, "an idle watch is unarmed");
    let token = armed(app.arm_echo_watch(pane_id, target()));
    assert_eq!(watch.armed_token(), token.get());
    traced(app.take_echo_watch(pane_id, token));
    assert_eq!(watch.armed_token(), 0);
}

/// Arm and take complete while another thread holds the pane's parser lock, so neither takes it.
#[test]
fn arm_and_take_take_no_parser_lock() {
    let (mut app, pane_id) = counting_app();
    let parser = Arc::clone(&app.find_pane(pane_id).expect("seeded pane").parser);
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        locked_tx.send(()).expect("test receiver");
        // A bounded hold: a failing test releases instead of hanging the run.
        let _ = release_rx.recv_timeout(BOUNDED_WAIT);
    });
    locked_rx.recv_timeout(BOUNDED_WAIT).expect("the holder took the parser lock");
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let trace = traced(app.take_echo_watch(pane_id, token));
    release_tx.send(()).expect("holder alive");
    holder.join().expect("holder thread");
    assert_eq!(trace.target, target());
}

/// With the gate off, arm and take report `GateOff`, and no pane gains counter handles or a watch.
#[test]
fn gate_off_arm_and_take_return_gate_off_and_allocate_nothing() {
    let mut app = app_under_filter("warn");
    assert!(app.frame_counters.is_none(), "precondition: the gate is off");
    let pane_id = app.__test_seed_counting_tab("off");
    assert_eq!(app.arm_echo_watch(pane_id, target()), ArmOutcome::GateOff);
    assert_eq!(app.take_echo_watch(pane_id, ArmToken(1)), TakeOutcome::GateOff);
    assert!(app.find_pane(pane_id).expect("seeded pane").frame_counters.is_none());
    assert_eq!(app.next_echo_arm, 1, "no token was issued");
}

/// A pane removed while its worker's handles survive keeps its watch alive through them; the App
/// can no longer reach it (`NoPane`), and the watch is freed when the last owner drops.
#[test]
fn a_removed_pane_keeps_its_slot_until_the_worker_handle_drops() {
    let (mut app, pane_id) = counting_app();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(
        app.find_pane(pane_id).expect("seeded pane"),
    );
    let weak = Arc::downgrade(&watch_of(&app, pane_id));
    let removed = app.main_mut().and_then(|main| main.panes.remove(&pane_id));
    drop(removed.expect("the pane was in the main window"));
    assert_eq!(app.take_echo_watch(pane_id, token), TakeOutcome::NoPane);
    assert!(weak.upgrade().is_some(), "the worker's handles still own the watch");
    drop(handles);
    assert!(weak.upgrade().is_none(), "the last owner dropped");
}

/// The per-pane watch stays within its fixed bound.
#[test]
fn the_echo_watch_size_bound_holds() {
    assert!(std::mem::size_of::<EchoWatch>() <= ECHO_WATCH_MAX_BYTES);
    assert!(std::mem::size_of::<EchoCache>() <= 64, "the worker's cache stays small");
}
