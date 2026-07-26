use super::*;
use crate::{
    cancel::CancelSource,
    clock::{SystemClock, TestClock},
};
use sonicterm_types::CancelReason;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    assert_eq!(supervisor.live_tasks(), 0, "the slot is returned even when unresolved");
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

#[test]
fn shutdown_reports_unresolved_owners_rather_than_dropping_them() {
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
    assert_eq!(report.live_tasks, 0);
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
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        ResourceAmount::default(),
        "a settled owner holds nothing"
    );
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn shutdown_leaves_an_unsettled_owner_pinned_with_its_charge_visible() {
    // MM-06: when the supervisor gives up, the charge stays attributed to its
    // original owner and the owner stays in Closing. That is the defined
    // outcome, not a leak to be swept: an owner that cannot settle must remain
    // visible rather than being forced to Closed with work outstanding.
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
    assert_eq!(report.live_tasks, 0, "the slot is returned even though the work did not settle");

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

    // Surrendering releases the charge and unpins the whole subtree.
    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(supervisor.retained_tasks(), 0);
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount, ResourceAmount::default());
    governor.finish_close(pane).unwrap();
    governor.finish_close(window).unwrap();

    // The diagnosis is not retracted by the cleanup.
    assert_eq!(report.unresolved_owners, vec![pane]);
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
