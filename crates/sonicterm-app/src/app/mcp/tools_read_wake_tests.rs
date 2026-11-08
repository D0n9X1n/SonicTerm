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
    spawn_wait_on(&McpShared::new(McpControl::Agent, None), entry, condition)
}

/// [`spawn_wait`] on a given server, so a test can revoke the wait's call.
fn spawn_wait_on(
    shared: &Arc<McpShared>,
    entry: &PaneEntry,
    condition: WaitCondition,
) -> SpawnedWait {
    let (shared, entry) = (Arc::clone(shared), entry.clone());
    let (sender, events) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        // `Parked` comes from inside the signal's wait, with its mutex held, so a publication sent
        // after it must take that mutex after the waiter is registered and must notify it.
        let parked = sender.clone();
        crate::app::pane_signal::PARK_HOOK.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move || {
                let _ = parked.send(WaitEvent::Parked);
            }));
        });
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

/// Retiring a pane ends a wait sleeping on it with `pane_closed` at once, not at its timeout.
#[test]
fn closing_the_pane_ends_a_sleeping_wait() {
    let (mut app, pane_id, entry) = app_pane();
    let waiter = spawn_wait(&entry, regex_wait("never", LONG_RECHECK));
    waiter.until(WaitEvent::Parked);
    let window = app.registry.main_window_id.expect("main window");
    let pane = app.registry.windows.get_mut(&window).unwrap().panes.detach(pane_id).unwrap();
    app.retire_pane(pane);
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::PaneClosed);
    assert!(elapsed < WAKE_BOUND, "the wait ended after {elapsed:?}");
}

/// Output published before the close still matches: the close is reported only once the pane's
/// last output was scanned without a match.
#[test]
fn output_published_before_the_close_still_matches() {
    let entry = test_entry(20, 4, 100);
    entry.parser.lock().advance(b"READY\r\n");
    entry.output_generation.fetch_add(1, Ordering::Release);
    entry.change_signal.close();
    let shared = McpShared::new(McpControl::Agent, None);
    let outcome = wait_until(&shared, &entry, &regex_wait("READY", LONG_RECHECK), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// Stopping the server wakes a sleeping wait at once, so it reports `revoked` without waiting for
/// its recheck.
#[test]
fn stopping_the_server_wakes_a_sleeping_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let waiter = spawn_wait_on(&shared, &entry, regex_wait("never", LONG_RECHECK));
    waiter.until(WaitEvent::Parked);
    shared.stop();
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Revoked);
    assert!(elapsed < WAKE_BOUND, "the stopped wait ended after {elapsed:?}");
}

/// A finished wait leaves no registration behind, so later revocations wake nothing stale.
#[test]
fn a_finished_wait_unregisters() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let mut condition = regex_wait("never", LONG_RECHECK);
    condition.timeout = Duration::from_millis(20);
    wait_until(&shared, &entry, &condition, &scope(&shared));
    let before = entry.change_signal.sequence();
    shared.wake_waiters();
    assert_eq!(entry.change_signal.sequence(), before, "a finished wait was still registered");
}

/// The status line the app writes into a held pane after an unclean exit is published like VT
/// output, so a wait for its text matches instead of skipping a generation that never moved.
#[test]
fn the_exit_status_line_is_published_to_a_waiting_regex() {
    let (mut app, pane_id, entry) = app_pane();
    let waiter = spawn_wait(&entry, regex_wait(r"process exited with code 3", LONG_RECHECK));
    waiter.until(WaitEvent::Parked);
    app.handle_pane_process_exited(pane_id, PaneExitStatus::Code(3));
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Matched);
    assert!(elapsed < WAKE_BOUND, "the status line was seen after {elapsed:?}");
}

/// A close lands after more output than one scan may read: the wait keeps scanning the backlog
/// published before the close and reports the match beyond the first budget, not `pane_closed`.
#[test]
fn a_close_after_a_large_backlog_still_finds_a_late_match() {
    // 100 columns × 4,000 rows of scrollback, so 1,000 full rows exceed one 32,768-cell scan.
    let entry = test_entry(100, 4, 4_000);
    // Scanning starts after a marker row, so the forward scan, not a tail read, must reach the match.
    entry.parser.lock().advance(b"START\r\n");
    let mut condition = regex_wait("LATE", LONG_RECHECK);
    let start = read_pane(&entry.parser, ReadMode::Tail, ReadLimits::clamped(None, None));
    condition.after = Some(start.lines.last().expect("the marker row").row);
    let mut bytes = Vec::new();
    for row in 0..1_000 {
        bytes.extend_from_slice(format!("{row:0>99}\r\n").as_bytes());
    }
    bytes.extend_from_slice(b"LATE\r\n");
    entry.parser.lock().advance(&bytes);
    entry.output_generation.fetch_add(1, Ordering::Release);
    entry.change_signal.close();
    let shared = McpShared::new(McpControl::Agent, None);
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// Ending a connection wakes a wait its worker is running, so the call sees the close at once.
#[test]
fn closing_the_connection_wakes_its_running_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let closed = Arc::new(AtomicBool::new(false));
    let (sender, events) = std::sync::mpsc::channel();
    let handle = {
        let (shared, entry, closed) = (Arc::clone(&shared), entry.clone(), Arc::clone(&closed));
        std::thread::spawn(move || {
            let parked = sender.clone();
            crate::app::pane_signal::PARK_HOOK.with_borrow_mut(|hook| {
                *hook = Some(Box::new(move || {
                    let _ = parked.send(WaitEvent::Parked);
                }));
            });
            let started = Instant::now();
            let call = CallScope::admit(&shared, &closed);
            let outcome = wait_until(&shared, &entry, &regex_wait("never", LONG_RECHECK), &call);
            (outcome.reason, started.elapsed())
        })
    };
    let waiter = SpawnedWait { handle, events };
    waiter.until(WaitEvent::Parked);
    crate::app::mcp::close_connection(&shared, &closed);
    let (reason, elapsed) = waiter.join();
    assert_eq!(reason, WaitReason::Revoked);
    assert!(elapsed < WAKE_BOUND, "the closed connection's wait ended after {elapsed:?}");
}

