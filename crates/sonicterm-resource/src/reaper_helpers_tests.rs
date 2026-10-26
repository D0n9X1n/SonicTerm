use super::*;
use crate::reaper::test_support::*;
use crate::reaper::*;
use crate::{CancelSource, SystemClock, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Whole grants serialize units at their minimum capacity; a queued sibling cannot start with a partial grant.
#[test]
fn whole_helper_grants_never_start_partial_units() {
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(2, 5, 16).unwrap(),
        Arc::new(SystemClock),
    ));
    let installed = Arc::new(AtomicUsize::new(0));
    let unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(2);
    let mut releases = Vec::new();
    for id in 1..=2 {
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        releases.push(release_tx);
        let started = started_tx.clone();
        let (slot, handles) = supervisor
            .try_reserve_unit(ReapUnitDemand { helpers: 5, handles: 8 })
            .unwrap()
            .into_parts();
        let work: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
            let _handles = handles;
            let _ = started.try_send(id);
            if release_rx.recv_timeout(Duration::from_secs(5)).is_ok() {
                ReapResult::Settled
            } else {
                ReapResult::TimedOut
            }
        });
        slot.enqueue(Box::new(GrantedTestTask {
            id,
            grant: Arc::new(Mutex::new(None)),
            calls: VecDeque::from([work]),
            installed: installed.clone(),
            installed_without_counters: unlocked.clone(),
            state: supervisor.state.clone(),
        }));
    }
    let cancel = CancelSource::new();
    let runner = {
        let supervisor = supervisor.clone();
        let token = cancel.token();
        std::thread::spawn(move || {
            supervisor.run_until(Instant::now() + Duration::from_secs(5), &token)
        })
    };
    let first = started_rx.recv_timeout(Duration::from_secs(1));
    let while_first = supervisor.live_helpers();
    let premature = started_rx.recv_timeout(Duration::from_millis(20));
    let _ = releases[0].try_send(());
    let second = started_rx.recv_timeout(Duration::from_secs(1));
    let while_second = supervisor.live_helpers();
    let _ = releases[1].try_send(());
    let progress = runner.join().expect("controlled grant run ended");
    assert_eq!(first.unwrap(), 1);
    assert!(premature.is_err(), "second unit started while the complete grant was occupied");
    assert_eq!(second.unwrap(), 2);
    assert_eq!((while_first, while_second), (5, 5));
    assert_eq!(installed.load(Ordering::SeqCst), 2, "one whole grant installed per unit");
    assert!(unlocked.load(Ordering::SeqCst), "task grant installation must not run under counters");
    assert_eq!(progress.settled, 2);
    assert_eq!(
        (supervisor.live_tasks(), supervisor.live_helpers(), supervisor.live_handles()),
        (0, 0, 0)
    );
}

/// Preservation: a grant-installation panic returns every helper permit without retaining the counters lock.
#[test]
fn panicking_grant_installation_returns_all_helpers() {
    struct PanickingGrantTask;
    impl ReapTask for PanickingGrantTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            ReapAction::Complete(ReapResult::Failed)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            CancelOutcome::TimedOut
        }
        fn helper_claim(&self) -> HelperClaim {
            HelperClaim::Grant(5)
        }
        fn accept_helper_grant(&mut self, _grant: HelperGrant) {
            panic!("grant installation failed");
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 5, 8).unwrap(), Arc::new(SystemClock));
    let mut in_flight = Vec::new();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervisor.start_on_helper(
            Box::new(|| ReapResult::Settled),
            Box::new(PanickingGrantTask),
            &mut in_flight,
        )
    }));
    assert!(failed.is_err());
    assert!(in_flight.is_empty());
    assert_eq!(supervisor.live_helpers(), 0);
}

/// Preservation: native worker clones keep the whole grant until the last running worker exits, with one occupant per slot.
#[test]
fn helper_grant_worker_retains_capacity_and_rejects_duplicate_slot() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 5, 8).unwrap(), Arc::new(SystemClock));
    let held = Arc::new(Mutex::new(None));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(GrantedTestTask {
        id: 1,
        grant: held.clone(),
        calls: VecDeque::from([
            Box::new(|| ReapResult::Settled) as Box<dyn FnOnce() -> ReapResult + Send>
        ]),
        installed: Arc::new(AtomicUsize::new(0)),
        installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        state: supervisor.state.clone(),
    }));
    supervisor.run_until(Instant::now() + Duration::from_secs(1), &CancelSource::new().token());
    let grant = held.lock().take().expect("task installed complete grant");
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let worker = grant
        .spawn(1, "controlled-native-worker", move || {
            release_rx.recv_timeout(Duration::from_secs(2))
        })
        .unwrap();
    let duplicate_refused = grant.spawn(1, "duplicate-native-worker", || ()).unwrap_err().kind();
    drop(grant);
    let retained = supervisor.live_helpers();
    let _ = release_tx.try_send(());
    worker.join().expect("native worker joined").expect("native gate released");
    assert_eq!(duplicate_refused, std::io::ErrorKind::WouldBlock);
    assert_eq!(
        retained, 5,
        "a worker clone retains every reserved helper, not only its occupied slot"
    );
    assert_eq!(supervisor.live_helpers(), 0);
}

