//! Pins `wait_for`'s wakeups: an idle wait checks at most every [`WAIT_RECHECK`], and output
//! published by the VT worker, an exit the event loop records, and a busy parser that frees each
//! end a wait well before a lengthened recheck could. Waits here use a 10 s recheck, so a wake
//! that depended on the recheck instead of the pane's change signal misses its 5 s bound.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use regex::Regex;
use sonicterm_cfg::config::{Config, McpControl};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_types::PaneExitStatus;

use super::*;
use crate::app::mcp::test_entry;
use crate::app::App;

/// A recheck no wake in these tests may rely on.
const LONG_RECHECK: Duration = Duration::from_secs(10);
/// The bound a signalled wake meets with room to spare on a loaded runner.
const WAKE_BOUND: Duration = Duration::from_secs(5);

/// A call admitted now on `shared`, for a connection that stays open.
fn scope(shared: &McpShared) -> CallScope {
    CallScope::admit(shared, &Arc::new(AtomicBool::new(false)))
}

/// A regex wait for `pattern` bounded by 20 s, sleeping up to `recheck` between checks.
fn regex_wait(pattern: &str, recheck: Duration) -> WaitCondition {
    WaitCondition {
        regex: Some(Regex::new(pattern).unwrap()),
        after: None,
        quiet: None,
        watch_exit: false,
        timeout: Duration::from_secs(20),
        recheck,
    }
}

/// An App with one seeded pane, and its MCP entry as the loop resolves it.
fn app_pane() -> (App, u64, PaneEntry) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("wait");
    let entry = app.mcp_resolve_pane(pane_id).expect("the seeded pane resolves");
    (app, pane_id, entry)
}

/// Publish `bytes` through the production VT batch publisher of `pane_id`.
fn publish(handles: &crate::app::spawn_pane::PaneVtHandles, bytes: &[u8]) {
    crate::app::spawn_pane::publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut crate::app::spawn_pane::SyncLatch::default(),
        |_| None,
        |_| {},
        Instant::now,
        |_| {},
    );
}

/// The VT worker handles of `pane_id` in the App's main window.
fn worker(app: &App, pane_id: u64) -> crate::app::spawn_pane::PaneVtHandles {
    let window = app.registry.main_window_id.expect("main window");
    crate::app::spawn_pane::PaneVtHandles::from_pane_state(
        &app.registry.windows[&window].panes[&pane_id],
    )
    .with_pane_id(pane_id)
}

/// A wait running on its own thread: its result, and the events it reports as it runs.
struct SpawnedWait {
    handle: std::thread::JoinHandle<(WaitReason, Duration)>,
    events: std::sync::mpsc::Receiver<WaitEvent>,
}

impl SpawnedWait {
    /// Block until the wait reports `event`, so the test acts only once the wait is in that state.
    fn until(&self, event: WaitEvent) {
        loop {
            let seen = self.events.recv_timeout(WAKE_BOUND).expect("the wait reported its state");
            if seen == event {
                return;
            }
        }
    }

    /// The wait's reason and how long it ran from its first check.
    fn join(self) -> (WaitReason, Duration) {
        self.handle.join().unwrap()
    }
}

/// Run `condition` on another thread, reporting its `WaitEvent`s to the returned handle.
fn spawn_wait(entry: &PaneEntry, condition: WaitCondition) -> SpawnedWait {
    let (shared, entry) = (McpShared::new(McpControl::Agent, None), entry.clone());
    let (sender, events) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        WAIT_OBSERVER.with_borrow_mut(|observer| *observer = Some(sender));
        let started = Instant::now();
        let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
        (outcome.reason, started.elapsed())
    });
    SpawnedWait { handle, events }
}

/// An idle 1 s wait sleeps on the signal: it checks about once per 250 ms recheck, not once
/// per 50 ms poll as a polling wait would (about 20 checks).
#[test]
fn an_idle_wait_checks_at_most_every_recheck() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let mut condition = regex_wait("never", WAIT_RECHECK);
    condition.timeout = Duration::from_secs(1);
    WAIT_CHECKS.with(|checks| checks.set(0));
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    let checks = WAIT_CHECKS.with(std::cell::Cell::get);
    assert_eq!(outcome.reason, WaitReason::Timeout);
    // One check at the start, one per recheck, one at the timeout; scheduling may add one.
    assert!(checks <= 7, "an idle 1 s wait checked {checks} times");
}

