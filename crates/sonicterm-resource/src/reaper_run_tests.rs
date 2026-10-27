use super::*;
use crate::reaper::test_support::*;
use crate::reaper::*;
use crate::{CancelSource, SystemClock, TestClock};
use sonicterm_types::CancelReason;
use std::sync::atomic::{AtomicUsize, Ordering};

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
