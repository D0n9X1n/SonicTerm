use std::{collections::VecDeque, time::Duration};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

use super::*;

/// A process identity with the given pid and start token.
fn identity(pid: u32, start: u64) -> ProcessIdentity {
    ProcessIdentity { pid, start }
}

/// A foreground process observation.
fn process(name: &str, privileged: bool) -> Option<ForegroundProcess> {
    Some(ForegroundProcess { name: name.into(), privileged })
}

/// A pane whose foreground source is `source` with a detached, unpublished exit flag.
fn pane_with(source: ProcessIdentity) -> (PaneState, PtyExitObserved) {
    let mut pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(20, 4)))), None);
    let exited = PtyExitObserved::detached();
    pane.test_foreground_source = Some((source, exited.clone()));
    (pane, exited)
}

/// Shared observation points of one test's probes.
#[derive(Clone, Default)]
struct Observed {
    notified: Arc<AtomicUsize>,
    clock_reads: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    batches: Arc<Mutex<Vec<Vec<ProcessIdentity>>>>,
}

impl Observed {
    // Ordering: notified loads Relaxed; tests read it after the worker's channel or join edges.
    fn notified(&self) -> usize {
        self.notified.load(Ordering::Relaxed)
    }

    // Ordering: live loads Relaxed; tests poll it until the worker's guard has dropped.
    fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

/// A sampler that records each batch and answers from `answers` in order, then `None`.
fn scripted(answers: Vec<Option<ForegroundProcess>>, observed: &Observed) -> Sampler {
    let answers = Mutex::new(VecDeque::from(answers));
    let batches = Arc::clone(&observed.batches);
    Arc::new(move |identities, _stop| {
        batches.lock().push(identities.to_vec());
        identities.iter().map(|_| answers.lock().pop_front().flatten()).collect()
    })
}

/// Spawn the worker on a plain test thread.
fn thread_spawner() -> Spawner {
    Box::new(|job| std::thread::Builder::new().name("fg-probe-test".into()).spawn(job).map(drop))
}

/// A spawner that never runs a job; tests drive batches with [`drive`] instead.
fn idle_spawner() -> Spawner {
    Box::new(|_job| Ok(()))
}

/// Probes with an injected sampler and spawner, a counting clock and a counting notifier.
// Ordering: notified and clock_reads add Relaxed; they are test counters.
fn probes(sampler: Sampler, spawner: Spawner, observed: &Observed) -> ForegroundProbes {
    let notified = Arc::clone(&observed.notified);
    let clock_reads = Arc::clone(&observed.clock_reads);
    ForegroundProbes::from_parts(ProbeParts {
        notify: Box::new(move || {
            notified.fetch_add(1, Ordering::Relaxed);
            true
        }),
        sampler,
        spawner,
        clock: Arc::new(move || {
            clock_reads.fetch_add(1, Ordering::Relaxed);
            Instant::now()
        }),
        live_workers: Arc::clone(&observed.live),
    })
}

/// Run one worker batch on the test thread.
fn drive(probes: &ForegroundProbes) -> bool {
    probe_batch(&probes.shared, &probes.sampler, probes.stats(), &probes.clock)
}

/// Poll `condition` until it holds, failing after five seconds.
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// An App whose main tab's pane samples `source` through injected `probes`.
fn app_with(
    probes: ForegroundProbes,
    source: ProcessIdentity,
) -> (App, WindowId, u64, PtyExitObserved) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let main = app.main_window_id.expect("seeded main window");
    app.fg_probes = Arc::new(probes);
    let exited = PtyExitObserved::detached();
    app.windows.get_mut(&main).unwrap().panes.get_mut(&pane_id).unwrap().test_foreground_source =
        Some((source, exited.clone()));
    (app, main, pane_id, exited)
}

/// Set demand for the app's pane, bypassing the TTL like a due Windows wake.
fn demand(app: &mut App, main: WindowId, pane_id: u64) -> Demand {
    let probes = Arc::clone(&app.fg_probes);
    let pane = app.windows.get_mut(&main).unwrap().panes.get_mut(&pane_id).unwrap();
    probes.request(pane_id, pane, WantOrigin::Frame, Instant::now(), true)
}

/// The cached observation of the app's pane.
fn cached(app: &App, main: WindowId, pane_id: u64) -> Option<ForegroundProcess> {
    app.windows[&main].panes[&pane_id]
        .fg_proc_cache
        .as_ref()
        .and_then(|(_, process)| process.clone())
}

