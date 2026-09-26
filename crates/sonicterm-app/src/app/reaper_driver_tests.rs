//! PTY retirement admission and the single-driver lifecycle.

use super::*;
use sonicterm_types::{GovernorLimits, ProcessKind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map::enum_map! { _ => usize::MAX },
            class_items: enum_map::enum_map! { _ => None },
        },
    )
    .unwrap()
}

/// A driver must refuse fixed limits that cannot admit one whole native PTY unit.
#[test]
fn driver_limits_reject_incomplete_native_unit() {
    #[cfg(windows)]
    let limits = [ReaperLimits::new(1, 4, 8).unwrap(), ReaperLimits::new(1, 5, 7).unwrap()];
    #[cfg(not(windows))]
    let limits = [ReaperLimits::new(1, 1, 2).unwrap()];
    for limits in limits {
        assert!(ReaperDriver::with_limits(governor(), limits).is_err());
    }
}

struct DriverFixture {
    supervisor: Arc<ReaperSupervisor>,
    wake: Sender<()>,
    control: Sender<Instant>,
    result: Receiver<ShutdownReport>,
    worker: Option<JoinHandle<()>>,
    cancel: CancelSource,
    schedule: Arc<DriverSchedule>,
}

impl DriverFixture {
    fn start(supervisor: Arc<ReaperSupervisor>, run_budget: Duration) -> Self {
        Self::with_observations(
            supervisor,
            run_budget,
            Arc::new(Mutex::new(Vec::new())),
            Duration::from_secs(60),
        )
    }

    fn with_observations(
        supervisor: Arc<ReaperSupervisor>,
        run_budget: Duration,
        observations: Observations,
        recheck_interval: Duration,
    ) -> Self {
        let (wake, wake_rx) = crossbeam_channel::bounded(1);
        let (control, control_rx) = crossbeam_channel::bounded(1);
        let (report_tx, result) = crossbeam_channel::bounded(1);
        let cancel = CancelSource::new();
        let token = cancel.token();
        let schedule =
            Arc::new(DriverSchedule { run_budget, recheck_interval, runs: AtomicUsize::new(0) });
        let shared = Arc::clone(&supervisor);
        let worker_schedule = Arc::clone(&schedule);
        let worker = thread::spawn(move || {
            let report =
                drive(&shared, &wake_rx, &control_rx, &token, &observations, &worker_schedule);
            let _ = report_tx.try_send(report);
        });
        Self { supervisor, wake, control, result, worker: Some(worker), cancel, schedule }
    }

    fn finish(&mut self) -> ShutdownReport {
        let deadline = Instant::now() + Duration::from_secs(2);
        self.supervisor.shutdown_handle().close_admission(deadline);
        let _ = self.control.try_send(deadline);
        let report = self.result.recv_timeout(Duration::from_secs(3)).expect("driver shutdown");
        self.worker.take().unwrap().join().expect("driver thread");
        report
    }
}