/// Output the VT worker publishes wakes a sleeping waiter at once.
#[test]
fn published_output_wakes_a_sleeping_wait() {
    let (app, pane_id, entry) = app_pane();
    let waiter = spawn_wait(&entry, regex_wait("READY", LONG_RECHECK));
    // Published only once the wait has inspected and is sleeping, so only the signal can wake it.
    waiter.until(WaitEvent::Parked);
    publish(&worker(&app, pane_id), b"READY\r\n");
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Matched);
    assert!(elapsed < WAKE_BOUND, "the wait woke after {elapsed:?}");
}

/// Output published right after the wait's first look, before it sleeps, is not slept through.
#[test]
fn output_between_the_first_look_and_the_sleep_is_not_lost() {
    let (app, pane_id, entry) = app_pane();
    let handles = worker(&app, pane_id);
    let shared = McpShared::new(McpControl::Agent, None);
    let started = Instant::now();
    let outcome = wait_until_observed(
        &shared,
        &entry,
        &regex_wait("READY", LONG_RECHECK),
        &scope(&shared),
        || publish(&handles, b"READY\r\n"),
    );
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert!(started.elapsed() < WAKE_BOUND, "the wait slept through the output");
}

/// A process that exits without printing wakes an exit wait when the event loop records it.
#[test]
fn a_silent_exit_wakes_a_sleeping_wait() {
    let (mut app, pane_id, entry) = app_pane();
    let mut condition = regex_wait("never", LONG_RECHECK);
    condition.regex = None;
    condition.watch_exit = true;
    let waiter = spawn_wait(&entry, condition);
    waiter.until(WaitEvent::Parked);
    app.handle_pane_process_exited(pane_id, PaneExitStatus::Code(3));
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Exited);
    assert!(elapsed < WAKE_BOUND, "the wait woke after {elapsed:?}");
}

/// One published batch wakes every wait on the pane.
#[test]
fn every_wait_on_a_pane_wakes() {
    let (app, pane_id, entry) = app_pane();
    let waiters: Vec<_> =
        (0..3).map(|_| spawn_wait(&entry, regex_wait("READY", LONG_RECHECK))).collect();
    // Every wait is asleep before the one publication that must wake them all.
    for waiter in &waiters {
        waiter.until(WaitEvent::Parked);
    }
    publish(&worker(&app, pane_id), b"READY\r\n");
    for waiter in waiters {
        let (reason, elapsed) = waiter.join();
        assert_eq!(reason, WaitReason::Matched);
        assert!(elapsed < WAKE_BOUND, "a wait woke after {elapsed:?}");
    }
}

/// A wait that found the parser held retries on the short poll, not the recheck, so it matches
/// soon after the holder lets go even though nothing new is announced then.
#[test]
fn a_busy_parser_is_retried_on_the_short_poll() {
    let entry = test_entry(20, 4, 100);
    let held = Arc::clone(&entry.parser);
    let mut guard = held.lock();
    guard.advance(b"READY\r\n");
    entry.output_generation.fetch_add(1, Ordering::Release);
    entry.change_signal.notify();
    let waiter = spawn_wait(&entry, regex_wait("READY", LONG_RECHECK));
    // Released only once a check met the held parser, so the retry, not a first look, matches.
    waiter.until(WaitEvent::Busy);
    drop(guard);
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Matched);
    assert!(elapsed < WAKE_BOUND, "the busy wait recovered after {elapsed:?}");
}

/// Synthetic measurement: four idle 1 s waits, the most the server runs at once (4 connections,
/// one call each), check about 5 times each on the signal instead of 20 times on a 50 ms poll.
#[test]
fn four_concurrent_idle_waits_check_on_the_recheck_cadence() {
    let entry = test_entry(20, 4, 100);
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let entry = entry.clone();
            std::thread::spawn(move || {
                let shared = McpShared::new(McpControl::Agent, None);
                let mut condition = regex_wait("never", WAIT_RECHECK);
                condition.timeout = Duration::from_secs(1);
                WAIT_CHECKS.with(|checks| checks.set(0));
                wait_until(&shared, &entry, &condition, &scope(&shared));
                WAIT_CHECKS.with(std::cell::Cell::get)
            })
        })
        .collect();
    let total: u64 = waiters.into_iter().map(|waiter| waiter.join().unwrap()).sum();
    // A 50 ms poll would check about 4 × 21 = 84 times; the signal-driven wait about 4 × 6.
    assert!(total <= 28, "four idle waits checked {total} times in 1 s");
}