/// A later write replaces an undrained result (newest wins) without a second event; a write
/// after a drain posts again.
#[test]
fn newest_result_wins_and_a_write_after_a_drain_posts_again() {
    let observed = Observed::default();
    let probes = probes(
        scripted(
            vec![process("first", false), process("second", false), process("third", false)],
            &observed,
        ),
        idle_spawner(),
        &observed,
    );
    let (mut pane, _) = pane_with(identity(10, 1));
    let now = Instant::now();
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, now, false), Demand::Queued);
    assert!(drive(&probes));
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, now, true), Demand::Queued);
    assert!(drive(&probes));
    assert_eq!(observed.notified(), 1, "an undelivered event is never doubled");

    let drained = probes.drain();
    assert_eq!(drained.results.len(), 1);
    assert_eq!(drained.results[0].1.observation, process("second", false), "newest wins");

    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, now, true), Demand::Queued);
    assert!(drive(&probes));
    assert_eq!(observed.notified(), 2, "a write after the drain posts again");
}

/// A pane retired while its sample is in flight gets no entry back, and a batch with no
/// surviving result neither sets `posted` nor sends an event.
#[test]
fn a_late_write_after_retirement_recreates_nothing_and_posts_nothing() {
    let observed = Observed::default();
    let retire: Arc<Mutex<Option<ProbeRegistration>>> = Arc::new(Mutex::new(None));
    let sampler: Sampler = {
        let retire = Arc::clone(&retire);
        Arc::new(move |identities, _stop| {
            // Retire the pane mid-probe, as closing it on the event loop would.
            drop(retire.lock().take());
            identities.iter().map(|_| process("vim", false)).collect()
        })
    };
    let probes = probes(sampler, idle_spawner(), &observed);
    let (mut pane, _) = pane_with(identity(10, 1));
    assert_eq!(
        probes.request(7, &mut pane, WantOrigin::Frame, Instant::now(), false),
        Demand::Queued
    );
    *retire.lock() = pane.fg_probe.take();

    assert!(drive(&probes));

    let map = probes.shared.map.lock();
    assert!(map.entries.is_empty(), "the late result recreated no entry");
    assert!(!map.posted, "an all-dropped batch sets no posted");
    drop(map);
    assert_eq!(observed.notified(), 0, "an all-dropped batch sends no event");
}

/// The drain drops, and counts as stale, a result whose token no longer matches the pane's
/// registration, one for a pane whose child exited, and one for a pane that closed.
#[test]
fn drain_drops_mismatched_exited_and_closed_results_as_stale() {
    for case in ["token", "exited", "closed"] {
        let observed = Observed::default();
        let probes =
            probes(scripted(vec![process("gsudo", true)], &observed), idle_spawner(), &observed);
        let (mut app, main, pane_id, exited) = app_with(probes, identity(10, 1));
        let stats = Arc::new(ForegroundWorkerStats::default());
        app.fg_probes.seal_stats(Some(Arc::clone(&stats)));
        assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
        assert!(drive(&app.fg_probes));
        let window = app.windows.get_mut(&main).unwrap();
        match case {
            "token" => {
                window.panes.get_mut(&pane_id).unwrap().fg_probe.as_mut().unwrap().identity =
                    identity(10, 2);
            }
            "exited" => exited.publish_for_test(),
            _ => {
                // Keep the registration so the stored result outlives the pane, as a pane closed
                // between the worker's write and the drain leaves it.
                let mut closed = window.panes.remove(&pane_id).unwrap();
                std::mem::forget(closed.fg_probe.take());
            }
        }

        app.drain_foreground_probe_results(Instant::now());

        assert_eq!(stats.stale.load(Ordering::Relaxed), 1, "{case}");
        if case != "closed" {
            assert_eq!(cached(&app, main, pane_id), None, "{case}: the cache is unchanged");
            assert!(!app.windows[&main].tabs.tabs()[0].foreground_privileged, "{case}");
        }
    }
}

/// A demand is rate-limited by the newer of the sample and the last demand; a sample is
/// demanded again only once it is a TTL old.
#[test]
fn success_then_ttl_expiry_demands_again_and_demand_is_rate_limited() {
    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
    let (mut pane, _) = pane_with(identity(10, 1));
    let start = Instant::now();
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, start, false), Demand::Queued);
    let soon = start + Duration::from_millis(100);
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, soon, false), Demand::Fresh);

    let sampled = start + Duration::from_millis(10);
    pane.fg_proc_cache = Some((sampled, process("zsh", false)));
    let inside = sampled + FOREGROUND_PROCESS_TTL - Duration::from_millis(1);
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, inside, false), Demand::Fresh);
    let expired = sampled + FOREGROUND_PROCESS_TTL;
    assert_eq!(probes.request(7, &mut pane, WantOrigin::Frame, expired, false), Demand::Queued);
}