// Lifecycle: DriverFixture bounds an early assertion failure and signals its own driving thread before dropping it.
impl Drop for DriverFixture {
    fn drop(&mut self) {
        self.cancel.cancel(CancelReason::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(1);
        self.supervisor.shutdown_handle().close_admission(deadline);
        let _ = self.control.try_send(deadline);
        if let Some(worker) = self.worker.take() {
            let until = Instant::now() + Duration::from_secs(2);
            while !worker.is_finished() && Instant::now() < until {
                thread::sleep(Duration::from_millis(1));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

/// Wake hints without work must not start a supervisor run or create an idle polling loop.
#[test]
fn idle_driver_never_runs() {
    let supervisor =
        Arc::new(ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock)));
    let mut fixture = DriverFixture::start(supervisor, Duration::from_millis(20));
    fixture.wake.try_send(()).unwrap();
    assert!(fixture.result.recv_timeout(Duration::from_millis(30)).is_err());
    let runs = fixture.schedule.runs.load(Ordering::SeqCst);
    assert!(fixture.finish().is_clean());
    assert_eq!(runs, 0);
}

struct TestTask {
    owner: ResourceOwnerId,
    work: std::collections::VecDeque<Box<dyn FnOnce() -> ReapResult + Send>>,
    grant: Arc<Mutex<Option<HelperGrant>>>,
    complete: Arc<AtomicBool>,
    handles: Arc<Mutex<Vec<JoinHandle<ReapResult>>>>,
    joined: Arc<AtomicUsize>,
    requeue: Option<Box<dyn FnOnce() + Send>>,
}

impl ReapTask for TestTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::RunBlocking(self.work.pop_front().expect("test has a pending blocking call"))
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
        *self.grant.lock() = Some(grant);
    }
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.grant.lock().clone()
    }
    fn keep_abandoned_helper(&mut self, handle: JoinHandle<ReapResult>) {
        self.handles.lock().push(handle);
    }
    fn is_collectable(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
            && self.handles.lock().iter().all(JoinHandle::is_finished)
    }
    fn join_finished_helpers(&mut self) {
        for handle in std::mem::take(&mut *self.handles.lock()) {
            assert!(handle.is_finished());
            handle.join().unwrap();
            self.joined.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn requeue_unstarted(&mut self) -> bool {
        if self.grant.lock().is_some() {
            return false;
        }
        if let Some(requeue) = self.requeue.take() {
            requeue();
        }
        true
    }
}

fn test_task(id: u64, work: impl FnOnce() -> ReapResult + Send + 'static) -> TestTask {
    TestTask {
        owner: ResourceOwnerId::new(id).unwrap(),
        work: std::collections::VecDeque::from([
            Box::new(work) as Box<dyn FnOnce() -> ReapResult + Send>
        ]),
        grant: Arc::new(Mutex::new(None)),
        complete: Arc::new(AtomicBool::new(false)),
        handles: Arc::new(Mutex::new(Vec::new())),
        joined: Arc::new(AtomicUsize::new(0)),
        requeue: None,
    }
}

/// A task enqueued during a blocking call must run without needing an extra, unrelated wake.
#[test]
fn enqueue_during_run_not_lost() {
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(2, 10, 16).unwrap(),
        Arc::new(SystemClock),
    ));
    let mut fixture = DriverFixture::start(Arc::clone(&supervisor), Duration::from_secs(2));
    let (started_tx, started_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(test_task(1, move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        ReapResult::Settled
    })));
    fixture.wake.try_send(()).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).expect("first call started");
    let (second_tx, second_rx) = crossbeam_channel::bounded(1);
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(test_task(2, move || {
        second_tx.send(()).unwrap();
        ReapResult::Settled
    })));
    let observed = second_rx.recv_timeout(Duration::from_secs(1));
    let _ = release_tx.try_send(());
    assert!(fixture.finish().is_clean());
    assert!(observed.is_ok(), "mid-run enqueue was stranded");
}

/// The in-run collector can consume the last completion before returning; readiness must still start carried work.
#[test]
fn carry_over_starts_after_in_run_collection_frees_its_grant() {
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(2, 5, 16).unwrap(),
        Arc::new(SystemClock),
    ));
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let (started_tx, started_rx) = crossbeam_channel::bounded(1);
    let complete = Arc::new(AtomicBool::new(false));
    let complete_worker = Arc::clone(&complete);
    let mut first = test_task(1, move || {
        complete_worker.store(true, Ordering::SeqCst);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        ReapResult::Settled
    });
    first.complete = complete;
    let handles = Arc::clone(&first.handles);
    let joined = Arc::clone(&first.joined);
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(first));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(40), &cancel.token());
    started_rx.recv_timeout(Duration::from_secs(1)).expect("first task phase completed");
    assert_eq!(supervisor.retained_tasks(), 1);
    assert_eq!(supervisor.live_helpers(), 5);

    let (second_tx, second_rx) = crossbeam_channel::bounded(1);
    let mut second = test_task(2, move || {
        second_tx.send(()).unwrap();
        ReapResult::Settled
    });
    let (cutoff_tx, cutoff_rx) = crossbeam_channel::bounded(1);
    second.requeue = Some(Box::new(move || {
        release_tx.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while handles.lock().iter().any(|handle| !handle.is_finished()) && Instant::now() < until {
            thread::yield_now();
        }
        assert!(handles.lock().iter().all(JoinHandle::is_finished));
        cutoff_tx.send(()).unwrap();
    }));
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(second));
    let mut fixture = DriverFixture::start(Arc::clone(&supervisor), Duration::from_millis(40));
    cutoff_rx.recv_timeout(Duration::from_secs(2)).expect("second task crossed its cutoff");
    let started = second_rx.recv_timeout(Duration::from_secs(2));
    let report = fixture.finish();
    assert!(started.is_ok(), "carried work waited for an unrelated wake after in-run collection");
    assert!(report.is_clean());
    assert_eq!(joined.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.schedule.runs.load(Ordering::SeqCst), 2);
}

