use super::*;
use crate::{
    cancel::CancelSource,
    clock::{SystemClock, TestClock},
};
use sonicterm_types::CancelReason;
use std::sync::atomic::{AtomicUsize, Ordering};

std::thread_local! {
    // Notify only this test's first actual condition-variable wait, while counters still excludes shutdown.
    static SLOT_WAIT_STARTED: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const {
        std::cell::RefCell::new(None)
    };
    // Inject the closed-admission timeout branch once; this does not pretend to reproduce a scheduling race.
    static CLOSE_AT_SLOT_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Expose the actual wait boundary and optional branch-state injection while counters is held.
pub(super) fn slot_wait_started(counters: &mut Counters) {
    if CLOSE_AT_SLOT_WAIT.replace(false) {
        counters.admitting = false;
    }
    SLOT_WAIT_STARTED.with(|signal| {
        if let Some(signal) = signal.borrow_mut().take() {
            let _ = signal.try_send(());
        }
    });
}

fn owner(id: u64) -> ResourceOwnerId {
    ResourceOwnerId::new(id).unwrap()
}

fn limits() -> ReaperLimits {
    ReaperLimits::new(2, 1, 2).unwrap()
}

fn supervisor(clock: &TestClock) -> ReaperSupervisor {
    ReaperSupervisor::new(limits(), Arc::new(clock.clone()))
}

/// Settles immediately with a configured disposition.
struct ImmediateTask {
    owner: ResourceOwnerId,
    result: ReapResult,
    completions: Arc<AtomicUsize>,
}

impl ReapTask for ImmediateTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::Complete(self.result)
    }
    fn on_completion(&mut self, _result: ReapResult) {
        self.completions.fetch_add(1, Ordering::Relaxed);
    }
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
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

/// Never settles; always asks to be polled past any deadline.
struct StuckTask {
    owner: ResourceOwnerId,
    forced: Arc<AtomicUsize>,
    settles_on_force: bool,
}

impl ReapTask for StuckTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        ReapAction::PollAfter(now + Duration::from_secs(3600))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        self.forced.fetch_add(1, Ordering::Relaxed);
        if self.settles_on_force {
            CancelOutcome::Settled
        } else {
            CancelOutcome::TimedOut
        }
    }
}

#[test]
fn zero_ceiling_limits_are_rejected() {
    assert!(ReaperLimits::new(0, 1, 1).is_none());
    assert!(ReaperLimits::new(1, 0, 1).is_none());
    assert!(ReaperLimits::new(1, 1, 0).is_none());
    assert!(ReaperLimits::new(1, 1, 1).is_some());
}

#[test]
fn queue_full_refuses_admission_and_leaves_the_caller_owning_its_work() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let first = supervisor.try_reserve_slot().expect("first slot");
    let second = supervisor.try_reserve_slot().expect("second slot");
    // Ceiling is 2. The third caller must be refused, not silently queued.
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::QueueFull);
    assert!(!ReapAdmission::QueueFull.admits());
    // Dropping an unused slot returns capacity, which is what makes the
    // synchronous-completion fallback safe.
    drop(first);
    let third = supervisor.try_reserve_slot().expect("slot freed by drop");
    drop(second);
    drop(third);
    assert_eq!(supervisor.live_tasks(), 0);
}

/// A unit reserves one task and every native-handle permit atomically; partial capacity never consumes a task slot.
#[test]
fn unit_reservation_claims_task_and_handles_atomically() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(2, 5, 15).unwrap(), Arc::new(TestClock::new()));
    let demand = ReapUnitDemand { helpers: 5, handles: 8 };
    let first = supervisor.try_reserve_unit(demand).expect("first whole unit");
    assert_eq!((supervisor.live_tasks(), supervisor.live_handles()), (1, 8));
    assert_eq!(supervisor.try_reserve_unit(demand).err(), Some(ReapAdmission::QueueFull));
    assert_eq!(
        (supervisor.live_tasks(), supervisor.live_handles()),
        (1, 8),
        "refused unit leaves both axes intact"
    );
    let (slot, mut handles) = first.into_parts();
    drop(handles.pop());
    assert_eq!(supervisor.live_handles(), 7, "each closed native can return its own permit");
    let second =
        supervisor.try_reserve_unit(demand).expect("all handles now fit alongside the first task");
    assert_eq!((supervisor.live_tasks(), supervisor.live_handles()), (2, 15));
    drop(second);
    drop(slot);
    assert_eq!(
        (supervisor.live_tasks(), supervisor.live_handles()),
        (0, 7),
        "native custody outlives an unused task slot"
    );
    drop(handles);
    assert_eq!(supervisor.live_handles(), 0);
}

/// A host unit that cannot ever fit is distinguished from transient saturation before any permit is issued.
#[test]
fn unit_below_minimum_capacity_never_claims_permits() {
    let demand = ReapUnitDemand { helpers: 5, handles: 8 };
    for limits in [ReaperLimits::new(1, 4, 8).unwrap(), ReaperLimits::new(1, 5, 7).unwrap()] {
        let supervisor = ReaperSupervisor::new(limits, Arc::new(TestClock::new()));
        assert_eq!(
            supervisor.try_reserve_unit(demand).err(),
            Some(ReapAdmission::BelowMinimumCapacity)
        );
        assert_eq!(
            (supervisor.live_tasks(), supervisor.live_handles(), supervisor.live_helpers()),
            (0, 0, 0)
        );
    }
    assert!(!ReapAdmission::BelowMinimumCapacity.admits());
}

#[test]
fn a_reserved_slot_transfers_ownership_only_on_enqueue() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let completions = Arc::new(AtomicUsize::new(0));
    let slot = supervisor.try_reserve_slot().unwrap();
    assert_eq!(supervisor.live_tasks(), 1, "reservation is accounted before enqueue");
    slot.enqueue(Box::new(ImmediateTask {
        owner: owner(1),
        result: ReapResult::Settled,
        completions: completions.clone(),
    }));
    let cancel = CancelSource::new();
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(progress.settled, 1);
    assert_eq!(completions.load(Ordering::Relaxed), 1);
    assert_eq!(supervisor.live_tasks(), 0);
}

/// An opt-in unit keeps its whole helper grant in task/closure shared custody across retries.
struct GrantedTestTask {
    id: u64,
    grant: Arc<Mutex<Option<HelperGrant>>>,
    calls: VecDeque<Box<dyn FnOnce() -> ReapResult + Send>>,
    installed: Arc<AtomicUsize>,
    installed_without_counters: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<SupervisorState>,
}

impl ReapTask for GrantedTestTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(self.id)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        self.calls
            .pop_front()
            .map_or(ReapAction::Complete(ReapResult::Settled), ReapAction::RunBlocking)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn helper_claim(&self) -> HelperClaim {
        if self.grant.lock().is_some() {
            HelperClaim::Held
        } else {
            HelperClaim::Grant(5)
        }
    }
    fn accept_helper_grant(&mut self, grant: HelperGrant) {
        self.installed_without_counters
            .store(self.state.counters.try_lock().is_some(), Ordering::SeqCst);
        self.installed.fetch_add(1, Ordering::SeqCst);
        *self.grant.lock() = Some(grant);
    }
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.grant.lock().clone()
    }
}

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

#[test]
fn a_timeout_keeps_the_charge_and_records_the_owner() {
    // Timeout retains ownership and its task permit instead of making room for more work.
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(3),
        forced: forced.clone(),
        settles_on_force: false,
    }));
    let cancel = CancelSource::new();
    // The task asks to be polled an hour out; the deadline is one second.
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(progress.unresolved, 1, "an unsettled task must not count as settled");
    assert_eq!(progress.settled, 0);
    assert_eq!(forced.load(Ordering::Relaxed), 1, "the task was force-cancelled, not abandoned");
    assert_eq!(supervisor.live_tasks(), 1, "the retained task keeps its slot");
}