/// The drain applies an accepted result, and repaints chrome for a name-only change but not
/// for an unchanged sample.
#[test]
fn drain_repaints_chrome_for_a_name_change_and_not_for_an_unchanged_sample() {
    let observed = Observed::default();
    let probes = probes(
        scripted(vec![process("vim", false), process("vim", false)], &observed),
        idle_spawner(),
        &observed,
    );
    let (mut app, main, pane_id, _) = app_with(probes, identity(10, 1));
    app.windows.get_mut(&main).unwrap().panes.get_mut(&pane_id).unwrap().fg_proc_cache =
        Some((Instant::now(), process("zsh", false)));

    for (expected_repaint, label) in [(true, "name change"), (false, "unchanged")] {
        assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
        assert!(drive(&app.fg_probes));
        let before = app.windows[&main].redraw.snapshot();
        app.drain_foreground_probe_results(Instant::now());
        let after = app.windows[&main].redraw.snapshot();
        assert_eq!(cached(&app, main, pane_id), process("vim", false), "{label}");
        assert_eq!(after != before, expected_repaint, "{label}");
    }
}

/// An accepted privileged sample sets the tab's warning.
#[test]
fn drain_applies_a_privileged_sample_to_the_tab_warning() {
    let observed = Observed::default();
    let probes =
        probes(scripted(vec![process("gsudo", true)], &observed), idle_spawner(), &observed);
    let (mut app, main, pane_id, _) = app_with(probes, identity(10, 1));
    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    assert!(drive(&app.fg_probes));

    app.drain_foreground_probe_results(Instant::now());

    assert_eq!(cached(&app, main, pane_id), process("gsudo", true));
    assert!(app.windows[&main].tabs.tabs()[0].foreground_privileged);
}

/// Death while a ready event is undelivered sends no second event; one drain applies the
/// stored result and then enters `Unavailable`, once.
#[test]
fn death_with_an_undelivered_event_posts_nothing_more_and_one_drain_applies_then_stops() {
    let observed = Observed::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let sampler: Sampler = {
        let calls = Arc::clone(&calls);
        // Ordering: calls adds Relaxed; it only numbers this test sampler's batches.
        Arc::new(move |identities, _stop| {
            assert_eq!(calls.fetch_add(1, Ordering::Relaxed), 0, "injected worker death");
            identities.iter().map(|_| process("vim", false)).collect()
        })
    };
    let (mut app, main, pane_id, _) =
        app_with(probes(sampler, thread_spawner(), &observed), identity(10, 1));
    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    app.fg_probes.wake();
    wait_until("the first ready event", || observed.notified() == 1);
    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    app.fg_probes.wake();
    wait_until("the worker to die", || observed.live() == 0);
    assert_eq!(observed.notified(), 1, "death never queues a second undelivered event");

    app.drain_foreground_probe_results(Instant::now());

    assert_eq!(
        cached(&app, main, pane_id),
        process("vim", false),
        "results apply before Unavailable"
    );
    assert!(app.fg_probes.is_unavailable());
    assert!(app.fg_probes.shared.map.lock().entries.is_empty());
    app.drain_foreground_probe_results(Instant::now());
    assert!(app.fg_probes.is_unavailable(), "a second drain changes nothing");
}

/// Death after its last event was drained posts exactly one new event; the drain it
/// triggers enters `Unavailable`.
#[test]
fn death_after_a_drain_posts_exactly_one_event_then_stops() {
    let observed = Observed::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let sampler: Sampler = {
        let calls = Arc::clone(&calls);
        // Ordering: calls adds Relaxed; it only numbers this test sampler's batches.
        Arc::new(move |identities, _stop| {
            assert_eq!(calls.fetch_add(1, Ordering::Relaxed), 0, "injected worker death");
            identities.iter().map(|_| process("vim", false)).collect()
        })
    };
    let (mut app, main, pane_id, _) =
        app_with(probes(sampler, thread_spawner(), &observed), identity(10, 1));
    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    app.fg_probes.wake();
    wait_until("the first ready event", || observed.notified() == 1);
    app.drain_foreground_probe_results(Instant::now());
    assert!(!app.fg_probes.is_unavailable());

    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    app.fg_probes.wake();
    wait_until("the worker to die", || observed.live() == 0);
    assert_eq!(observed.notified(), 2, "death posts one event");
    app.drain_foreground_probe_results(Instant::now());
    assert!(app.fg_probes.is_unavailable());
}