/// A Windows-shaped unit keeps its grant and live drain across closer refusal; the pending sibling starts only after retry settles.
#[test]
fn closer_spawn_failure_after_drain_start_settles_at_minimum() {
    struct RefuseCloserOnce {
        inner: GrantSpawner,
        refused: AtomicBool,
    }
    impl NativeWorkerSpawner for RefuseCloserOnce {
        fn spawn(
            &self,
            slot: usize,
            name: &'static str,
            work: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<JoinHandle<()>> {
            if slot == 2 && !self.refused.swap(true, Ordering::SeqCst) {
                return Err(std::io::Error::other("controlled closer refusal"));
            }
            self.inner.spawn(slot, name, work)
        }
    }
    let supervisor =
        Arc::new(ReaperSupervisor::new(ReaperLimits::new(2, 5, 8).unwrap(), Arc::new(SystemClock)));
    let (slot, permits) = supervisor
        .try_reserve_unit(ReapUnitDemand { helpers: 5, handles: 8 })
        .unwrap()
        .into_parts();
    let permits = Arc::new(Mutex::new(Some(permits)));
    let drain = Arc::new(Mutex::new(None::<JoinHandle<()>>));
    let spawner = Arc::new(Mutex::new(None::<Arc<RefuseCloserOnce>>));
    let (drained_tx, drained_rx) = crossbeam_channel::bounded(1);
    let (retry_tx, retry_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let (sibling_tx, sibling_rx) = crossbeam_channel::bounded(1);
    let drain_starts = Arc::new(AtomicUsize::new(0));
    let close_starts = Arc::new(AtomicUsize::new(0));
    let mut first = test_task(1, || ReapResult::Settled);
    let grant = Arc::clone(&first.grant);
    let first_spawner = Arc::clone(&spawner);
    let first_drain = Arc::clone(&drain);
    let first_drain_starts = Arc::clone(&drain_starts);
    first.work.clear();
    first.work.push_back(Box::new(move || {
        let native = Arc::new(RefuseCloserOnce {
            inner: GrantSpawner(grant.lock().as_ref().unwrap().clone()),
            refused: AtomicBool::new(false),
        });
        *first_drain.lock() = Some(
            native
                .spawn(
                    1,
                    "controlled-drain",
                    Box::new(move || {
                        first_drain_starts.fetch_add(1, Ordering::SeqCst);
                        drained_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    }),
                )
                .unwrap(),
        );
        let refused = native.spawn(
            2,
            "controlled-close",
            Box::new(|| {
                panic!("refused closer must not run");
            }),
        );
        *first_spawner.lock() = Some(native);
        assert!(refused.is_err());
        ReapResult::Failed
    }));
    let retry_close_starts = Arc::clone(&close_starts);
    first.work.push_back(Box::new(move || {
        retry_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let native = spawner.lock().take().unwrap();
        let closer = native
            .spawn(
                2,
                "controlled-close",
                Box::new(move || {
                    retry_close_starts.fetch_add(1, Ordering::SeqCst);
                    drop(permits.lock().take());
                    drained_tx.send(()).unwrap();
                }),
            )
            .unwrap();
        let drainer = drain.lock().take().unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while (!closer.is_finished() || !drainer.is_finished()) && Instant::now() < until {
            thread::yield_now();
        }
        assert!(closer.is_finished() && drainer.is_finished());
        closer.join().unwrap();
        drainer.join().unwrap();
        ReapResult::Settled
    }));
    slot.enqueue(Box::new(first));
    // The sibling needs helper admission only; the first unit holds the minimum native-handle capacity.
    supervisor.try_reserve_slot().unwrap().enqueue(Box::new(test_task(2, move || {
        sibling_tx.send(()).unwrap();
        ReapResult::Settled
    })));
    let mut fixture = DriverFixture::start(Arc::clone(&supervisor), Duration::from_secs(5));
    retry_rx.recv_timeout(Duration::from_secs(2)).expect("failed closer reached Held retry");
    let held = (supervisor.live_tasks(), supervisor.live_helpers(), supervisor.live_handles());
    let premature = sibling_rx.recv_timeout(Duration::from_millis(20));
    let _ = release_tx.try_send(());
    let sibling = sibling_rx.recv_timeout(Duration::from_secs(2));
    let report = fixture.finish();
    assert_eq!(held, (2, 5, 8));
    assert!(premature.is_err(), "pending sibling borrowed a partial unit");
    assert!(sibling.is_ok(), "Held retry did not free the grant for its pending sibling");
    assert_eq!(drain_starts.load(Ordering::SeqCst), 1);
    assert_eq!(close_starts.load(Ordering::SeqCst), 1);
    assert!(report.is_clean());
}

/// Completion discovered after collection must keep the driver awake even if its last worker has already exited.
#[test]
fn completed_observation_keeps_collection_scheduled() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    let pty = PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
    let pid = pty.pid().unwrap();
    super::super::pty_test_support::record_process(pid, true);
    let mut teardown = pty.into_teardown();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if teardown.run_remaining() == ReapResult::Settled {
            break;
        }
        assert!(Instant::now() < until, "owned observation fixture did not settle");
    }
    super::super::pty_test_support::record_process(pid, false);
    let observation = Arc::new(TaskObservation {
        completion: teardown.completion(),
        abandoned: Mutex::new(Vec::new()),
        sunk: AtomicBool::new(false),
    });
    let observations = Arc::new(Mutex::new(vec![Arc::downgrade(&observation)]));
    assert!(observation.collectable());
    assert!(needs_recheck(&observations), "finished custody lost its last collection wake");
    observation.sunk.store(true, Ordering::SeqCst);
    assert!(!needs_recheck(&observations), "terminal sink custody cannot be collected by polling");
}

