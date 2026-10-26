pub(super) use super::test_support::slot_wait_started;
use super::test_support::*;
use super::*;
use crate::{
    cancel::CancelSource,
    clock::{SystemClock, TestClock},
};
use std::sync::atomic::{AtomicUsize, Ordering};

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