#[test]
fn retained_custody_refuses_admission_at_the_task_ceiling() {
    // A timeout retains both the task and its admission permit until terminal cleanup.
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(clock.clone()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(1),
        forced: Arc::new(AtomicUsize::new(0)),
        settles_on_force: false,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());

    assert_eq!(supervisor.retained_tasks(), 1);
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::QueueFull);

    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(supervisor.live_tasks(), 0);
    let _slot = supervisor.try_reserve_slot().expect("terminal cleanup returns capacity");
}

#[test]
fn repeated_timeouts_never_exceed_a_tiny_task_bound() {
    // Retained tasks consume the same small bound across repeated run_until calls.
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(2, 1, 1).unwrap(), Arc::new(clock.clone()));
    let cancel = CancelSource::new();
    let mut admitted = 0;
    let mut refused = 0;
    for _ in 0..10 {
        match supervisor.try_reserve_slot() {
            Ok(slot) => {
                admitted += 1;
                slot.enqueue(Box::new(StuckTask {
                    owner: owner(1),
                    forced: Arc::new(AtomicUsize::new(0)),
                    settles_on_force: false,
                }));
                supervisor
                    .run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
            }
            Err(ReapAdmission::QueueFull) => refused += 1,
            Err(other) => panic!("unexpected admission refusal: {other:?}"),
        }
    }
    assert_eq!(admitted, 2);
    assert_eq!(refused, 8);
    assert_eq!(supervisor.live_tasks(), 2);

    assert_eq!(supervisor.release_retained(), 2);
    let _first = supervisor.try_reserve_slot().expect("first released permit");
    let _second = supervisor.try_reserve_slot().expect("second released permit");
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::QueueFull);
}

#[test]
fn a_forced_cancel_that_settles_releases_the_charge() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(4),
        forced: forced.clone(),
        settles_on_force: true,
    }));
    let cancel = CancelSource::new();
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(progress.settled, 1);
    assert_eq!(progress.unresolved, 0);
}

#[test]
fn cancellation_short_circuits_the_run_loop() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(5),
        forced: forced.clone(),
        settles_on_force: true,
    }));
    let cancel = CancelSource::new();
    cancel.cancel(CancelReason::Shutdown);
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(3600)), &cancel.token());
    assert_eq!(progress.settled, 1);
    assert_eq!(forced.load(Ordering::Relaxed), 1);
}

#[test]
fn handles_are_bounded_and_released() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    supervisor.try_reserve_handle().unwrap();
    supervisor.try_reserve_handle().unwrap();
    assert_eq!(supervisor.try_reserve_handle().unwrap_err(), ReapAdmission::QueueFull);
    assert_eq!(supervisor.live_handles(), 2);
    supervisor.release_handle();
    supervisor.try_reserve_handle().expect("handle freed");
    supervisor.release_handle();
    supervisor.release_handle();
    assert_eq!(supervisor.live_handles(), 0);
}

#[test]
fn shutdown_stops_admission_and_reports_a_clean_exit() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let completions = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(ImmediateTask {
        owner: owner(6),
        result: ReapResult::Settled,
        completions: completions.clone(),
    }));
    let cancel = CancelSource::new();
    let report =
        supervisor.shutdown(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.settled, 1);
    assert!(!supervisor.is_admitting());
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::ShuttingDown);
}

/// Closing admission wakes an already waiting reserver without waiting for its held slot or deadline.
#[test]
fn shutdown_wakes_a_waiting_reserver() {
    let supervisor =
        Arc::new(ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock)));
    let held = supervisor.try_reserve_slot().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let waiter = {
        let supervisor = supervisor.clone();
        std::thread::spawn(move || {
            SLOT_WAIT_STARTED.with(|signal| *signal.borrow_mut() = Some(started_tx));
            let result = supervisor.reserve_slot_until(Instant::now() + Duration::from_secs(5));
            let _ = result_tx.send(result.map(drop));
        })
    };
    // The hook executes with counters held, so shutdown can only cross it after Condvar has released that lock.
    let entered_wait = started_rx.recv_timeout(Duration::from_secs(2));
    let cancel = CancelSource::new();
    let report = supervisor.shutdown(Instant::now() + Duration::from_millis(100), &cancel.token());
    let observed = result_rx.recv_timeout(Duration::from_secs(2));
    // Return the reservation and join before assertions, including when the missing notification leaves the wait blocked.
    drop(held);
    let joined = waiter.join();
    assert!(entered_wait.is_ok(), "reserver did not enter its capacity wait");
    joined.expect("capacity waiter exited normally");
    assert_eq!(
        observed.expect("shutdown wakes the capacity waiter"),
        Err(ReapAdmission::ShuttingDown)
    );
    assert_eq!(report.live_tasks, 1, "shutdown must not discard a still-held reservation");
    assert_eq!(supervisor.live_tasks(), 0);
}

#[test]
fn shutdown_reports_unresolved_owners_rather_than_dropping_them() {
    // Shutdown reports retained custody in both its owner list and live task count.
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(7),
        forced: forced.clone(),
        settles_on_force: false,
    }));
    let cancel = CancelSource::new();
    let report =
        supervisor.shutdown(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert!(!report.is_clean(), "an unresolved owner must not read as clean");
    assert_eq!(report.unresolved_owners, vec![owner(7)]);
    assert_eq!(report.live_tasks, 1);
    assert_eq!(report.live_helpers, 0);
}

#[test]
fn waiting_for_a_slot_times_out_without_admitting() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let _first = supervisor.try_reserve_slot().unwrap();
    let _second = supervisor.try_reserve_slot().unwrap();
    let outcome = supervisor.reserve_slot_until(Instant::now() + Duration::from_millis(20));
    assert_eq!(outcome.err(), Some(ReapAdmission::QueueFull));
}

/// Inject closed admission under the waiter guard; a real timeout must prefer shutdown without claiming a race reproduction.
#[test]
fn closed_admission_at_wait_timeout_reports_shutting_down() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let held = supervisor.try_reserve_slot().unwrap();
    CLOSE_AT_SLOT_WAIT.set(true);
    // No notification is sent and capacity stays occupied until after the deadline branch returns.
    let observed = supervisor.reserve_slot_until(Instant::now() + Duration::from_millis(20));
    CLOSE_AT_SLOT_WAIT.set(false);
    drop(held);
    assert_eq!(observed.err(), Some(ReapAdmission::ShuttingDown));
    assert_eq!(supervisor.live_tasks(), 0);
}

#[test]
fn a_released_slot_wakes_a_waiting_reserver() {
    let clock = TestClock::new();
    let supervisor = Arc::new(supervisor(&clock));
    let first = supervisor.try_reserve_slot().unwrap();
    let _second = supervisor.try_reserve_slot().unwrap();
    let waiter = {
        let supervisor = supervisor.clone();
        std::thread::spawn(move || {
            supervisor
                .reserve_slot_until(Instant::now() + Duration::from_secs(5))
                .map(|slot| {
                    drop(slot);
                })
                .is_ok()
        })
    };
    std::thread::sleep(Duration::from_millis(30));
    drop(first);
    assert!(waiter.join().unwrap(), "a released slot must wake a waiter");
}

#[test]
fn release_retained_wakes_a_waiting_reserver() {
    // Retained custody blocks a waiter; terminal cleanup must wake it before its deadline.
    let clock = TestClock::new();
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(1, 1, 1).unwrap(),
        Arc::new(clock.clone()),
    ));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
        owner: owner(1),
        forced: Arc::new(AtomicUsize::new(0)),
        settles_on_force: false,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let waiter = {
        let supervisor = supervisor.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = supervisor.reserve_slot_until(Instant::now() + Duration::from_secs(5));
            result_tx.send(result).unwrap();
        })
    };
    started_rx.recv_timeout(Duration::from_secs(5)).expect("waiter started");
    let while_retained = result_rx.recv_timeout(Duration::from_millis(30));
    let released = supervisor.release_retained();
    let after_release = result_rx.recv_timeout(Duration::from_secs(1));
    // Join before assertions so the baseline failure cannot leave a waiting thread behind.
    waiter.join().unwrap();

    assert!(
        matches!(while_retained, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "retained custody must keep the reserver blocked: {while_retained:?}"
    );
    assert_eq!(released, 1);
    let slot = after_release.expect("cleanup wakes the waiter promptly").expect("slot admitted");
    drop(slot);
    assert_eq!(supervisor.live_tasks(), 0);
}