fn native_task(
    supervisor: &ReaperSupervisor,
    governor: &ResourceGovernor,
) -> (ReapSlot, PtyTask, u32) {
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    let mut pty = PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
    let pid = pty.pid().unwrap();
    super::super::pty_test_support::record_process(pid, true);
    let (slot, permits) = supervisor
        .try_reserve_unit(ReapUnitDemand {
            helpers: PTY_NATIVE_HELPER_DEMAND,
            handles: PTY_NATIVE_HANDLE_DEMAND,
        })
        .unwrap()
        .into_parts();
    pty.install_native_permits(
        permits.into_iter().map(|permit| Box::new(permit) as NativePermit).collect(),
    )
    .unwrap();
    let (wake, _) = crossbeam_channel::bounded(1);
    let task = PtyTask::new(pty, governor, supervisor.unresolved_sink(), wake, false);
    (slot, task, pid)
}

struct ExitGatedPtyTask {
    inner: PtyTask,
    release: Option<Receiver<()>>,
    completed: Sender<()>,
}

impl ReapTask for ExitGatedPtyTask {
    fn owner(&self) -> ResourceOwnerId {
        self.inner.owner()
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        match (self.inner.next_action(now), self.release.take()) {
            (ReapAction::RunBlocking(work), Some(release)) => {
                let completed = self.completed.clone();
                ReapAction::RunBlocking(Box::new(move || {
                    let result = work();
                    if result == ReapResult::Settled {
                        completed.send(()).unwrap();
                        release.recv_timeout(Duration::from_secs(15)).unwrap();
                    }
                    result
                }))
            }
            (action, release) => {
                self.release = release;
                action
            }
        }
    }
    fn on_completion(&mut self, result: ReapResult) {
        self.inner.on_completion(result);
    }
    fn force_cancel(&mut self) -> CancelOutcome {
        self.inner.force_cancel()
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
    fn keep_abandoned_helper(&mut self, handle: JoinHandle<ReapResult>) {
        self.inner.keep_abandoned_helper(handle);
    }
    fn is_collectable(&self) -> bool {
        self.inner.is_collectable()
    }
    fn join_finished_helpers(&mut self) {
        self.inner.join_finished_helpers();
    }
    fn requeue_unstarted(&mut self) -> bool {
        self.inner.requeue_unstarted()
    }
}

/// Native phase completion keeps its task, charge, owner and grant until the outer worker exits and the timer collects it.
#[test]
fn late_native_completion_releases_retained_custody_without_another_wake() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    let governor = governor();
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(1, PTY_NATIVE_HELPER_DEMAND, PTY_NATIVE_HANDLE_DEMAND).unwrap(),
        Arc::new(SystemClock),
    ));
    let (slot, task, pid) = native_task(&supervisor, &governor);
    let owner = task.owner;
    let observation = Arc::clone(&task.observation);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let (complete_tx, complete_rx) = crossbeam_channel::bounded(1);
    slot.enqueue(Box::new(ExitGatedPtyTask {
        inner: task,
        release: Some(release_rx),
        completed: complete_tx,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(100), &cancel.token());
    complete_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("native phases completed before outer thread exit");
    assert!(observation.completion.is_complete());
    assert!(!observation.collectable());
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.live_helpers(), PTY_NATIVE_HELPER_DEMAND);
    assert_eq!(supervisor.live_handles(), 0, "native values closed before thread exit gate");
    let snapshot = governor.snapshot(owner).unwrap();
    assert_eq!(snapshot.owner_kind, OwnerKind::PtyTransport);
    assert_eq!(snapshot.parent, Some(governor.root_owner()));
    assert_eq!(snapshot.owner_class_items[ResourceClass::ReaperWork], 1);
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    let observations = Arc::new(Mutex::new(vec![Arc::downgrade(&observation)]));
    drop(observation);
    let mut fixture = DriverFixture::with_observations(
        Arc::clone(&supervisor),
        Duration::from_millis(30),
        observations,
        Duration::from_millis(5),
    );
    assert!(fixture.result.recv_timeout(Duration::from_millis(25)).is_err());
    assert_eq!(supervisor.live_tasks(), 1, "timer cannot collect a live outer worker");
    release_tx.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while supervisor.live_tasks() != 0 && Instant::now() < until {
        thread::yield_now();
    }
    let tasks = supervisor.live_tasks();
    let report = fixture.finish();
    assert_eq!(tasks, 0, "late completion required an unrelated wake or terminal release");
    assert!(report.is_clean());
    assert_eq!(
        governor.snapshot(governor.root_owner()).unwrap().process_class_items
            [ResourceClass::ReaperWork],
        0
    );
    assert!(!governor
        .snapshot(owner)
        .is_ok_and(|snapshot| snapshot.owner_state != sonicterm_types::OwnerState::Closed));
    super::super::pty_test_support::record_process(pid, false);
}