/// Preservation: outer spawn refusal keeps the grant in task custody and Held retry does not increment helpers again.
#[test]
fn whole_helper_grant_survives_outer_spawn_failure_and_retry() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 5, 8).unwrap(), Arc::new(SystemClock));
    let slot = supervisor.try_reserve_slot().unwrap();
    let installed = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(Mutex::new(None));
    let task: Box<dyn ReapTask> = Box::new(GrantedTestTask {
        id: 1,
        grant: held.clone(),
        calls: VecDeque::new(),
        installed: installed.clone(),
        installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        state: supervisor.state.clone(),
    });
    let mut in_flight = Vec::new();
    let previous = FAIL_HELPER_SPAWN.replace(true);
    let refused =
        supervisor.start_on_helper(Box::new(|| ReapResult::Settled), task, &mut in_flight);
    FAIL_HELPER_SPAWN.set(previous);
    let (failed, mut task) = refused.expect("controlled outer spawn refused");
    let helper_count = supervisor.live_helpers();
    let held_claim = task.helper_claim();
    assert!(supervisor.start_on_helper(failed, task, &mut in_flight).is_none());
    let (handle, returned) = in_flight.pop().unwrap();
    let synthetic = handle.join().expect("synthetic failed call joined");
    task = returned;
    task.on_completion(synthetic);
    assert!(supervisor
        .start_on_helper(Box::new(|| ReapResult::Settled), task, &mut in_flight)
        .is_none());
    let (handle, task) = in_flight.pop().unwrap();
    let settled = handle.join().expect("Held retry joined");
    drop(task);
    held.lock().take();
    drop(slot);
    assert_eq!(helper_count, 5);
    assert_eq!(held_claim, HelperClaim::Held);
    assert_eq!(installed.load(Ordering::SeqCst), 1, "retry must reuse the installed grant");
    assert_eq!(synthetic, ReapResult::Failed);
    assert_eq!(settled, ReapResult::Settled);
    assert_eq!((supervisor.live_tasks(), supervisor.live_helpers()), (0, 0));
}

#[test]
fn blocking_work_never_runs_on_the_poll_loop() {
    // A real clock: the assertion observes a real OS thread, and virtual time
    // jumps to the deadline on the first poll, abandoning the call before it
    // records anything. Virtual time is for deferral, not for watching threads.
    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(2, 1, 2).unwrap(), Arc::new(clock));
    let ran_on = Arc::new(Mutex::new(None));
    supervisor
        .try_reserve_slot()
        .unwrap()
        .enqueue(Box::new(BlockingTask { owner: owner(2), ran_on: ran_on.clone() }));
    let cancel = CancelSource::new();
    let poll_thread = std::thread::current().id();
    supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    let observed = ran_on.lock().expect("blocking work ran");
    assert_ne!(observed, poll_thread, "blocking work must not occupy the poll loop");
    assert_eq!(supervisor.live_helpers(), 0, "helper count returns to zero");
}

/// Emits one blocking call that reports whether a sibling was running at the
/// same moment.
struct OverlapTask {
    owner: ResourceOwnerId,
    started: Arc<std::sync::Barrier>,
    overlapped: Arc<std::sync::atomic::AtomicBool>,
    issued: bool,
}