/// Holds a committed charge until told to settle, so a test can keep an owner
/// pinned in `Closing` the way a real uncancellable transport would.
struct ChargeHoldingTask {
    owner: ResourceOwnerId,
    charge: Option<crate::CommittedReservation>,
    settle_after: usize,
    polls: usize,
}

impl ReapTask for ChargeHoldingTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        self.polls += 1;
        if self.polls >= self.settle_after {
            // Releasing the charge is what actually lets the owner close.
            self.charge = None;
            ReapAction::Complete(ReapResult::Settled)
        } else {
            ReapAction::PollAfter(now + Duration::from_millis(1))
        }
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        // Never settles on force: the charge outlives the deadline, which is
        // the case that pins an owner.
        CancelOutcome::TimedOut
    }
}

#[test]
fn an_owner_cannot_close_while_a_reap_task_holds_its_charge() {
    // MM-05: begin_close succeeds because it only stops admission; finish_close
    // must refuse while the reaper still holds a charge against the owner, and
    // must succeed once the task settles and drops it.
    use enum_map::enum_map;
    use sonicterm_types::{
        GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
    };

    let governor = crate::ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let owner_limits = || OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let window =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits()).unwrap();
    let committed = governor
        .try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount { bytes: 256, items: 1 })
        .unwrap()
        .commit(ResourceAmount { bytes: 256, items: 1 })
        .unwrap_or_else(|error| panic!("commit failed: {:?}", error.error));

    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(4, 2, 4).unwrap(), Arc::new(clock.clone()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(ChargeHoldingTask {
        owner: pane,
        charge: Some(committed),
        settle_after: 3,
        polls: 0,
    }));

    // Admission stops, but the charge is still outstanding.
    governor.begin_close(pane).unwrap();
    let refused = governor.finish_close(pane).unwrap_err();
    assert!(
        matches!(refused, sonicterm_types::BudgetError::OwnerHasLiveCharges { owner, amount }
            if owner == pane && amount == ResourceAmount { bytes: 256, items: 1 }),
        "expected the true outstanding amount, got {refused:?}"
    );

    // Drive the reaper until the task settles and drops the charge.
    let cancel = CancelSource::new();
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(progress.settled, 1);

    // Now the owner can finish closing.
    governor.finish_close(pane).unwrap();
    // Its record is dropped once closed, so there is nothing left to snapshot
    // — which is the strongest form of "a settled owner holds nothing".
    assert!(
        matches!(governor.snapshot(pane), Err(sonicterm_types::BudgetError::OwnerNotFound(id)) if id == pane),
        "a closed owner's record must not outlive it"
    );
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn shutdown_leaves_an_unsettled_owner_pinned_with_its_charge_visible() {
    // Shutdown preserves the original owner's charge, Closing state, and task permit.
    // Unsettled work must remain visible rather than being forced to Closed.
    use enum_map::enum_map;
    use sonicterm_types::{
        GovernorLimits, OwnerKind, OwnerLimits, OwnerState, ProcessKind, ResourceAmount,
        ResourceClass,
    };

    let governor = crate::ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let owner_limits = || OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let window =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits()).unwrap();
    let committed = governor
        .try_reserve(pane, ResourceClass::ReaperWork, ResourceAmount { bytes: 512, items: 2 })
        .unwrap()
        .commit(ResourceAmount { bytes: 512, items: 2 })
        .unwrap_or_else(|error| panic!("commit failed: {:?}", error.error));

    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(4, 2, 4).unwrap(), Arc::new(clock.clone()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(ChargeHoldingTask {
        owner: pane,
        charge: Some(committed),
        settle_after: usize::MAX, // never settles
        polls: 0,
    }));
    governor.begin_close(pane).unwrap();

    let cancel = CancelSource::new();
    let report =
        supervisor.shutdown(deadline_from(&clock, Duration::from_millis(1)), &cancel.token());

    assert!(!report.is_clean(), "an unsettled owner must not read as a clean exit");
    assert_eq!(
        report.unresolved_owners,
        vec![pane],
        "the report names the owner still holding work"
    );
    assert_eq!(report.live_tasks, 1, "unsettled work keeps its task permit");

    // The charge is still attributed to its original owner, and the owner is
    // still Closing rather than forced Closed.
    let snapshot = governor.snapshot(pane).unwrap();
    assert_eq!(snapshot.owner_amount, ResourceAmount { bytes: 512, items: 2 });
    assert_eq!(snapshot.owner_state, OwnerState::Closing);
    assert!(matches!(
        governor.finish_close(pane),
        Err(sonicterm_types::BudgetError::OwnerHasLiveCharges { .. })
    ));
    assert_eq!(snapshot.release_failures, 0, "a pinned charge is not a release failure");
    assert_eq!(supervisor.retained_tasks(), 1, "the unsettled task is held, not dropped");

    // Terminal cleanup: dropping the supervisor releases what it was holding,
    // so retention defers the release rather than leaking it forever.
    drop(supervisor);
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        ResourceAmount::default(),
        "terminal cleanup releases the retained charge"
    );
    governor.finish_close(pane).unwrap();
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

/// Holds a charge and surrenders it only when asked, modelling a transport
/// that never settles on its own.
struct WedgedTask {
    owner: ResourceOwnerId,
    charge: Option<crate::CommittedReservation>,
}

impl ReapTask for WedgedTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        ReapAction::PollAfter(now + Duration::from_secs(3600))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn surrender_charges(&mut self) {
        self.charge = None;
    }
}

#[test]
fn a_wedged_task_can_surrender_its_charge_and_unpin_the_subtree() {
    // Retention keeps an unsettled charge visible, but without a way to give it
    // up one wedged transport pins its owner, and every ancestor above it, for
    // the life of the process. This is the escape hatch.
    use enum_map::enum_map;
    use sonicterm_types::{
        BudgetError, GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount,
        ResourceClass,
    };

    let governor = crate::ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let owner_limits = || OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let window =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits()).unwrap();
    let committed = governor
        .try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount { bytes: 128, items: 1 })
        .unwrap()
        .commit(ResourceAmount { bytes: 128, items: 1 })
        .unwrap_or_else(|error| panic!("commit failed: {:?}", error.error));

    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(4, 2, 4).unwrap(), Arc::new(clock.clone()));
    supervisor
        .try_reserve_slot()
        .unwrap()
        .enqueue(Box::new(WedgedTask { owner: pane, charge: Some(committed) }));

    governor.begin_close(pane).unwrap();
    let cancel = CancelSource::new();
    let report =
        supervisor.shutdown(deadline_from(&clock, Duration::from_millis(1)), &cancel.token());
    assert!(!report.is_clean());
    assert_eq!(supervisor.retained_tasks(), 1);

    // Pinned: neither the pane nor the window above it can close.
    assert!(matches!(governor.finish_close(pane), Err(BudgetError::OwnerHasLiveCharges { .. })));
    governor.begin_close(window).unwrap();
    assert!(matches!(governor.finish_close(window), Err(BudgetError::OwnerHasLiveChildren { .. })));

    // Surrendering releases the charge and task permit, unpinning the whole subtree.
    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(supervisor.live_tasks(), 0);
    assert_eq!(supervisor.retained_tasks(), 0);
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount, ResourceAmount::default());
    governor.finish_close(pane).unwrap();
    governor.finish_close(window).unwrap();

    // The diagnosis is not retracted by the cleanup.
    assert_eq!(report.unresolved_owners, vec![pane]);
}

