//! One App-owned driver for reserved PTY teardown and explicit synchronous fallback.

use std::{
    sync::{Arc, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use sonicterm_io::pty::{
    PtyCompletion, PtyHandle, PtyTeardown, PTY_NATIVE_HANDLE_DEMAND, PTY_NATIVE_HELPER_DEMAND,
    PTY_TEARDOWN_TAIL_BOUND,
};
use sonicterm_resource::{
    CancelSource, CancelToken, CommittedReservation, HelperClaim, HelperGrant, ReapAction,
    ReapShutdownHandle, ReapSlot, ReapTask, ReapUnitDemand, ReaperLimits, ReaperSupervisor,
    ResourceGovernor, ShutdownReport, SystemClock, UnresolvedSink,
};
use sonicterm_types::{
    lifecycle::{NativePermit, NativeWorkerSpawner},
    CancelOutcome, CancelReason, OwnerKind, ReapAdmission, ReapResult, ResourceAmount,
    ResourceClass, ResourceOwnerId,
};

use super::{tracking_only_owner_limits, OwnerGuard};

const REAPER_MAX_TASKS: usize = 256;
#[cfg(windows)]
const REAPER_MAX_HELPERS: usize = 20;
#[cfg(not(windows))]
const REAPER_MAX_HELPERS: usize = 12;
const REAPER_MAX_HANDLES: usize = REAPER_MAX_TASKS * PTY_NATIVE_HANDLE_DEMAND;
const UNJOINED_RECHECK_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetirementPath {
    Reaper,
    Fallback,
}

struct GrantSpawner(HelperGrant);

impl NativeWorkerSpawner for GrantSpawner {
    fn spawn(
        &self,
        slot: usize,
        name: &'static str,
        work: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<JoinHandle<()>> {
        self.0.spawn(slot, name, work)
    }
}

struct TaskObservation {
    completion: PtyCompletion,
    abandoned: Mutex<Vec<JoinHandle<ReapResult>>>,
    sunk: std::sync::atomic::AtomicBool,
}

impl TaskObservation {
    // Completion releases its worker guard before abandoned is observed; neither guard survives native work.
    fn collectable(&self) -> bool {
        self.completion.is_complete()
            && self.completion.workers_finished()
            && self.abandoned.lock().iter().all(JoinHandle::is_finished)
    }

    // Completion releases its worker guard before abandoned handles are taken out and joined.
    fn join_finished(&self) {
        self.completion.join_finished_workers();
        let finished = {
            let mut handles = self.abandoned.lock();
            let mut finished = Vec::new();
            let mut index = 0;
            while index < handles.len() {
                if handles[index].is_finished() {
                    finished.push(handles.swap_remove(index));
                } else {
                    // When: is_finished is false, retain the outer helper rather than making its deadline advisory.
                    index += 1;
                }
            }
            finished
        };
        for handle in finished {
            if handle.join().is_err() {
                tracing::warn!("retained PTY teardown helper panicked");
            }
        }
    }
}

type Observations = Arc<Mutex<Vec<Weak<TaskObservation>>>>;

struct PtyCustody {
    teardown: PtyTeardown,
    _charge: CommittedReservation,
    owner: OwnerGuard,
    grant: Option<HelperGrant>,
    observation: Arc<TaskObservation>,
}

struct PtyReapState {
    custody: Option<PtyCustody>,
    sink: UnresolvedSink,
}

// Lifecycle: PtyReapState keeps incomplete teardown, charges, owner, and grant together in the unresolved sink.
impl Drop for PtyReapState {
    fn drop(&mut self) {
        let Some(mut custody) = self.custody.take() else {
            // When: custody was already transferred, nothing native remains for this holder to destroy.
            return;
        };
        if !custody.observation.collectable() {
            // When: collectable is false, the last holder gives unfinished phases one bounded final pass.
            let _ = custody.teardown.run_remaining();
        }
        if custody.observation.collectable() {
            custody.observation.join_finished();
        } else {
            // When: collectable remains false, preserve native custody and accounting until process exit instead of releasing early.
            custody.observation.sunk.store(true, std::sync::atomic::Ordering::SeqCst);
            self.sink.retain(custody.owner.id(), Box::new(custody));
        }
    }
}

struct PtyTask {
    state: Arc<Mutex<PtyReapState>>,
    observation: Arc<TaskObservation>,
    owner: ResourceOwnerId,
    grant: Option<HelperGrant>,
}

impl PtyTask {
    fn new(
        pty: PtyHandle,
        governor: &ResourceGovernor,
        sink: UnresolvedSink,
        wake: Sender<()>,
        fallback: bool,
    ) -> Self {
        let owner = governor
            .create_child(
                governor.root_owner(),
                OwnerKind::PtyTransport,
                tracking_only_owner_limits(),
            )
            .expect("retired native transport owner admission");
        let owner = OwnerGuard::new(governor.clone(), owner);
        let amount = ResourceAmount { bytes: 0, items: 1 };
        let charge = governor
            .try_reserve(owner.id(), ResourceClass::ReaperWork, amount)
            .expect("tracking-only native teardown charge")
            .commit(amount)
            .expect("exact native teardown charge commit");
        let mut teardown = pty.into_teardown();
        teardown.retain_for_supervisor();
        if fallback {
            let duplicate_sink = sink.clone();
            teardown.set_fallback_duplicate_tracker(Arc::new(move || {
                Box::new(duplicate_sink.track_fallback_duplicate())
            }));
        }
        let observation = Arc::new(TaskObservation {
            completion: teardown.completion(),
            abandoned: Mutex::new(Vec::new()),
            sunk: std::sync::atomic::AtomicBool::new(false),
        });
        observation.completion.set_wake(wake);
        let owner_id = owner.id();
        let state = Arc::new(Mutex::new(PtyReapState {
            custody: Some(PtyCustody {
                teardown,
                _charge: charge,
                owner,
                grant: None,
                observation: Arc::clone(&observation),
            }),
            sink,
        }));
        Self { state, observation, owner: owner_id, grant: None }
    }
}

impl ReapTask for PtyTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }

    fn next_action(&mut self, _now: Instant) -> ReapAction {
        if self.observation.collectable() {
            // When: collectable proves all phases and threads finished, join before reporting settlement.
            self.observation.join_finished();
            return ReapAction::Complete(ReapResult::Settled);
        }
        let state = Arc::clone(&self.state);
        ReapAction::RunBlocking(Box::new(move || {
            let mut state = state.lock();
            state.custody.as_mut().expect("owned PTY custody").teardown.run_remaining()
        }))
    }

    fn on_completion(&mut self, _result: ReapResult) {}

    fn force_cancel(&mut self) -> CancelOutcome {
        let Some(mut state) = self.state.try_lock() else {
            // When: try_lock refuses an active helper's payload, preserve custody without waiting for native work.
            return CancelOutcome::TimedOut;
        };
        let custody = state.custody.as_mut().expect("owned PTY custody");
        if self.observation.collectable() {
            // When: collectable includes actual thread exit, bounded cancellation can credit settlement after joining.
            self.observation.join_finished();
            return CancelOutcome::Settled;
        }
        custody.teardown.terminate_only();
        CancelOutcome::TimedOut
    }

    fn helper_claim(&self) -> HelperClaim {
        if self.grant.is_some() {
            HelperClaim::Held
        } else {
            // When: grant is absent, claim the entire native unit before starting any of its sibling workers.
            HelperClaim::Grant(PTY_NATIVE_HELPER_DEMAND)
        }
    }

    fn accept_helper_grant(&mut self, grant: HelperGrant) {
        let mut state = self.state.lock();
        let custody = state.custody.as_mut().expect("owned PTY custody");
        custody.teardown.set_worker_spawner(Arc::new(GrantSpawner(grant.clone())));
        custody.grant = Some(grant.clone());
        self.grant = Some(grant);
    }

    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.grant.clone()
    }

    fn keep_abandoned_helper(&mut self, handle: JoinHandle<ReapResult>) {
        self.observation.abandoned.lock().push(handle);
    }

    fn is_collectable(&self) -> bool {
        self.observation.collectable()
    }

    fn join_finished_helpers(&mut self) {
        self.observation.join_finished();
    }

    fn requeue_unstarted(&mut self) -> bool {
        if self.grant.is_some() {
            // When: grant exists, native work already started and belongs in retained custody, not carry-over.
            return false;
        }
        let _ = self.force_cancel();
        true
    }
}