/// Shutdown handles control while a completed native unit still has a live helper, retaining its charge and grant.
#[test]
fn shutdown_reports_live_helper_and_retains_terminal_custody() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    let governor = governor();
    let supervisor = Arc::new(ReaperSupervisor::new(
        ReaperLimits::new(1, PTY_NATIVE_HELPER_DEMAND, PTY_NATIVE_HANDLE_DEMAND).unwrap(),
        Arc::new(SystemClock),
    ));
    let (slot, task, pid) = native_task(&supervisor, &governor);
    let owner = task.owner;
    let observation = Arc::clone(&task.observation);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let (complete_tx, complete_rx) = crossbeam_channel::bounded(1);
    slot.enqueue(Box::new(ExitGatedPtyTask {
        inner: task,
        release: Some(release_rx),
        completed: complete_tx,
    }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_millis(100), &cancel.token());
    complete_rx.recv_timeout(Duration::from_secs(10)).expect("native phases completed");
    let observations = Arc::new(Mutex::new(vec![Arc::downgrade(&observation)]));
    let mut fixture = DriverFixture::with_observations(
        Arc::clone(&supervisor),
        Duration::from_millis(30),
        observations,
        Duration::from_millis(5),
    );
    let report = fixture.finish();
    let retained = supervisor.release_retained();
    let task_count = supervisor.live_tasks();
    let helpers = supervisor.live_helpers();
    let sink_entries = supervisor.unresolved_sink().entries();
    let charge = governor.snapshot(owner).unwrap().owner_class_items[ResourceClass::ReaperWork];
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(2);
    while !observation.collectable() && Instant::now() < until {
        thread::yield_now();
    }
    observation.join_finished();
    super::super::pty_test_support::record_process(pid, false);
    assert!(!report.is_clean());
    assert!(report.unresolved_owners.contains(&owner));
    assert_eq!((retained, task_count, sink_entries, charge), (1, 1, 1, 1));
    assert_eq!(helpers, PTY_NATIVE_HELPER_DEMAND);
    assert!(observation.sunk.load(Ordering::SeqCst));
    assert!(observation.collectable(), "the owned helper did not exit after fixture release");
    assert_eq!(supervisor.collect_settled_retained(), 0);
    assert_eq!(supervisor.unresolved_sink().entries(), 1);
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.live_helpers(), PTY_NATIVE_HELPER_DEMAND);
    assert_eq!(
        governor.snapshot(owner).unwrap().owner_class_items[ResourceClass::ReaperWork],
        1,
        "terminal sink custody survives even after its last worker exits"
    );
}