/// Dropping the probes with a wake still buffered exits the worker without probing or
/// posting; the live count goes 1 -> 0.
#[test]
fn shutdown_with_a_buffered_wake_never_probes() {
    let observed = Observed::default();
    let (gate_tx, gate_rx) = crossbeam_channel::bounded::<()>(1);
    let spawner: Spawner = Box::new(move |job| {
        let gate = gate_rx.clone();
        std::thread::Builder::new()
            .name("fg-probe-test".into())
            .spawn(move || {
                // Hold the job until the test has dropped the probes.
                let _ = gate.recv();
                job();
            })
            .map(drop)
    });
    let probes = probes(scripted(vec![process("vim", false)], &observed), spawner, &observed);
    let (mut pane, _) = pane_with(identity(10, 1));
    assert_eq!(
        probes.request(7, &mut pane, WantOrigin::Frame, Instant::now(), false),
        Demand::Queued
    );
    probes.wake();
    assert_eq!(observed.live(), 1);

    drop(probes);
    gate_tx.send(()).unwrap();

    wait_until("the worker to exit", || observed.live() == 0);
    assert!(observed.batches.lock().is_empty(), "no probe ran after shutdown");
    assert_eq!(observed.notified(), 0, "no event after shutdown");
}

/// A failed spawn leaves no live worker, enters `Unavailable`, and later demand resolves
/// synchronously.
#[test]
fn a_failed_spawn_counts_no_worker_and_resolves_demand_synchronously() {
    let observed = Observed::default();
    let spawner: Spawner = Box::new(|_job| Err(std::io::Error::other("refused")));
    let probes = probes(scripted(Vec::new(), &observed), spawner, &observed);
    let (mut pane, _) = pane_with(identity(10, 1));
    assert_eq!(
        probes.request(7, &mut pane, WantOrigin::Frame, Instant::now(), false),
        Demand::Queued
    );

    probes.wake();

    assert_eq!(observed.live(), 0);
    assert!(probes.is_unavailable());
    assert!(probes.shared.map.lock().entries.is_empty());
    let later = Instant::now() + FOREGROUND_PROCESS_TTL;
    assert!(matches!(
        probes.request(7, &mut pane, WantOrigin::Frame, later, false),
        Demand::Resolved { .. }
    ));
}

/// Any child exit, clean or not, clears the cached name and warning and removes the entry;
/// an unclean exit keeps the pane, and further demand changes nothing.
#[test]
fn an_exit_clears_name_warning_and_entry_but_keeps_an_unclean_pane() {
    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
    let (mut app, main, pane_id, exited) = app_with(probes, identity(10, 1));
    assert_eq!(demand(&mut app, main, pane_id), Demand::Queued);
    let window = app.windows.get_mut(&main).unwrap();
    window.panes.get_mut(&pane_id).unwrap().fg_proc_cache =
        Some((Instant::now(), process("gsudo", true)));
    assert!(window.tabs.set_foreground_privileged(0, true));
    exited.publish_for_test();

    app.handle_pane_process_exited(pane_id, Some(false));

    let pane = &app.windows[&main].panes[&pane_id];
    assert_eq!(cached(&app, main, pane_id), None);
    assert!(pane.fg_probe.is_none());
    assert!(!app.windows[&main].tabs.tabs()[0].foreground_privileged);
    assert!(app.fg_probes.shared.map.lock().entries.is_empty());
    assert_eq!(demand(&mut app, main, pane_id), Demand::Fresh, "an exited pane demands nothing");
}

/// A pane with no identity caches `(now, None)` synchronously and starts no worker.
#[test]
fn a_pane_without_identity_resolves_without_a_worker() {
    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), thread_spawner(), &observed);
    let mut pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(20, 4)))), None);
    let now = Instant::now();
    pane.fg_proc_cache = Some((now - FOREGROUND_PROCESS_TTL, process("zsh", false)));

    let demanded = probes.request(7, &mut pane, WantOrigin::Frame, now, false);

    assert_eq!(demanded, Demand::Resolved { changed: true });
    assert_eq!(pane.fg_proc_cache, Some((now, None)));
    assert_eq!(observed.live(), 0, "no worker for a pane that cannot be sampled");
}

/// `Unavailable` is entered once: entries go, demand resolves synchronously, and entering it
/// again changes nothing.
#[test]
fn unavailable_clears_the_map_and_resolves_demand_synchronously() {
    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
    let (mut pane, _) = pane_with(identity(10, 1));
    assert_eq!(
        probes.request(7, &mut pane, WantOrigin::Frame, Instant::now(), false),
        Demand::Queued
    );

    probes.enter_unavailable("test");
    probes.enter_unavailable("test again");

    assert!(probes.shared.map.lock().entries.is_empty());
    let later = Instant::now() + FOREGROUND_PROCESS_TTL;
    assert_eq!(
        probes.request(7, &mut pane, WantOrigin::Frame, later, false),
        Demand::Resolved { changed: false }
    );
}