/// Retains custody normally but fails while surrendering its charges.
struct PanickingSurrenderTask;

impl ReapTask for PanickingSurrenderTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(1)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::Complete(ReapResult::TimedOut)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn surrender_charges(&mut self) {
        panic!("surrender failed");
    }
}

#[test]
fn a_panicking_surrender_still_returns_the_permit() {
    // Unwinding surrender destroys retained custody and must also restore admission.
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(clock.clone()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(PanickingSurrenderTask));
    let cancel = CancelSource::new();
    supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(supervisor.retained_tasks(), 1);

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervisor.release_retained();
    }));
    let panic = outcome.expect_err("surrender panic must propagate");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"surrender failed"));
    assert_eq!(supervisor.retained_tasks(), 0);
    assert_eq!(supervisor.live_tasks(), 0);
    let _slot = supervisor.try_reserve_slot().expect("unwound cleanup returns capacity");
}

/// A settled task's destructor may release native permits through the same supervisor counters.
struct CounterCheckingDropTask {
    state: Arc<SupervisorState>,
    observed_unlocked: Arc<std::sync::atomic::AtomicBool>,
}

impl ReapTask for CounterCheckingDropTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(1)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::Complete(ReapResult::Settled)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
}

// Lifecycle: CounterCheckingDropTask observes counters without blocking a regression's failed destruction path.
impl Drop for CounterCheckingDropTask {
    fn drop(&mut self) {
        self.observed_unlocked.store(self.state.counters.try_lock().is_some(), Ordering::SeqCst);
    }
}

/// Returning task custody must release counters before destructor code can return native-handle permits.
#[test]
fn settle_drops_a_settled_task_after_releasing_counters() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let observed_unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CounterCheckingDropTask {
        state: supervisor.state.clone(),
        observed_unlocked: observed_unlocked.clone(),
    }));
    let cancel = CancelSource::new();
    let progress =
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    assert_eq!(progress.settled, 1);
    assert!(observed_unlocked.load(Ordering::SeqCst), "settled destructor still holds counters");
    assert_eq!(supervisor.live_tasks(), 0);
}

/// Settles on the caller thread but fails during task destruction.
struct PanickingDropTask {
    dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

impl ReapTask for PanickingDropTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(1)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::Complete(ReapResult::Settled)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::Settled
    }
}

// Lifecycle: PanickingDropTask records the destruction thread before testing unwind cleanup.
impl Drop for PanickingDropTask {
    fn drop(&mut self) {
        *self.dropped_on.lock() = Some(std::thread::current().id());
        panic!("task destructor failed");
    }
}

#[test]
fn a_settled_task_with_a_panicking_destructor_returns_its_permit() {
    // settle drops on the run_until caller, so its panic must not strand the task permit.
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(clock.clone()));
    let dropped_on = Arc::new(Mutex::new(None));
    supervisor
        .try_reserve_slot()
        .unwrap()
        .enqueue(Box::new(PanickingDropTask { dropped_on: dropped_on.clone() }));
    let cancel = CancelSource::new();
    let caller = std::thread::current().id();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervisor.run_until(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());
    }));

    let panic = outcome.expect_err("task destructor panic must reach the run_until caller");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"task destructor failed"));
    assert_eq!(*dropped_on.lock(), Some(caller));
    assert_eq!(supervisor.live_tasks(), 0);
    let _slot = supervisor.try_reserve_slot().expect("panicking destruction returns capacity");
}

#[test]
fn releasing_retained_work_is_idempotent() {
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(2, 1, 2).unwrap(), Arc::new(clock.clone()));
    assert_eq!(supervisor.release_retained(), 0, "nothing retained yet");
    assert_eq!(supervisor.release_retained(), 0, "still nothing");
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

/// A bounded gate keeps an actual helper running while another thread requests graceful shutdown.
struct DeadlineGatedTask {
    started: std::sync::mpsc::SyncSender<()>,
    release: Option<std::sync::mpsc::Receiver<()>>,
    completion: Arc<Mutex<Option<ReapResult>>>,
}

impl ReapTask for DeadlineGatedTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(1)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        let release = self.release.take().expect("only one gated call is issued");
        let started = self.started.clone();
        ReapAction::RunBlocking(Box::new(move || {
            let _ = started.try_send(());
            if release.recv_timeout(Duration::from_secs(5)).is_ok() {
                ReapResult::Settled
            } else {
                ReapResult::TimedOut
            }
        }))
    }
    fn on_completion(&mut self, result: ReapResult) {
        *self.completion.lock() = Some(result);
    }
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
}

/// Closing admission must shorten an existing run without cancelling or relinquishing its gated helper's custody.
#[test]
fn close_admission_lowers_a_running_runs_deadline() {
    let supervisor =
        Arc::new(ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock)));
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let completion = Arc::new(Mutex::new(None));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(DeadlineGatedTask {
        started: started_tx,
        release: Some(release_rx),
        completion: completion.clone(),
    }));
    let cancel = CancelSource::new();
    let (run_tx, run_rx) = std::sync::mpsc::sync_channel(1);
    let runner = {
        let supervisor = supervisor.clone();
        let token = cancel.token();
        std::thread::spawn(move || {
            let progress = supervisor.run_until(Instant::now() + Duration::from_secs(60), &token);
            let _ = run_tx.send(progress);
        })
    };
    let started = started_rx.recv_timeout(Duration::from_secs(1));
    let (wait_tx, wait_rx) = std::sync::mpsc::sync_channel(1);
    let (waiting_tx, waiting_rx) = std::sync::mpsc::sync_channel(1);
    let waiter = {
        let supervisor = supervisor.clone();
        std::thread::spawn(move || {
            SLOT_WAIT_STARTED.with(|signal| *signal.borrow_mut() = Some(wait_tx));
            let result = supervisor.reserve_slot_until(Instant::now() + Duration::from_secs(5));
            let _ = waiting_tx.send(result.map(drop));
        })
    };
    let entered_wait = wait_rx.recv_timeout(Duration::from_secs(1));
    let began_close = Instant::now();
    supervisor.shutdown_handle().close_admission(began_close + Duration::from_millis(100));
    let returned = run_rx.recv_timeout(Duration::from_secs(2));
    let elapsed = began_close.elapsed();
    let waiting = waiting_rx.recv_timeout(Duration::from_millis(100));
    let live_tasks = supervisor.live_tasks();
    let retained = supervisor.retained_tasks();
    let result = *completion.lock();
    let cancelled_during_close = cancel.is_cancelled();
    // Release all owned work before asserting so a missing shared deadline cannot leave a 60-second run behind.
    let _ = release_tx.try_send(());
    cancel.cancel(CancelReason::Shutdown);
    let joined = runner.join();
    let waiter_joined = waiter.join();
    let helpers_until = Instant::now() + Duration::from_secs(1);
    while supervisor.live_helpers() > 0 && Instant::now() < helpers_until {
        std::thread::yield_now();
    }
    supervisor.release_retained();
    joined.expect("run thread stopped after fixture release");
    waiter_joined.expect("capacity waiter stopped");
    assert!(started.is_ok() && entered_wait.is_ok(), "controlled helper and waiter started");
    assert!(returned.is_ok(), "closing admission did not shorten the running deadline");
    assert!(elapsed < Duration::from_secs(2), "the shared deadline was not observed promptly");
    assert_eq!(waiting.expect("admission waiter woke"), Err(ReapAdmission::ShuttingDown));
    assert_eq!((live_tasks, retained), (1, 1), "timed-out helper keeps task custody");
    assert_eq!(result, Some(ReapResult::TimedOut));
    assert!(!cancelled_during_close, "graceful admission close is not cancellation");
    assert_eq!(supervisor.live_helpers(), 0);
}

/// Report the first actual clock wait so deadline publication crosses an already sleeping run.
struct WaitObservedClock {
    entered: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
}

