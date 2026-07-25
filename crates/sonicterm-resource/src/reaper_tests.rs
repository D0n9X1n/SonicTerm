use super::*;
use crate::{cancel::CancelSource, clock::TestClock};
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
    let clock = TestClock::new();
    let supervisor = supervisor(&clock);
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