impl ReapTask for OverlapTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        if self.issued {
            return ReapAction::Complete(ReapResult::Settled);
        }
        self.issued = true;
        let started = self.started.clone();
        let overlapped = self.overlapped.clone();
        ReapAction::RunBlocking(Box::new(move || {
            // Rendezvous with the sibling call. If helpers run serially this
            // never completes, because the second call cannot start until the
            // first returns.
            let waited = started.wait();
            if waited.is_leader() {
                overlapped.store(true, Ordering::Relaxed);
            }
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
}

#[test]
fn two_blocking_calls_run_at_the_same_time() {
    // The property that matters: a hung native call must not hold up another
    // owner's teardown. Both calls rendezvous on a barrier, so this test
    // cannot pass if the supervisor joins each helper before starting the
    // next — the earlier implementation would hang here rather than fail.
    //
    // A real clock, for the same reason the poll-loop test needs one: these are
    // real OS threads, and virtual time jumps to the deadline on the first
    // poll, abandoning both calls before either reaches the barrier.
    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(4, 2, 4).unwrap(), Arc::new(clock));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let overlapped = Arc::new(std::sync::atomic::AtomicBool::new(false));

    for id in 1..=2u64 {
        supervisor.try_reserve_slot().unwrap().enqueue(Box::new(OverlapTask {
            owner: owner(id),
            started: barrier.clone(),
            overlapped: overlapped.clone(),
            issued: false,
        }));
    }

    let cancel = CancelSource::new();
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(5)), &cancel.token());

    assert!(overlapped.load(Ordering::Relaxed), "two blocking calls never overlapped");
    assert_eq!(progress.settled, 2);
    assert_eq!(supervisor.live_helpers(), 0, "helper slots return to zero");
    assert_eq!(supervisor.live_tasks(), 0);
}

/// A call that runs until the test releases it.
struct WedgedCallTask {
    owner: ResourceOwnerId,
    issued: bool,
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl ReapTask for WedgedCallTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        if self.issued {
            return ReapAction::Complete(ReapResult::Settled);
        }
        self.issued = true;
        let release = self.release.clone();
        ReapAction::RunBlocking(Box::new(move || {
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
}

#[test]
fn a_running_helper_keeps_its_task_counted() {
    // Real time observes the helper's separate lifetime after the task times out.
    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(clock));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(WedgedCallTask {
        owner: owner(1),
        issued: false,
        release: release.clone(),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(deadline_from(&clock, Duration::from_millis(30)), &cancel.token());
    let live_tasks_at_timeout = supervisor.live_tasks();
    let live_helpers_at_timeout = supervisor.live_helpers();
    let admission_at_timeout = supervisor.try_reserve_slot().err();

    // Release and reap the helper before asserting so a failing regression leaves no thread running.
    release.store(true, Ordering::SeqCst);
    let waited = Instant::now();
    while supervisor.live_helpers() > 0 && waited.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(supervisor.live_helpers(), 0);
    assert_eq!(live_tasks_at_timeout, 1);
    assert_eq!(live_helpers_at_timeout, 1);
    assert_eq!(admission_at_timeout, Some(ReapAdmission::QueueFull));
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::QueueFull);

    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(supervisor.live_tasks(), 0);
    let _slot = supervisor.try_reserve_slot().expect("only task cleanup restores admission");
}

#[test]
fn a_failed_helper_spawn_keeps_its_task_counted() {
    // Persistent spawn refusal must retain the pending task through deadline cancellation.
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(clock.clone()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(WedgedCallTask {
        owner: owner(1),
        issued: false,
        release: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }));
    let cancel = CancelSource::new();
    let previous = FAIL_HELPER_SPAWN.replace(true);
    // Restore the calling thread's seam even if the run unwinds.
    struct RestoreSpawnFailure(bool);
    // Lifecycle: RestoreSpawnFailure Drop restores this thread's prior spawn-failure setting.
    impl Drop for RestoreSpawnFailure {
        fn drop(&mut self) {
            FAIL_HELPER_SPAWN.set(self.0);
        }
    }
    let restore = RestoreSpawnFailure(previous);
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_millis(10)), &cancel.token());
    drop(restore);

    assert_eq!(progress.settled, 0);
    assert_eq!(progress.unresolved, 1);
    assert_eq!(supervisor.live_helpers(), 0);
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::QueueFull);
}

#[test]
fn a_wedged_call_does_not_hold_the_deadline() {
    // Joining a running handle surrendered the deadline to the call itself: a
    // fifty millisecond budget waited three seconds, and a call that never
    // returns would have waited forever. That is the hang this supervisor
    // exists to bound.
    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(4, 2, 4).unwrap(), Arc::new(clock));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(WedgedCallTask {
        owner: owner(20),
        issued: false,
        release: release.clone(),
    }));

    let cancel = CancelSource::new();
    let budget = Duration::from_millis(50);
    let started = Instant::now();
    let report = supervisor.shutdown(deadline_from(&clock, budget), &cancel.token());
    let elapsed = started.elapsed();

    // Let the abandoned helper end so the test leaves nothing spinning.
    release.store(true, Ordering::SeqCst);

    assert!(
        elapsed < budget * 10,
        "shutdown overran its deadline: took {elapsed:?} against a {budget:?} budget"
    );
    assert!(!report.is_clean(), "an unreturned call is not a clean shutdown");
    assert_eq!(report.unresolved_owners, vec![owner(20)], "the stuck owner is named");
}