/// Pane churn while the probe is blocked never grows the map past the live panes, and late
/// results for closed panes recreate nothing.
#[test]
fn churn_while_the_probe_is_blocked_keeps_the_map_within_live_panes() {
    let observed = Observed::default();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded::<()>(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(1);
    let sampler: Sampler = Arc::new(move |identities, _stop| {
        let _ = entered_tx.try_send(());
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
        identities.iter().map(|_| process("vim", false)).collect()
    });
    let probes = probes(sampler, thread_spawner(), &observed);
    let now = Instant::now();
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    for pane_id in 1..=3 {
        let (mut pane, _) = pane_with(identity(pane_id as u32 + 100, 1));
        assert_eq!(
            probes.request(pane_id, &mut pane, WantOrigin::Frame, now, false),
            Demand::Queued
        );
        panes.insert(pane_id, pane);
    }
    probes.wake();
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the probe started");

    for pane_id in [1, 2] {
        drop(panes.remove(&pane_id));
        assert!(probes.shared.map.lock().entries.len() <= panes.len());
    }
    for pane_id in 4..=6 {
        let (mut pane, _) = pane_with(identity(pane_id as u32 + 100, 1));
        assert_eq!(
            probes.request(pane_id, &mut pane, WantOrigin::Frame, now, false),
            Demand::Queued
        );
        panes.insert(pane_id, pane);
        assert!(probes.shared.map.lock().entries.len() <= panes.len());
    }
    release_tx.send(()).unwrap();
    wait_until("the blocked batch to post", || observed.notified() == 1);

    let map = probes.shared.map.lock();
    assert!(map.entries.len() <= panes.len());
    assert!(
        map.entries.keys().all(|pane_id| panes.contains_key(pane_id)),
        "no closed pane returned"
    );
    drop(map);
    drop(probes);
    wait_until("the worker to exit", || observed.live() == 0);
}

/// With the counter gate off the worker reads the clock once per probed pane; with it on,
/// once more for the batch start, and records the batch.
// Ordering: clock_reads loads Relaxed; the batch runs on this thread.
#[test]
fn the_counter_gate_costs_one_clock_read_per_batch() {
    for gate_on in [false, true] {
        let observed = Observed::default();
        let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
        let stats = Arc::new(ForegroundWorkerStats::default());
        probes.seal_stats(gate_on.then(|| Arc::clone(&stats)));
        let mut panes = Vec::new();
        for pane_id in 1..=3 {
            let (mut pane, _) = pane_with(identity(pane_id as u32, 1));
            assert_eq!(
                probes.request(pane_id, &mut pane, WantOrigin::Frame, Instant::now(), false),
                Demand::Queued
            );
            panes.push(pane);
        }

        assert!(drive(&probes));

        assert_eq!(observed.clock_reads.load(Ordering::Relaxed), 3 + usize::from(gate_on));
        let recorded = (stats.probes.load(Ordering::Relaxed), stats.panes.load(Ordering::Relaxed));
        assert_eq!(recorded, if gate_on { (1, 3) } else { (0, 0) });
    }
}

/// Due wakes are consumed: a due warning wake merges with `now + TTL`, a due activity wake is
/// taken, a future wake survives, and afterwards every stored wake is absent or strictly future.
#[test]
fn due_wakes_are_consumed_and_future_wakes_survive() {
    let start = Instant::now();
    let due = start + Duration::from_millis(500);
    let assert_future = |schedule: &ForegroundSchedule, now: Instant| {
        assert!(schedule.activity_wake.is_none_or(|wake| wake.due > now));
        assert!(schedule.warning_wake.is_none_or(|wake| wake > now));
    };

    let mut schedule = ForegroundSchedule { activity_wake: None, warning_wake: Some(due) };
    assert_eq!(schedule.take_due(due, true, false), Some(WantOrigin::Warning));
    assert!(!schedule.settle_warning(due, true, false));
    assert_eq!(schedule.warning_wake, Some(start + Duration::from_millis(1000)));
    assert_future(&schedule, due);

    let mut schedule = ForegroundSchedule {
        activity_wake: Some(PendingForegroundProbe { due, fixed: true }),
        warning_wake: None,
    };
    assert_eq!(schedule.take_due(due, false, false), Some(WantOrigin::Activity));
    assert!(schedule.activity_wake.is_none());
    assert_future(&schedule, due);

    let later = start + Duration::from_millis(800);
    let mut schedule = ForegroundSchedule {
        activity_wake: Some(PendingForegroundProbe { due: later, fixed: false }),
        warning_wake: Some(due),
    };
    assert_eq!(schedule.take_due(due, true, false), Some(WantOrigin::Warning));
    assert_eq!(schedule.activity_wake.map(|wake| wake.due), Some(later), "a future wake survives");
    assert!(!schedule.settle_warning(due, true, false));
    assert_future(&schedule, due);
}

/// Simultaneous expiry yields one `Activity` demand, which one worker batch serves.
#[test]
fn simultaneous_expiry_sets_one_activity_demand_and_one_batch() {
    let due = Instant::now() + Duration::from_millis(500);
    let mut schedule = ForegroundSchedule {
        activity_wake: Some(PendingForegroundProbe { due, fixed: true }),
        warning_wake: Some(due),
    };
    let origin = schedule.take_due(due, true, false);
    assert_eq!(origin, Some(WantOrigin::Activity));
    assert_eq!(schedule, ForegroundSchedule::default());

    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
    let mut panes = Vec::new();
    for pane_id in 1..=2 {
        let (mut pane, _) = pane_with(identity(pane_id as u32, 1));
        assert_eq!(probes.request(pane_id, &mut pane, origin.unwrap(), due, true), Demand::Queued);
        panes.push(pane);
    }
    assert!(drive(&probes));
    assert!(drive(&probes));
    let batches = observed.batches.lock();
    assert_eq!(batches.len(), 1, "one batch serves both panes");
    assert_eq!(batches[0].len(), 2);
}

/// A taken warning wake demands only while a warning is shown in a non-elevated process, and
/// an elevated process re-arms nothing.
#[test]
fn a_warning_wake_revalidates_and_privilege_stops_rearming() {
    let due = Instant::now();
    for (warning_active, privileged) in [(false, false), (true, true)] {
        let mut schedule = ForegroundSchedule { activity_wake: None, warning_wake: Some(due) };
        assert_eq!(schedule.take_due(due, warning_active, privileged), None);
        assert!(schedule.settle_warning(due, warning_active, privileged));
        assert_eq!(schedule.warning_wake, None);
    }
    let mut schedule = ForegroundSchedule::default();
    schedule.arm_after_input(due, true);
    schedule.arm_after_output(due, true);
    assert_eq!(schedule.activity_wake, None, "an elevated process samples nothing");
}

/// Clearing a warning before the take leaves no warning-only batch; frame and activity demand
/// are kept, and so is the activity wake.
#[test]
fn a_take_after_the_clear_finds_no_warning_only_demand() {
    let observed = Observed::default();
    let probes = probes(scripted(Vec::new(), &observed), idle_spawner(), &observed);
    let now = Instant::now();
    let mut panes = Vec::new();
    for (pane_id, origin) in
        [(1, WantOrigin::Warning), (2, WantOrigin::Frame), (3, WantOrigin::Activity)]
    {
        let (mut pane, _) = pane_with(identity(pane_id as u32, 1));
        assert_eq!(probes.request(pane_id, &mut pane, origin, now, true), Demand::Queued);
        panes.push(pane);
    }
    let mut schedule = ForegroundSchedule {
        activity_wake: Some(PendingForegroundProbe {
            due: now + FOREGROUND_PROCESS_TTL,
            fixed: true,
        }),
        warning_wake: Some(now + FOREGROUND_PROCESS_TTL),
    };

    assert!(schedule.settle_warning(now, false, false));
    probes.clear_warning_wants();
    assert!(drive(&probes));

    let batches = observed.batches.lock();
    let pids: Vec<u32> = batches[0].iter().map(|identity| identity.pid).collect();
    assert_eq!(batches.len(), 1);
    assert!(!pids.contains(&1), "the warning-only demand was dropped");
    assert!(pids.contains(&2) && pids.contains(&3), "frame and activity demand stay");
    assert!(schedule.activity_wake.is_some(), "the activity wake survives the clear");
}

/// A batch taken before the clear may still probe and complete after it: that one batch only.
/// The pause sits at the seam between the take and the native probe, so the clear lands after
/// the demand was taken but before any sampling. The cleared deadline never fires again, and
/// an arm made during the pause survives.
#[test]
fn a_batch_taken_before_the_clear_is_the_only_one_after_it() {
    let observed = Observed::default();
    let probes =
        probes(scripted(vec![process("gsudo", true)], &observed), idle_spawner(), &observed);
    let (entered_tx, entered_rx) = crossbeam_channel::bounded::<usize>(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(1);
    let batches = Arc::clone(&observed.batches);
    assert!(probes
        .shared
        .after_take
        .set(Box::new(move || {
            // Report how many native probes ran before this point, then wait for the clear.
            let _ = entered_tx.try_send(batches.lock().len());
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        }))
        .is_ok());
    let now = Instant::now();
    let (mut pane, _) = pane_with(identity(1, 1));
    assert_eq!(probes.request(1, &mut pane, WantOrigin::Warning, now, true), Demand::Queued);
    let mut schedule = ForegroundSchedule { activity_wake: None, warning_wake: Some(now) };
    assert_eq!(schedule.take_due(now, true, false), Some(WantOrigin::Warning));

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| drive(&probes));
        let sampled = entered_rx.recv_timeout(Duration::from_secs(5)).expect("the batch was taken");
        assert_eq!(sampled, 0, "the pause precedes the native probe");
        // The clear's linearization point falls between the take and the probe.
        assert!(schedule.settle_warning(now, false, false));
        probes.clear_warning_wants();
        schedule.arm_after_input(now, false);
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap());
    });

    assert_eq!(observed.batches.lock().len(), 1, "the taken batch still probes, once");
    assert_eq!(observed.notified(), 1, "the taken batch completes and posts once");
    assert_eq!(schedule.warning_wake, None, "the cleared deadline is gone");
    assert!(schedule.activity_wake.is_some(), "an arm made during the pause survives");
    assert!(drive(&probes));
    assert_eq!(observed.batches.lock().len(), 1, "no warning-only batch after the clear");
}