impl Clock for WaitObservedClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait_until(&self, deadline: Instant) {
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.try_send(());
        }
        SystemClock.wait_until(deadline);
    }
}

/// A deferred task stays dormant until its requested poll but does not settle on shutdown cancellation.
struct LongDeferredTask;

impl ReapTask for LongDeferredTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(1)
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        ReapAction::PollAfter(now + Duration::from_secs(3))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
}

/// A poll sleep must recheck a newly shortened deadline rather than holding shutdown until the old wake instant.
#[test]
fn close_admission_interrupts_a_deferred_poll_wait() {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(1, 1, 1).unwrap(),
        Arc::new(WaitObservedClock { entered: Mutex::new(Some(entered_tx)) }),
    ));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(LongDeferredTask));
    let cancel = CancelSource::new();
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let runner = {
        let supervisor = supervisor.clone();
        let token = cancel.token();
        std::thread::spawn(move || {
            let progress = supervisor.run_until(Instant::now() + Duration::from_secs(60), &token);
            let _ = finished_tx.send(progress);
        })
    };
    let entered = entered_rx.recv_timeout(Duration::from_secs(1));
    supervisor.shutdown_handle().close_admission(Instant::now() + Duration::from_millis(100));
    let returned = finished_rx.recv_timeout(Duration::from_secs(2));
    // The old wait is at most three seconds, so even its failing path rejoins before checking the observation.
    cancel.cancel(CancelReason::Shutdown);
    let joined = runner.join();
    supervisor.release_retained();
    joined.expect("deferred run stopped");
    assert!(entered.is_ok(), "run entered its first deferred wait");
    assert!(returned.is_ok(), "deferred poll wait ignored the shared shutdown deadline");
    let progress = returned.unwrap();
    assert_eq!(progress.unresolved, 1);
    assert_eq!(
        progress.polls, 1,
        "sliced clock waits must not re-poll the deferred task before its due time"
    );
}

/// A queued task observes an already closed drain deadline before next_action, without starting native work.
#[test]
fn closed_drain_deadline_cancels_a_queued_task_before_next_action() {
    struct QueuedCountingTask {
        next: Arc<AtomicUsize>,
        forced: Arc<AtomicUsize>,
    }
    impl ReapTask for QueuedCountingTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            self.next.fetch_add(1, Ordering::SeqCst);
            ReapAction::Complete(ReapResult::Settled)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            self.forced.fetch_add(1, Ordering::SeqCst);
            CancelOutcome::TimedOut
        }
    }
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let next = Arc::new(AtomicUsize::new(0));
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor
        .try_reserve_slot()
        .unwrap()
        .enqueue(Box::new(QueuedCountingTask { next: next.clone(), forced: forced.clone() }));
    supervisor.shutdown_handle().close_admission(clock.now());
    let progress =
        supervisor.run_until(clock.now() + Duration::from_secs(60), &CancelSource::new().token());
    let retained = supervisor.retained_tasks();
    let tasks = supervisor.live_tasks();
    supervisor.release_retained();
    assert_eq!(next.load(Ordering::SeqCst), 0, "closed shared deadline must precede next_action");
    assert_eq!(forced.load(Ordering::SeqCst), 1);
    assert_eq!((progress.settled, progress.unresolved, retained, tasks), (0, 1, 1, 1));
}

/// Preservation: repeat shutdown controls can only shorten the existing drain deadline and never reopen admission.
#[test]
fn shutdown_handle_never_extends_the_published_deadline() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let handle = supervisor.shutdown_handle();
    let first = clock.now() + Duration::from_secs(2);
    handle.close_admission(first);
    handle.clone().close_admission(first + Duration::from_secs(1));
    assert_eq!(supervisor.effective_deadline(first + Duration::from_secs(5)), first);
    let earlier = first - Duration::from_secs(1);
    handle.close_admission(earlier);
    assert_eq!(supervisor.effective_deadline(first), earlier);
    assert_eq!(supervisor.try_reserve_slot().unwrap_err(), ReapAdmission::ShuttingDown);
}

/// Terminal unresolved storage owns its payload, charge and handle permits without running native destructors at process teardown.
#[test]
fn unresolved_sink_preserves_payload_accounting_and_admission() {
    use crate::ResourceGovernor;
    use sonicterm_types::{
        GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
    };
    struct AccountedPayload {
        dropped: Arc<AtomicUsize>,
        _charge: crate::CommittedReservation,
        _handles: Vec<ReapHandlePermit>,
        _governor: ResourceGovernor,
    }
    // Lifecycle: AccountedPayload records native destruction before its charge and permits release.
    impl Drop for AccountedPayload {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    let governor = ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map::enum_map! { _ => usize::MAX },
            class_items: enum_map::enum_map! { _ => None },
        },
    )
    .unwrap();
    let transport = governor
        .create_child(
            governor.root_owner(),
            OwnerKind::PtyTransport,
            OwnerLimits {
                owner_bytes: usize::MAX,
                class_bytes: enum_map::enum_map! { _ => usize::MAX },
                class_items: enum_map::enum_map! { _ => None },
            },
        )
        .unwrap();
    let amount = ResourceAmount { bytes: 0, items: 1 };
    let charge = governor
        .try_reserve(transport, ResourceClass::ReaperWork, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    let (slot, handles) = supervisor
        .try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 })
        .unwrap()
        .into_parts();
    drop(slot);
    let dropped = Arc::new(AtomicUsize::new(0));
    sink.retain(
        transport,
        Box::new(AccountedPayload {
            dropped: dropped.clone(),
            _charge: charge,
            _handles: handles,
            _governor: governor.clone(),
        }),
    );
    assert_eq!((sink.entries(), supervisor.live_tasks(), supervisor.live_handles()), (1, 1, 1));
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    assert_eq!(
        supervisor.try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 }).err(),
        Some(ReapAdmission::QueueFull)
    );
    assert_eq!(
        governor.snapshot(transport).unwrap().owner_class_items[ResourceClass::ReaperWork],
        1
    );
    let report = supervisor.shutdown(Instant::now(), &CancelSource::new().token());
    assert!(!report.is_clean());
    assert_eq!((report.live_tasks, report.unresolved_entries, report.live_handles), (1, 1, 1));
    assert!(report.unresolved_owners.contains(&transport));
    drop(sink);
    drop(supervisor);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        0,
        "terminal sink drop must not destroy unresolved native payloads"
    );
    assert_eq!(
        governor.snapshot(transport).unwrap().owner_class_items[ResourceClass::ReaperWork],
        1
    );
}

/// Preservation: terminal task release hands off unresolved custody before returning its task permit, with no admission gap.
#[test]
fn release_retained_transfers_native_payload_to_unresolved_sink() {
    struct RetainOnDropTask {
        sink: UnresolvedSink,
        handles: Option<Vec<ReapHandlePermit>>,
    }
    impl ReapTask for RetainOnDropTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            ReapAction::Complete(ReapResult::TimedOut)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            CancelOutcome::TimedOut
        }
    }
    // Lifecycle: RetainOnDropTask moves handles into the sink instead of releasing unresolved native custody.
    impl Drop for RetainOnDropTask {
        fn drop(&mut self) {
            self.sink.retain(owner(1), Box::new(self.handles.take().unwrap()));
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(TestClock::new()));
    let sink = supervisor.unresolved_sink();
    let (slot, handles) = supervisor
        .try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 })
        .unwrap()
        .into_parts();
    slot.enqueue(Box::new(RetainOnDropTask { sink: sink.clone(), handles: Some(handles) }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_secs(1), &cancel.token());
    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(
        (
            sink.entries(),
            supervisor.live_tasks(),
            supervisor.live_handles(),
            supervisor.retained_tasks()
        ),
        (1, 1, 1, 0)
    );
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    let report = supervisor.shutdown(Instant::now(), &cancel.token());
    assert_eq!(
        report.unresolved_owners,
        vec![owner(1)],
        "task and sink naming the same transport stays deduplicated"
    );
    assert_eq!((report.live_tasks, report.unresolved_entries), (1, 1));
}