/// Admission saturation takes the explicit slotless fallback and its counters stay separate from reserved retirement.
#[test]
fn slotless_fallback_settles_without_borrowing_reserved_capacity() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    let governor = governor();
    let mut driver = ReaperDriver::with_limits(
        governor.clone(),
        ReaperLimits::new(1, PTY_NATIVE_HELPER_DEMAND, PTY_NATIVE_HANDLE_DEMAND).unwrap(),
    )
    .unwrap();
    let held = driver.supervisor.try_reserve_slot().unwrap();
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    let pty = PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
    let pid = pty.pid().unwrap();
    super::super::pty_test_support::record_process(pid, true);
    assert_eq!(driver.retire(pty, None), RetirementPath::Fallback);
    assert_eq!(driver.path_counts(), (0, 1));
    assert_eq!(driver.supervisor.live_tasks(), 1, "only the preexisting reservation remains");
    assert_eq!(driver.supervisor.live_helpers(), 0);
    assert_eq!(driver.supervisor.live_handles(), 0);
    assert_eq!(driver.sink.open_fallback_duplicates(), 0);
    assert_eq!(
        governor.snapshot(governor.root_owner()).unwrap().process_class_items
            [ResourceClass::ReaperWork],
        0
    );
    drop(held);
    assert!(driver.finish());
    super::super::pty_test_support::record_process(pid, false);
}

#[cfg(windows)]
struct FailingTask(PtyTask);

#[cfg(windows)]
impl ReapTask for FailingTask {
    fn owner(&self) -> ResourceOwnerId {
        self.0.owner()
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        ReapAction::Complete(ReapResult::Failed)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        self.0.force_cancel()
    }
    fn is_collectable(&self) -> bool {
        self.0.is_collectable()
    }
    fn join_finished_helpers(&mut self) {
        self.0.join_finished_helpers();
    }
}

#[cfg(windows)]
struct RefuseNativeWorkers;

#[cfg(windows)]
impl NativeWorkerSpawner for RefuseNativeWorkers {
    fn spawn(
        &self,
        _slot: usize,
        _name: &'static str,
        _work: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<JoinHandle<()>> {
        Err(std::io::Error::other("controlled native worker refusal"))
    }
}

/// A final failed ConPTY pass retains real native values, their permits, the transport owner and charge until process exit.
#[cfg(windows)]
#[test]
fn final_drop_of_incomplete_payload_keeps_custody() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    let governor = governor();
    let supervisor = ReaperSupervisor::new(
        ReaperLimits::new(1, PTY_NATIVE_HELPER_DEMAND, PTY_NATIVE_HANDLE_DEMAND).unwrap(),
        Arc::new(SystemClock),
    );
    let (slot, task, pid) = native_task(&supervisor, &governor);
    let owner = task.owner;
    let observation = Arc::clone(&task.observation);
    task.state
        .lock()
        .custody
        .as_mut()
        .unwrap()
        .teardown
        .set_worker_spawner(Arc::new(RefuseNativeWorkers));
    slot.enqueue(Box::new(FailingTask(task)));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_secs(1), &cancel.token());
    assert_eq!(supervisor.retained_tasks(), 1);
    let started = Instant::now();
    assert_eq!(supervisor.release_retained(), 1);
    assert!(started.elapsed() < PTY_TEARDOWN_TAIL_BOUND + Duration::from_secs(1));
    assert_eq!(supervisor.live_tasks(), 1, "sink entry replaces the released task permit");
    assert!(supervisor.live_handles() > 0, "undrained master still owns its native permits");
    assert_eq!(supervisor.unresolved_sink().entries(), 1);
    assert!(observation.sunk.load(Ordering::SeqCst));
    assert!(!needs_recheck(&Arc::new(Mutex::new(vec![Arc::downgrade(&observation)]))));
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    let snapshot = governor.snapshot(owner).unwrap();
    assert_eq!(snapshot.owner_class_items[ResourceClass::ReaperWork], 1);
    assert_eq!(snapshot.parent, Some(governor.root_owner()));
    let report = supervisor.shutdown(Instant::now(), &cancel.token());
    assert!(!report.is_clean());
    assert_eq!(report.unresolved_entries, 1);
    assert!(report.unresolved_owners.contains(&owner));
    // The isolated child process owns intentionally retained native state until its OS exit.
    super::super::pty_test_support::record_process(pid, false);
}

/// Repeated empty shutdown returns the same clean terminal disposition without a second driver join.
#[test]
fn empty_shutdown_is_clean_and_idempotent() {
    let mut driver = ReaperDriver::new(governor()).unwrap();
    assert!(driver.finish());
    assert!(driver.finish());
    assert_eq!(driver.path_counts(), (0, 0));
}