/// A spawner that keeps each job unrun, so the wake channel stays open while tests drive batches.
fn holding_spawner(held: &Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>) -> Spawner {
    let held = Arc::clone(held);
    Box::new(move |job| {
        held.lock().push(job);
        Ok(())
    })
}

/// An App with two tabs whose active panes sample identities 10 and 20 through `probes`;
/// tab 0 shows a gsudo warning from a fresh cached sample.
fn two_tab_app(probes: ForegroundProbes) -> (App, WindowId, [u64; 2]) {
    let (mut app, main, first, _) = app_with(probes, identity(10, 1));
    let second = app.__test_seed_tab("second");
    let window = app.windows.get_mut(&main).unwrap();
    window.panes.get_mut(&second).unwrap().test_foreground_source =
        Some((identity(20, 1), PtyExitObserved::detached()));
    window.panes.get_mut(&first).unwrap().fg_proc_cache =
        Some((Instant::now(), process("gsudo", true)));
    let first_tab = window.tab_states.iter().position(|tab| tab.active_pane == first).unwrap();
    assert!(window.tabs.set_foreground_privileged(first_tab, true));
    (app, main, [first, second])
}

/// Activity and warning wakes expiring together through the real due adapter set one demand
/// per tab's active pane and wake the worker once; the drain applies both results, clears the
/// warning, and leaves no wake stored.
#[test]
fn the_due_adapter_serves_simultaneous_expiry_with_one_batch_and_the_drain_settles() {
    let observed = Observed::default();
    let held = Arc::new(Mutex::new(Vec::new()));
    let probes = probes(
        scripted(vec![process("pwsh", false), process("pwsh", false)], &observed),
        holding_spawner(&held),
        &observed,
    );
    let (mut app, main, _) = two_tab_app(probes);
    let due = Instant::now();
    app.foreground_schedule = ForegroundSchedule {
        activity_wake: Some(PendingForegroundProbe { due, fixed: true }),
        warning_wake: Some(due),
    };

    let _ = app.refresh_foreground_privileges_if_due(due);

    assert_eq!(held.lock().len(), 1, "one worker for the whole due demand");
    assert!(app.foreground_schedule.activity_wake.is_none(), "the due activity wake was consumed");
    assert!(app.foreground_schedule.warning_wake.is_none_or(|wake| wake > due));
    assert!(drive(&app.fg_probes));
    assert!(drive(&app.fg_probes));
    let batches = observed.batches.lock().clone();
    assert_eq!(batches.len(), 1, "simultaneous expiry is one batch");
    let mut pids: Vec<u32> = batches[0].iter().map(|identity| identity.pid).collect();
    pids.sort_unstable();
    assert_eq!(pids, [10, 20]);

    app.drain_foreground_probe_results(due);

    assert!(app.windows[&main].tabs.tabs().iter().all(|tab| !tab.foreground_privileged));
    assert_eq!(app.foreground_schedule, ForegroundSchedule::default(), "no warning, no wake");
}