#[test]
fn an_abandoned_helper_returns_its_slot_when_the_call_finishes() {
    // Abandoning a call at the deadline is what keeps the deadline real, but
    // the slot has to come back when the call eventually returns. Releasing on
    // the join path alone did not: an abandoned call is never joined, so the
    // pool shrank by one on every abandonment until no blocking work could
    // start again — a hang traded for a silent, permanent stall.
    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(8, 2, 8).unwrap(), Arc::new(clock));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel = CancelSource::new();
    let budget = Duration::from_millis(30);

    // Abandon enough wedged calls to fill the pool.
    for id in 30..=31u64 {
        supervisor.try_reserve_slot().unwrap().enqueue(Box::new(WedgedCallTask {
            owner: owner(id),
            issued: false,
            release: release.clone(),
        }));
        let started = Instant::now();
        supervisor.run_until(deadline_from(&clock, budget), &cancel.token());
        assert!(started.elapsed() < budget * 10, "a wedged call must not hold the deadline");
    }
    assert_eq!(supervisor.live_helpers(), 2, "both helpers are genuinely still running");

    // Let the abandoned calls finish; each releases its own slot.
    release.store(true, Ordering::SeqCst);
    let waited = Instant::now();
    while supervisor.live_helpers() > 0 && waited.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        supervisor.live_helpers(),
        0,
        "an abandoned call must return its slot when it finishes"
    );

    // The pool is usable again: a fresh call runs to completion.
    let ran = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CountingCallTask {
        owner: owner(32),
        ran: ran.clone(),
        issued: false,
    }));
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(5)), &cancel.token());
    assert_eq!(ran.load(Ordering::SeqCst), 1, "the reclaimed pool still runs work");
    assert_eq!(progress.settled, 1);
}

/// Runs one blocking call and counts it.
struct CountingCallTask {
    owner: ResourceOwnerId,
    ran: Arc<AtomicUsize>,
    issued: bool,
}

impl ReapTask for CountingCallTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        if self.issued {
            return ReapAction::Complete(ReapResult::Settled);
        }
        self.issued = true;
        let ran = self.ran.clone();
        ReapAction::RunBlocking(Box::new(move || {
            ran.fetch_add(1, Ordering::SeqCst);
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
}

/// A blocking call that panics.
struct PanickingCallTask {
    owner: ResourceOwnerId,
    issued: bool,
}

impl ReapTask for PanickingCallTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        if self.issued {
            return ReapAction::Complete(ReapResult::Settled);
        }
        self.issued = true;
        ReapAction::RunBlocking(Box::new(|| panic!("native call panicked")))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
}

#[test]
fn a_panicking_call_returns_its_helper_slot() {
    // The slot release has to survive an unwind. Releasing after the call
    // instead of on drop lost the slot whenever a call panicked, and cleanup
    // work panicking is far more reachable than a native call wedging: two
    // panics were enough to pin the pool at its ceiling for the life of the
    // process, after which no blocking work could start at all.
    //
    // The previous test covered only the clean-return path, which is how this
    // reached a green suite.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let clock = SystemClock;
    let supervisor = ReaperSupervisor::new(ReaperLimits::new(8, 2, 8).unwrap(), Arc::new(clock));
    let cancel = CancelSource::new();

    for id in 40..=41u64 {
        supervisor
            .try_reserve_slot()
            .unwrap()
            .enqueue(Box::new(PanickingCallTask { owner: owner(id), issued: false }));
        supervisor.run_until(deadline_from(&clock, Duration::from_millis(200)), &cancel.token());
    }

    std::panic::set_hook(previous);

    assert_eq!(
        supervisor.live_helpers(),
        0,
        "a panicking call must return its slot, not pin it for the process lifetime"
    );

    // The pool is still usable, which is the consequence that matters.
    let ran = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CountingCallTask {
        owner: owner(42),
        ran: ran.clone(),
        issued: false,
    }));
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(5)), &cancel.token());
    assert_eq!(ran.load(Ordering::SeqCst), 1, "blocking work still runs after a panic");
    assert_eq!(progress.settled, 1);
}

/// Runs blocking work and records the thread it ran on.
struct BlockingTask {
    owner: ResourceOwnerId,
    ran_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

impl ReapTask for BlockingTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        let ran_on = self.ran_on.clone();
        ReapAction::RunBlocking(Box::new(move || {
            *ran_on.lock() = Some(std::thread::current().id());
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
}