/// Slotless cancellation duplicates have independent sink counts until their actual native owner closes them.
#[test]
fn fallback_duplicate_token_keeps_shutdown_unclean_until_closed() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    let duplicate = sink.track_fallback_duplicate();
    let report = supervisor.shutdown(Instant::now(), &CancelSource::new().token());
    assert_eq!(
        (report.live_tasks, report.live_handles, report.open_fallback_duplicates),
        (0, 0, 1)
    );
    assert!(!report.is_clean());
    drop(duplicate);
    assert_eq!(sink.open_fallback_duplicates(), 0);
    assert!(supervisor.shutdown(Instant::now(), &CancelSource::new().token()).is_clean());
}

/// Preservation: sink counters stay observable while its payload lock is held, so admission never nests that lock.
#[test]
fn unresolved_sink_admission_uses_atomic_entry_count() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    sink.retain(owner(1), Box::new(()));
    let payloads = sink.state.payloads.lock();
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    drop(payloads);
}

/// Retains an abandoned helper separately from payload state, exposing completion only after native work succeeds.
struct CollectableTestTask {
    id: u64,
    release: Option<std::sync::mpsc::Receiver<()>>,
    started: std::sync::mpsc::SyncSender<()>,
    complete: Arc<std::sync::atomic::AtomicBool>,
    abandoned: Arc<Mutex<Vec<std::thread::JoinHandle<ReapResult>>>>,
    joined: Arc<AtomicUsize>,
}

impl ReapTask for CollectableTestTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(self.id)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        let release = self.release.take().expect("controlled task starts once");
        let complete = self.complete.clone();
        let started = self.started.clone();
        ReapAction::RunBlocking(Box::new(move || {
            let _ = started.try_send(());
            if release.recv_timeout(Duration::from_secs(5)).is_err() {
                return ReapResult::Failed;
            }
            complete.store(true, Ordering::SeqCst);
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn keep_abandoned_helper(&mut self, handle: std::thread::JoinHandle<ReapResult>) {
        self.abandoned.lock().push(handle);
    }
    fn is_collectable(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
            && self.abandoned.lock().iter().all(std::thread::JoinHandle::is_finished)
    }
    fn join_finished_helpers(&mut self) {
        for handle in self.abandoned.lock().drain(..) {
            assert!(handle.is_finished(), "collector must not join an unfinished helper");
            let _ = handle.join();
            self.joined.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A timed-out task keeps its outer JoinHandle and permit until real completion is joined by retained collection.
#[test]
fn abandoned_helper_handle_is_kept_until_collected() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    let joined = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CollectableTestTask {
        id: 1,
        release: Some(release_rx),
        started: started_tx,
        complete,
        abandoned: abandoned.clone(),
        joined: joined.clone(),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(50), &cancel.token());
    let started = started_rx.recv_timeout(Duration::from_secs(1));
    let kept = abandoned.lock().len();
    let early = supervisor.collect_settled_retained();
    let before_tasks = supervisor.live_tasks();
    let refusal = supervisor.try_reserve_slot().err();
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(2);
    while abandoned.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        std::thread::yield_now();
    }
    let collected = supervisor.collect_settled_retained();
    let clean =
        supervisor.shutdown(Instant::now() + Duration::from_secs(1), &cancel.token()).is_clean();
    assert!(started.is_ok(), "real helper entered its gate");
    assert_eq!((kept, early, before_tasks), (1, 0, 1));
    assert_eq!(refusal, Some(ReapAdmission::QueueFull));
    assert_eq!((collected, joined.load(Ordering::SeqCst), supervisor.live_tasks()), (1, 1, 0));
    assert!(clean, "collected completed owner must be removed from unresolved reporting");
}

/// Wrap late whole-task completion with a Windows-shaped whole helper grant, without introducing native calls.
struct CollectableGrantTestTask {
    inner: CollectableTestTask,
    grant: Option<HelperGrant>,
}

impl ReapTask for CollectableGrantTestTask {
    fn owner(&self) -> ResourceOwnerId {
        self.inner.owner()
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        self.inner.next_action(now)
    }
    fn on_completion(&mut self, result: ReapResult) {
        self.inner.on_completion(result);
    }
    fn force_cancel(&mut self) -> CancelOutcome {
        self.inner.force_cancel()
    }
    fn helper_claim(&self) -> HelperClaim {
        if self.grant.is_some() {
            HelperClaim::Held
        } else {
            HelperClaim::Grant(5)
        }
    }
    fn accept_helper_grant(&mut self, grant: HelperGrant) {
        self.grant = Some(grant);
    }
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.grant.clone()
    }
    fn keep_abandoned_helper(&mut self, handle: std::thread::JoinHandle<ReapResult>) {
        self.inner.keep_abandoned_helper(handle);
    }
    fn is_collectable(&self) -> bool {
        self.inner.is_collectable()
    }
    fn join_finished_helpers(&mut self) {
        self.inner.join_finished_helpers();
    }
}

/// Retained completion must release a whole grant inside the same run that is waiting to start its sibling.
#[test]
fn completed_retained_unit_frees_its_grant_within_the_run() {
    let (wait_tx, wait_rx) = std::sync::mpsc::sync_channel(1);
    let clock = Arc::new(WaitObservedClock { entered: Mutex::new(None) });
    let supervisor =
        Arc::new(ReaperSupervisor::new(ReaperLimits::new(2, 5, 16).unwrap(), clock.clone()));
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    let joined = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CollectableGrantTestTask {
        inner: CollectableTestTask {
            id: 1,
            release: Some(release_rx),
            started: started_tx,
            complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            abandoned: abandoned.clone(),
            joined: joined.clone(),
        },
        grant: None,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(50), &cancel.token());
    let first_started = started_rx.recv_timeout(Duration::from_secs(1));
    let (second_tx, second_rx) = std::sync::mpsc::sync_channel(1);
    let work: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
        let _ = second_tx.try_send(());
        ReapResult::Settled
    });
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(GrantedTestTask {
        id: 2,
        grant: Arc::new(Mutex::new(None)),
        calls: VecDeque::from([work]),
        installed: Arc::new(AtomicUsize::new(0)),
        installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        state: supervisor.state.clone(),
    }));
    *clock.entered.lock() = Some(wait_tx);
    let runner = {
        let supervisor = supervisor.clone();
        let token = cancel.token();
        std::thread::spawn(move || {
            supervisor.run_until(Instant::now() + Duration::from_secs(2), &token)
        })
    };
    let sibling_pending = wait_rx.recv_timeout(Duration::from_secs(1));
    let held_helpers = supervisor.live_helpers();
    let _ = release_tx.try_send(());
    let second_started = second_rx.recv_timeout(Duration::from_secs(1));
    let progress = runner.join().expect("bounded pending run completed");
    // Clean the deliberately retained baseline failure only after observing whether the same run made progress.
    let retained_before_cleanup = supervisor.retained_tasks();
    supervisor.collect_settled_retained();
    supervisor.release_retained();
    assert!(
        first_started.is_ok() && sibling_pending.is_ok(),
        "real helper and pending sibling reached their gates"
    );
    assert_eq!(held_helpers, 5);
    assert!(
        second_started.is_ok(),
        "retained completion did not free its grant inside the pending run"
    );
    assert_eq!(progress.settled, 2, "the same run collects the first and settles the second unit");
    assert_eq!(retained_before_cleanup, 0);
    assert_eq!(joined.load(Ordering::SeqCst), 1);
    assert_eq!((supervisor.live_tasks(), supervisor.live_helpers()), (0, 0));
}

/// An unstarted controlled unit can keep its task and original call for a later driver run without claiming a grant.
struct CarryOverTestTask {
    inner: GrantedTestTask,
    requeued: Arc<AtomicUsize>,
    forced: Arc<AtomicUsize>,
}