/// Two waits on one pane register separately: when one ends, the other is still woken at once by
/// a revocation, rather than at its recheck.
#[test]
fn one_wait_ending_leaves_another_on_the_same_pane_registered() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let long = spawn_wait_on(&shared, &entry, regex_wait("never", LONG_RECHECK));
    long.until(WaitEvent::Parked);
    // A second wait on the same pane comes and goes.
    let mut short = regex_wait("never", LONG_RECHECK);
    short.timeout = Duration::from_millis(20);
    wait_until(&shared, &entry, &short, &scope(&shared));
    shared.stop();
    let (reason, elapsed) = long.join();
    assert_eq!(reason, WaitReason::Revoked);
    assert!(elapsed < WAKE_BOUND, "the remaining wait ended after {elapsed:?}");
}

/// Moving a pane's tab to another window keeps its signal open: a transfer is not a close.
#[test]
fn a_transferred_pane_keeps_its_signal_open() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["moving"]);
    let pane_id = app.__test_child_pane_ids(child).expect("the child window")[0];
    let signal = Arc::clone(&app.registry.windows[&child].panes[&pane_id].change_signal);
    app.__test_seed_tab("main");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 480.0),
        10.0,
        20.0,
    );
    app.transfer_tab(Some(child), 0, None, 1).expect("the tab moves to the main window");
    assert!(app.mcp_resolve_pane(pane_id).is_some(), "the pane lives on in the main window");
    assert!(!signal.is_closed(), "a transfer closed the pane's signal");
}

/// A wait that sees the pane closed while its parser stays busy reports `pane_closed` after the
/// bounded drain, with the rest marked unscanned, instead of holding until its timeout.
#[test]
fn a_closed_pane_with_a_busy_parser_ends_after_the_drain() {
    let entry = test_entry(20, 4, 100);
    entry.output_generation.fetch_add(1, Ordering::Release);
    entry.change_signal.close();
    let held = Arc::clone(&entry.parser);
    let (locked, ready) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = held.lock();
        locked.send(()).unwrap();
        std::thread::sleep(CLOSE_DRAIN + Duration::from_secs(2));
    });
    ready.recv().unwrap();
    let shared = McpShared::new(McpControl::Agent, None);
    let started = Instant::now();
    let outcome = wait_until(&shared, &entry, &regex_wait("never", LONG_RECHECK), &scope(&shared));
    let elapsed = started.elapsed();
    holder.join().unwrap();
    assert_eq!(outcome.reason, WaitReason::PaneClosed);
    assert!(!outcome.scan_complete, "rows the busy parser hid are reported unscanned");
    assert!(
        elapsed >= CLOSE_DRAIN && elapsed < CLOSE_DRAIN + Duration::from_secs(2),
        "{elapsed:?}"
    );
}

/// Output published and the pane closed between a check's `unscanned` computation and its close
/// decision are not lost: the close is acted on only from the next check, which scans that output
/// first, so the late match wins over `pane_closed`.
#[test]
fn a_close_racing_the_close_decision_still_scans_the_last_output() {
    let entry = test_entry(20, 4, 100);
    let racing = entry.clone();
    let fired = std::cell::Cell::new(false);
    CLOSE_CHECK_HOOK.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            if !fired.replace(true) {
                racing.parser.lock().advance(b"MATCH\r\n");
                crate::app::spawn_pane::publish_grid_change(
                    &racing.output_generation,
                    &racing.change_signal,
                );
                racing.change_signal.close();
            }
        }));
    });
    let shared = McpShared::new(McpControl::Agent, None);
    let outcome = wait_until(&shared, &entry, &regex_wait("MATCH", LONG_RECHECK), &scope(&shared));
    CLOSE_CHECK_HOOK.with_borrow_mut(|hook| *hook = None);
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// A wait without a regex inspects nothing, so it reports no inspected generation.
#[test]
fn a_regexless_wait_reports_no_inspected_generation() {
    let (mut app, pane_id, entry) = app_pane();
    entry.output_generation.fetch_add(1, Ordering::Release);
    let mut condition = regex_wait("never", LONG_RECHECK);
    condition.regex = None;
    condition.watch_exit = true;
    let waiter = spawn_wait(&entry, condition);
    waiter.until(WaitEvent::Parked);
    app.handle_pane_process_exited(pane_id, PaneExitStatus::Code(0));
    let (reason, _) = waiter.join();
    assert_eq!(reason, WaitReason::Exited);
    let exit_outcome_generation = {
        let shared = McpShared::new(McpControl::Agent, None);
        let mut exit_only = regex_wait("never", LONG_RECHECK);
        exit_only.regex = None;
        exit_only.watch_exit = true;
        wait_until(&shared, &entry, &exit_only, &scope(&shared)).inspected_generation
    };
    assert_eq!(exit_outcome_generation, None);
    // The clean exit closed that pane; a quiet-only wait runs on a fresh one.
    let fresh = test_entry(20, 4, 100);
    fresh.output_generation.fetch_add(1, Ordering::Release);
    let shared = McpShared::new(McpControl::Agent, None);
    let mut quiet = regex_wait("never", LONG_RECHECK);
    quiet.regex = None;
    quiet.quiet = Some(Duration::from_millis(20));
    let outcome = wait_until(&shared, &fresh, &quiet, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Quiet);
    assert_eq!(outcome.inspected_generation, None);
}
