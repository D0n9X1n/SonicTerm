use super::*;
use crate::reaper::test_support::*;
use crate::reaper::*;
use crate::{CancelSource, SystemClock, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};

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