impl ReapTask for CarryOverTestTask {
    fn owner(&self) -> ResourceOwnerId {
        self.inner.owner()
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        self.inner.next_action(now)
    }
    fn on_completion(&mut self, result: ReapResult) {
        self.inner.on_completion(result);
    }
    fn force_cancel(&mut self) -> CancelOutcome {
        self.forced.fetch_add(1, Ordering::SeqCst);
        CancelOutcome::TimedOut
    }
    fn helper_claim(&self) -> HelperClaim {
        self.inner.helper_claim()
    }
    fn accept_helper_grant(&mut self, grant: HelperGrant) {
        self.inner.accept_helper_grant(grant);
    }
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.inner.held_helper_grant()
    }
    fn requeue_unstarted(&mut self) -> bool {
        if self.inner.grant.lock().is_some() {
            return false;
        }
        self.requeued.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// An unstarted unit crosses a normal cutoff once, then parks until retained completion makes its whole grant admissible.
#[test]
fn unstarted_unit_is_requeued_at_a_normal_cutoff() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(2, 5, 16).unwrap(), Arc::new(SystemClock));
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CollectableGrantTestTask {
        inner: CollectableTestTask {
            id: 1,
            release: Some(release_rx),
            started: started_tx,
            complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            abandoned: abandoned.clone(),
            joined: Arc::new(AtomicUsize::new(0)),
        },
        grant: None,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(40), &cancel.token());
    let first_started = started_rx.recv_timeout(Duration::from_secs(1));
    let ran = Arc::new(AtomicUsize::new(0));
    let call_ran = ran.clone();
    let work: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
        call_ran.fetch_add(1, Ordering::SeqCst);
        ReapResult::Settled
    });
    let requeued = Arc::new(AtomicUsize::new(0));
    let forced = Arc::new(AtomicUsize::new(0));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CarryOverTestTask {
        inner: GrantedTestTask {
            id: 2,
            grant: Arc::new(Mutex::new(None)),
            calls: VecDeque::from([work]),
            installed: Arc::new(AtomicUsize::new(0)),
            installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            state: supervisor.state.clone(),
        },
        requeued: requeued.clone(),
        forced: forced.clone(),
    }));
    let before = supervisor.has_startable_work();
    supervisor.run_until(Instant::now() + Duration::from_millis(30), &cancel.token());
    let parked = !supervisor.has_startable_work();
    let retained_before = supervisor.retained_tasks();
    let tasks_before = supervisor.live_tasks();
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(1);
    while abandoned.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        std::thread::yield_now();
    }
    let collected = supervisor.collect_settled_retained();
    let ready = supervisor.has_startable_work();
    let progress = supervisor.run_until(Instant::now() + Duration::from_secs(1), &cancel.token());
    supervisor.release_retained();
    assert!(first_started.is_ok() && before && parked && ready);
    assert_eq!((retained_before, tasks_before, collected), (1, 2, 1));
    assert_eq!((requeued.load(Ordering::SeqCst), forced.load(Ordering::SeqCst)), (1, 0));
    assert_eq!(
        (ran.load(Ordering::SeqCst), progress.settled),
        (1, 1),
        "the original queued call survives its first cutoff"
    );
    assert_eq!((supervisor.live_tasks(), supervisor.live_helpers()), (0, 0));
}

/// Preservation: shutdown drains carry-over as terminal custody instead of requeueing or starting a new normal run.
#[test]
fn closed_admission_drains_carry_over_without_requeueing() {
    let clock = TestClock::new();
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 5, 8).unwrap(), Arc::new(clock.clone()));
    let ran = Arc::new(AtomicUsize::new(0));
    let call_ran = ran.clone();
    let requeued = Arc::new(AtomicUsize::new(0));
    let forced = Arc::new(AtomicUsize::new(0));
    let work: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
        call_ran.fetch_add(1, Ordering::SeqCst);
        ReapResult::Settled
    });
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CarryOverTestTask {
        inner: GrantedTestTask {
            id: 1,
            grant: Arc::new(Mutex::new(None)),
            calls: VecDeque::from([work]),
            installed: Arc::new(AtomicUsize::new(0)),
            installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            state: supervisor.state.clone(),
        },
        requeued: requeued.clone(),
        forced: forced.clone(),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(clock.now(), &cancel.token());
    assert!(
        supervisor.has_startable_work(),
        "unstarted carried work can start while admission remains open"
    );
    supervisor.shutdown_handle().close_admission(clock.now());
    let startable_closed = supervisor.has_startable_work();
    let report = supervisor.shutdown(clock.now() + Duration::from_secs(1), &cancel.token());
    let released = supervisor.release_retained();
    assert!(!startable_closed);
    assert_eq!(
        (
            requeued.load(Ordering::SeqCst),
            forced.load(Ordering::SeqCst),
            ran.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    assert_eq!((report.live_tasks, released), (1, 1));
    assert_eq!(report.unresolved_owners, vec![owner(1)]);
}

/// Preservation: eligible carry-over resumes at the queue front before work enqueued after the previous cutoff.
#[test]
fn carry_over_precedes_newly_enqueued_work() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(2, 5, 8).unwrap(), Arc::new(SystemClock));
    let order = Arc::new(Mutex::new(Vec::new()));
    let make_task = |id| {
        let output = order.clone();
        let work: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
            output.lock().push(id);
            ReapResult::Settled
        });
        CarryOverTestTask {
            inner: GrantedTestTask {
                id,
                grant: Arc::new(Mutex::new(None)),
                calls: VecDeque::from([work]),
                installed: Arc::new(AtomicUsize::new(0)),
                installed_without_counters: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                state: supervisor.state.clone(),
            },
            requeued: Arc::new(AtomicUsize::new(0)),
            forced: Arc::new(AtomicUsize::new(0)),
        }
    };
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(make_task(1)));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now(), &cancel.token());
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(make_task(2)));
    let progress = supervisor.run_until(Instant::now() + Duration::from_secs(1), &cancel.token());
    assert_eq!(*order.lock(), vec![1, 2]);
    assert_eq!(progress.settled, 2);
    assert_eq!(supervisor.live_tasks(), 0);
}

/// Preservation: collection invokes joins and task destruction only after releasing retained and counter locks.
#[test]
fn retained_collection_releases_locks_before_join_and_drop() {
    struct LockCheckingCollectedTask {
        state: Arc<SupervisorState>,
        join_unlocked: Arc<std::sync::atomic::AtomicBool>,
        drop_unlocked: Arc<std::sync::atomic::AtomicBool>,
    }
    impl ReapTask for LockCheckingCollectedTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            ReapAction::Complete(ReapResult::TimedOut)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            CancelOutcome::TimedOut
        }
        fn is_collectable(&self) -> bool {
            true
        }
        fn join_finished_helpers(&mut self) {
            let counters = self.state.counters.try_lock().is_some();
            let retained = self.state.retained.try_lock().is_some();
            self.join_unlocked.store(counters && retained, Ordering::SeqCst);
        }
    }
    // Lifecycle: LockCheckingCollectedTask records unlocked counters and retained custody before its fields release.
    impl Drop for LockCheckingCollectedTask {
        fn drop(&mut self) {
            let counters = self.state.counters.try_lock().is_some();
            let retained = self.state.retained.try_lock().is_some();
            self.drop_unlocked.store(counters && retained, Ordering::SeqCst);
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(TestClock::new()));
    let joined = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut slot = supervisor.try_reserve_slot().unwrap();
    slot.consumed = true;
    supervisor.state.retained.lock().push(Box::new(LockCheckingCollectedTask {
        state: supervisor.state.clone(),
        join_unlocked: joined.clone(),
        drop_unlocked: dropped.clone(),
    }));
    let collected = supervisor.collect_settled_retained();
    drop(slot);
    assert_eq!(collected, 1);
    assert!(joined.load(Ordering::SeqCst) && dropped.load(Ordering::SeqCst));
    assert_eq!(supervisor.live_tasks(), 0);
}