/// A warning wake expiring as a drain clears the warning: the due adapter sets warning demand,
/// the drain of an earlier result clears the warning, and the warning-only demand is dropped
/// before the worker takes it while a pending frame demand survives.
#[test]
fn a_warning_cleared_by_the_drain_at_its_expiry_drops_only_warning_demand() {
    let observed = Observed::default();
    let held = Arc::new(Mutex::new(Vec::new()));
    let probes = probes(
        scripted(vec![process("pwsh", false)], &observed),
        holding_spawner(&held),
        &observed,
    );
    let (mut app, main, [first, second]) = two_tab_app(probes);
    // An earlier batch for the first pane has stored its result, still undrained.
    assert_eq!(demand(&mut app, main, first), Demand::Queued);
    assert!(drive(&app.fg_probes));
    // The second pane has frame demand pending.
    assert_eq!(demand(&mut app, main, second), Demand::Queued);
    let due = Instant::now();
    app.foreground_schedule = ForegroundSchedule { activity_wake: None, warning_wake: Some(due) };

    let _ = app.refresh_foreground_privileges_if_due(due);
    app.drain_foreground_probe_results(due);

    assert!(app.windows[&main].tabs.tabs().iter().all(|tab| !tab.foreground_privileged));
    assert_eq!(app.foreground_schedule.warning_wake, None, "the warning wake is cleared");
    assert!(drive(&app.fg_probes));
    let batches = observed.batches.lock().clone();
    assert_eq!(batches.len(), 2);
    let pids: Vec<u32> = batches[1].iter().map(|identity| identity.pid).collect();
    assert_eq!(pids, [20], "the warning-only demand was dropped; the frame demand stayed");
}