fn needs_recheck(observations: &Observations) -> bool {
    let live = {
        let mut observations = observations.lock();
        let live: Vec<_> = observations.iter().filter_map(Weak::upgrade).collect();
        observations.retain(|entry| entry.strong_count() != 0);
        live
    };
    live.iter().any(|entry| {
        !entry.sunk.load(std::sync::atomic::Ordering::SeqCst) && entry.completion.is_complete()
    })
}

struct DriverSchedule {
    run_budget: Duration,
    recheck_interval: Duration,
    #[cfg(test)]
    runs: std::sync::atomic::AtomicUsize,
}

fn drive(
    supervisor: &ReaperSupervisor,
    wake: &Receiver<()>,
    control: &Receiver<Instant>,
    cancel: &CancelToken,
    observations: &Observations,
    schedule: &DriverSchedule,
) -> ShutdownReport {
    loop {
        supervisor.collect_settled_retained();
        match control.try_recv() {
            Ok(deadline) => {
                // When: control supplies deadline, stop normal runs and drain through the single driving thread.
                return supervisor.shutdown(deadline, cancel);
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                // When: control disconnects, the owner vanished and no future message can request its bounded drain.
                return supervisor.shutdown(Instant::now() + PTY_TEARDOWN_TAIL_BOUND, cancel);
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {
                // When: control is Empty, current readiness may advance normal work without a shutdown request.
            }
        }
        if supervisor.has_startable_work() {
            // When: has_startable_work is true after collection and control, a run can advance current work without another wake.
            #[cfg(test)]
            schedule.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            supervisor.run_until(Instant::now() + schedule.run_budget, cancel);
            continue;
        }
        let timer = if needs_recheck(observations) {
            crossbeam_channel::after(schedule.recheck_interval)
        } else {
            // When: needs_recheck is false, idle and terminal sink custody must not introduce a polling timer.
            crossbeam_channel::never()
        };
        crossbeam_channel::select! {
            recv(control) -> message => {
                let deadline = message.unwrap_or_else(|_| Instant::now() + PTY_TEARDOWN_TAIL_BOUND);
                return supervisor.shutdown(deadline, cancel);
            }
            recv(wake) -> _ => {}
            recv(timer) -> _ => {}
        }
    }
}

pub(super) struct ReaperDriver {
    supervisor: Arc<ReaperSupervisor>,
    shutdown: ReapShutdownHandle,
    governor: ResourceGovernor,
    sink: UnresolvedSink,
    wake: Sender<()>,
    control: Sender<Instant>,
    report: Receiver<ShutdownReport>,
    worker: Option<JoinHandle<()>>,
    cancel: CancelSource,
    observations: Observations,
    paths: Mutex<(usize, usize)>,
    finished: Option<bool>,
}

impl ReaperDriver {
    pub(super) fn new(governor: ResourceGovernor) -> Result<Self> {
        Self::with_limits(
            governor,
            ReaperLimits::new(REAPER_MAX_TASKS, REAPER_MAX_HELPERS, REAPER_MAX_HANDLES)
                .expect("nonzero native teardown ceilings"),
        )
    }

    pub(super) fn with_limits(governor: ResourceGovernor, limits: ReaperLimits) -> Result<Self> {
        if limits.max_helpers < PTY_NATIVE_HELPER_DEMAND
            || limits.max_handles < PTY_NATIVE_HANDLE_DEMAND
        {
            // Fixed limits below one native unit cannot safely start its sibling workers.
            bail!("native PTY teardown limits cannot admit one complete unit");
        }
        let supervisor = Arc::new(ReaperSupervisor::new(limits, Arc::new(SystemClock)));
        let shutdown = supervisor.shutdown_handle();
        let sink = supervisor.unresolved_sink();
        let (wake, wake_rx) = crossbeam_channel::bounded(1);
        let (control, control_rx) = crossbeam_channel::bounded(1);
        let (report_tx, report) = crossbeam_channel::bounded(1);
        let cancel = CancelSource::new();
        let cancel_token = cancel.token();
        let observations = Arc::new(Mutex::new(Vec::new()));
        let worker_observations = Arc::clone(&observations);
        let worker_supervisor = Arc::clone(&supervisor);
        let units_per_batch = limits.max_helpers / PTY_NATIVE_HELPER_DEMAND;
        let batches = limits.max_tasks.div_ceil(units_per_batch);
        let run_budget =
            PTY_TEARDOWN_TAIL_BOUND.saturating_mul(u32::try_from(batches).unwrap_or(u32::MAX));
        let schedule = DriverSchedule {
            run_budget,
            recheck_interval: UNJOINED_RECHECK_INTERVAL,
            #[cfg(test)]
            runs: std::sync::atomic::AtomicUsize::new(0),
        };
        let worker = thread::Builder::new()
            .name("sonic-pty-reaper".into())
            .spawn(move || {
                let report = drive(
                    &worker_supervisor,
                    &wake_rx,
                    &control_rx,
                    &cancel_token,
                    &worker_observations,
                    &schedule,
                );
                let _ = report_tx.try_send(report);
            })
            .context("start native PTY teardown driver")?;
        Ok(Self {
            supervisor,
            shutdown,
            governor,
            sink,
            wake,
            control,
            report,
            worker: Some(worker),
            cancel,
            observations,
            paths: Mutex::new((0, 0)),
            finished: None,
        })
    }

    pub(super) fn reserve(
        &self,
        pty: &mut PtyHandle,
    ) -> std::result::Result<ReapSlot, ReapAdmission> {
        let unit = self.supervisor.try_reserve_unit(ReapUnitDemand {
            helpers: PTY_NATIVE_HELPER_DEMAND,
            handles: PTY_NATIVE_HANDLE_DEMAND,
        })?;
        let (slot, permits) = unit.into_parts();
        pty.install_native_permits(
            permits.into_iter().map(|permit| Box::new(permit) as NativePermit).collect(),
        )
        .expect("native permit shape matches the PTY admission demand");
        Ok(slot)
    }

    // Lock order: observations releases before paths; neither lock is held during enqueue or native fallback destruction.
    pub(super) fn retire(&self, mut pty: PtyHandle, slot: Option<ReapSlot>) -> RetirementPath {
        let slot = slot.or_else(|| self.reserve(&mut pty).ok());
        let task =
            PtyTask::new(pty, &self.governor, self.sink.clone(), self.wake.clone(), slot.is_none());
        let owner_id = task.owner;
        self.observations.lock().push(Arc::downgrade(&task.observation));
        if let Some(slot) = slot {
            // When: slot exists, enqueue transfers the complete native unit without waiting for a phase on the caller.
            slot.enqueue(Box::new(task));
            self.paths.lock().0 += 1;
            let _ = self.wake.try_send(());
            RetirementPath::Reaper
        } else {
            // When: admission retry leaves no slot, the final state drop performs the explicit bounded synchronous fallback.
            self.paths.lock().1 += 1;
            tracing::warn!(
                owner = owner_id.get(),
                "PTY teardown admission refused; using bounded synchronous fallback"
            );
            drop(task);
            RetirementPath::Fallback
        }
    }

    pub(super) fn path_counts(&self) -> (usize, usize) {
        *self.paths.lock()
    }

    pub(super) fn finish(&mut self) -> bool {
        if let Some(settled) = self.finished {
            // When: finished is cached, shutdown and terminal release have already run exactly once.
            return settled;
        }
        let units = u32::try_from(self.supervisor.live_tasks().max(1)).unwrap_or(u32::MAX);
        let drain_by = Instant::now() + PTY_TEARDOWN_TAIL_BOUND.saturating_mul(units);
        self.shutdown.close_admission(drain_by);
        let _ = self.control.try_send(drain_by);
        let report = self.report.recv_deadline(drain_by).ok();
        if report.is_none() {
            // Cancellation after a missing report is an overrun abort, never the graceful shutdown signal.
            self.cancel.cancel(CancelReason::Timeout);
        }
        let join_by = Instant::now() + PTY_TEARDOWN_TAIL_BOUND;
        while self.worker.as_ref().is_some_and(|worker| !worker.is_finished())
            && Instant::now() < join_by
        {
            thread::sleep(Duration::from_millis(1));
        }
        let joined = if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.worker.take().expect("finished driver handle").join().is_ok()
        } else {
            // When: worker is not is_finished past join_by, detach only the driver while its supervisor retains every task.
            drop(self.worker.take());
            false
        };
        let clean_report = report.as_ref().is_some_and(ShutdownReport::is_clean);
        if joined {
            // A joined driver cannot race terminal payload release with another supervisor run.
            self.supervisor.release_retained();
        }
        let settled = joined
            && clean_report
            && self.supervisor.live_tasks() == 0
            && self.supervisor.live_helpers() == 0
            && self.supervisor.live_handles() == 0
            && self.sink.entries() == 0
            && self.sink.open_fallback_duplicates() == 0;
        let (reaper_closes, fallback_closes) = self.path_counts();
        tracing::info!(
            ?report,
            joined,
            settled,
            reaper_closes,
            fallback_closes,
            unresolved_entries = self.sink.entries(),
            open_fallback_duplicates = self.sink.open_fallback_duplicates(),
            "PTY reaper shutdown"
        );
        self.finished = Some(settled);
        settled
    }
}

// Lifecycle: ReaperDriver calls finish to join its worker and release retained custody; App retires still-installed panes.
impl Drop for ReaperDriver {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
#[path = "reaper_driver_tests.rs"]
mod reaper_driver_tests;