/// Closing admission preserves terminal unresolved custody rather than silently treating a late completion as normal collection.
#[test]
fn closed_admission_keeps_late_completed_retained_custody() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CollectableTestTask {
        id: 1,
        release: Some(release_rx),
        started: started_tx,
        complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        abandoned: abandoned.clone(),
        joined: Arc::new(AtomicUsize::new(0)),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(40), &cancel.token());
    let started = started_rx.recv_timeout(Duration::from_secs(1));
    supervisor.shutdown_handle().close_admission(Instant::now() + Duration::from_secs(1));
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(1);
    while abandoned.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        std::thread::yield_now();
    }
    let collected = supervisor.collect_settled_retained();
    let report = supervisor.shutdown(Instant::now() + Duration::from_secs(1), &cancel.token());
    let released = supervisor.release_retained();
    for handle in abandoned.lock().drain(..) {
        let _ = handle.join();
    }
    assert!(started.is_ok());
    assert_eq!(collected, 0, "closed admission leaves late custody for terminal release");
    assert_eq!((report.live_tasks, released), (1, 1));
    assert_eq!(report.unresolved_owners, vec![owner(1)]);
}

/// Preservation: a completed but Failed late helper is not collectable and keeps its owner named until terminal cleanup.
#[test]
fn late_failed_helper_remains_retained_until_terminal_release() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(CollectableTestTask {
        id: 1,
        release: Some(release_rx),
        started: started_tx,
        complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        abandoned: abandoned.clone(),
        joined: Arc::new(AtomicUsize::new(0)),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(40), &cancel.token());
    let started = started_rx.recv_timeout(Duration::from_secs(1));
    drop(release_tx);
    let until = Instant::now() + Duration::from_secs(1);
    while abandoned.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        std::thread::yield_now();
    }
    let collected = supervisor.collect_settled_retained();
    let report = supervisor.shutdown(Instant::now() + Duration::from_secs(1), &cancel.token());
    let released = supervisor.release_retained();
    let outcomes: Vec<_> =
        abandoned.lock().drain(..).map(|handle| handle.join().unwrap()).collect();
    assert!(started.is_ok());
    assert_eq!(outcomes, vec![ReapResult::Failed]);
    assert_eq!((collected, report.live_tasks, released), (0, 1, 1));
    assert_eq!(report.unresolved_owners, vec![owner(1)]);
}

/// Preservation: completion publication precedes thread return, so an unfinished outer handle keeps task custody.
#[test]
fn collection_waits_for_worker_exit_after_phase_success() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let abandoned = Arc::new(Mutex::new(Vec::new()));
    let joined = Arc::new(AtomicUsize::new(0));
    let (phase_tx, phase_rx) = std::sync::mpsc::sync_channel(1);
    let (return_tx, return_rx) = std::sync::mpsc::sync_channel(1);
    let worker_complete = complete.clone();
    let worker = std::thread::spawn(move || {
        worker_complete.store(true, Ordering::SeqCst);
        let _ = phase_tx.try_send(());
        let _ = return_rx.recv_timeout(Duration::from_secs(3));
        ReapResult::Settled
    });
    abandoned.lock().push(worker);
    let (unused_tx, _) = std::sync::mpsc::sync_channel(1);
    let slot = supervisor.try_reserve_slot().unwrap();
    slot.enqueue(Box::new(CollectableTestTask {
        id: 1,
        release: None,
        started: unused_tx,
        complete,
        abandoned: abandoned.clone(),
        joined: joined.clone(),
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now(), &cancel.token());
    let phase_finished = phase_rx.recv_timeout(Duration::from_secs(1));
    let early = supervisor.collect_settled_retained();
    let counted = supervisor.live_tasks();
    let _ = return_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(1);
    while abandoned.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
        std::thread::yield_now();
    }
    let late = supervisor.collect_settled_retained();
    assert!(phase_finished.is_ok());
    assert_eq!((early, counted), (0, 1), "completion flag alone cannot release custody");
    assert_eq!((late, joined.load(Ordering::SeqCst)), (1, 1));
    assert_eq!(supervisor.live_tasks(), 0);
}

/// Preservation: removed retained tasks return permits even if finished-helper join bookkeeping unwinds.
#[test]
fn panicking_retained_collection_returns_task_permits() {
    struct PanickingCollectTask;
    impl ReapTask for PanickingCollectTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            ReapAction::Complete(ReapResult::TimedOut)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            CancelOutcome::TimedOut
        }
        fn is_collectable(&self) -> bool {
            true
        }
        fn join_finished_helpers(&mut self) {
            panic!("retained join bookkeeping failed");
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(TestClock::new()));
    let mut slot = supervisor.try_reserve_slot().unwrap();
    // Install retained custody directly so this test isolates collector unwind rather than run_until's automatic collection.
    slot.consumed = true;
    supervisor.state.retained.lock().push(Box::new(PanickingCollectTask));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervisor.collect_settled_retained()
    }));
    drop(slot);
    assert!(failed.is_err());
    assert_eq!((supervisor.live_tasks(), supervisor.retained_tasks()), (0, 0));
    let _slot = supervisor.try_reserve_slot().expect("collector unwind returned admission");
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

/// The unresolved report names owners, so it is bounded by owner count.
///
/// One entry per unsettled *task* would grow with work rather than with the
/// number of owners in trouble, and the vector is cloned into every shutdown
/// report. Measured before this was deduplicated: 4,000 unsettled tasks from a
/// single owner produced 4,000 entries naming that one owner.
#[test]
fn the_unresolved_report_names_each_owner_once() {
    let clock = TestClock::new();
    // Enough task slots to enqueue several unsettled tasks from one owner.
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(8, 1, 2).unwrap(), Arc::new(clock.clone()));
    let cancel = CancelSource::new();
    let forced = Arc::new(AtomicUsize::new(0));

    const TASKS: usize = 6;
    for _ in 0..TASKS {
        supervisor.try_reserve_slot().unwrap().enqueue(Box::new(StuckTask {
            owner: owner(11),
            forced: forced.clone(),
            settles_on_force: false,
        }));
    }

    let report =
        supervisor.shutdown(deadline_from(&clock, Duration::from_secs(1)), &cancel.token());

    assert_eq!(
        report.unresolved_owners,
        vec![owner(11)],
        "{TASKS} unsettled tasks from one owner must name that owner once, not {TASKS} times"
    );
}

/// Releasing a handle that was never reserved is a bug, not a no-op.
///
/// The count exists to bound handles outstanding. Flooring an unpaired release
/// at zero keeps the count looking healthy while it no longer describes
/// reality, and admission then says yes past the ceiling: measured with a
/// ceiling of four, two reserved and five released left six outstanding.
#[test]
#[should_panic(expected = "released a native handle that was never reserved")]
fn releasing_an_unreserved_handle_is_caught() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);

    supervisor.try_reserve_handle().expect("the first handle is admitted");
    supervisor.release_handle();
    // Nothing is outstanding now, so this release is unpaired.
    supervisor.release_handle();
}

/// Handle admission stays bounded across balanced reserve/release cycles.
///
/// The property the count exists for: however many times handles are taken and
/// given back, the ceiling still refuses the one that would exceed it.
#[test]
fn handle_admission_respects_the_ceiling_after_balanced_cycles() {
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
    let ceiling = 2;

    for _ in 0..10 {
        supervisor.try_reserve_handle().expect("a handle is admitted");
        supervisor.release_handle();
    }

    let mut admitted = 0;
    while supervisor.try_reserve_handle().is_ok() {
        admitted += 1;
        assert!(admitted <= ceiling, "admitted {admitted} handles against a ceiling of {ceiling}");
    }
    assert_eq!(admitted, ceiling, "the ceiling must still admit exactly its limit");
    assert_eq!(supervisor.live_handles(), ceiling);
}