/// `Warning` never replaces a pending origin, and any other origin replaces `Warning`.
#[test]
fn warning_demand_never_replaces_another_origin() {
    assert_eq!(WantOrigin::merge(Some(WantOrigin::Frame), WantOrigin::Warning), WantOrigin::Frame);
    assert_eq!(
        WantOrigin::merge(Some(WantOrigin::Activity), WantOrigin::Warning),
        WantOrigin::Activity
    );
    assert_eq!(WantOrigin::merge(Some(WantOrigin::Warning), WantOrigin::Frame), WantOrigin::Frame);
    assert_eq!(WantOrigin::merge(None, WantOrigin::Warning), WantOrigin::Warning);
}

/// No frame path or timer walks the process table: the probe and `pid()` appear only in
/// the worker's platform sampler.
#[test]
fn no_event_loop_path_probes_the_foreground_process() {
    for (name, source) in [
        ("privilege.rs", include_str!("privilege.rs")),
        ("tab_state.rs", include_str!("tab_state.rs")),
        ("window_event.rs", include_str!("window_event.rs")),
        ("child_window_redraw.rs", include_str!("child_window_redraw.rs")),
        ("event_loop/windows.rs", include_str!("event_loop/windows.rs")),
    ] {
        let source = source.replace("\r\n", "\n");
        for probe in ["foreground_process_info", "foreground_processes_info"] {
            assert!(!source.contains(probe), "{name} probes on the event loop");
        }
        assert!(!source.contains(".pid()"), "{name} reads a pid on the frame path");
    }
    let frame_counters = include_str!("frame_counters.rs");
    assert!(!frame_counters.contains("fn time_probe"), "the event-loop probe timer is deleted");
}

/// The App's real foreground demand progression, through the injectable sampler, spawner and clock:
/// with frames demanding every 16 ms, a new sample is taken and applied once per
/// `FOREGROUND_PROCESS_TTL`, each with a later `sampled_at`, and never sooner. The perf harness's settle
/// barrier relies on this cadence to see two applied samples after the final process.
#[test]
fn frame_demand_keeps_sampling_the_foreground_every_ttl_while_frames_continue() {
    let observed = Observed::default();
    let virtual_now = Arc::new(Mutex::new(Instant::now()));
    let clock_now = Arc::clone(&virtual_now);
    let answers: Vec<Option<ForegroundProcess>> =
        (0..64).map(|_| process("sleep", false)).collect();
    let probes = ForegroundProbes::from_parts(ProbeParts {
        notify: Box::new(|| true),
        sampler: scripted(answers, &observed),
        spawner: idle_spawner(),
        clock: Arc::new(move || *clock_now.lock()),
        live_workers: Arc::clone(&observed.live),
    });
    let (mut app, main, pane_id, _) = app_with(probes, identity(10, 1));
    let start = *virtual_now.lock();
    let mut applied: Vec<Instant> = Vec::new();
    for frame in 0..200_u64 {
        let now = start + Duration::from_millis(16 * frame);
        *virtual_now.lock() = now;
        let probes = Arc::clone(&app.fg_probes);
        let pane = app.windows.get_mut(&main).unwrap().panes.get_mut(&pane_id).unwrap();
        if probes.request(pane_id, pane, WantOrigin::Frame, now, false) == Demand::Queued {
            // When: the frame's demand is queued, the worker batch runs and the event loop applies it.
            assert!(drive(&app.fg_probes));
            app.drain_foreground_probe_results(now);
        }
        let sampled_at =
            app.windows[&main].panes[&pane_id].fg_proc_cache.as_ref().map(|entry| entry.0);
        if let Some(sampled_at) = sampled_at.filter(|at| applied.last() != Some(at)) {
            // When: a new `sampled_at` is applied, this frame's demand produced a sample.
            applied.push(sampled_at);
        }
    }
    assert!(applied.len() >= 6, "{} samples in 3.2 s", applied.len());
    for pair in applied.windows(2) {
        let gap = pair[1].saturating_duration_since(pair[0]);
        assert!(gap >= FOREGROUND_PROCESS_TTL, "a sample came sooner than the TTL: {gap:?}");
        assert!(
            gap < FOREGROUND_PROCESS_TTL + Duration::from_millis(16),
            "sampling stalled: {gap:?}"
        );
    }
}
