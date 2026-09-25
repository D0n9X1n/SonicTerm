//! Bounded reaper supervisor for resources that outlive their owner's close.

use crate::{cancel::CancelToken, clock::Clock};
use parking_lot::{Condvar, Mutex};
use sonicterm_types::{CancelOutcome, ReapAdmission, ReapResult, ResourceOwnerId};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

/// How long the loop waits between checks when every helper is still running.
///
/// Short enough that a finished call is collected promptly, long enough that
/// waiting costs nothing measurable.
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Delay before a blocking call that did not settle is attempted again.
///
/// Without it a failing call is respawned on the next pass, which turns a
/// persistent failure into a thread-spawn storm rather than a retry.
const FAILED_CALL_BACKOFF: Duration = Duration::from_millis(10);

#[cfg(test)]
std::thread_local! {
    // Keep repeated spawn failures local to the test driving this supervisor.
    static FAIL_HELPER_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Fixed ceilings for one supervisor.
///
/// Limits are immutable after construction so admission cannot be widened while
/// work is in flight.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ReaperLimits {
    /// Task custody ceiling, including reservations and retained unsettled tasks.
    pub max_tasks: usize,
    /// Reserved helper capacity, including whole grants retained between calls; not a running-thread count.
    pub max_helpers: usize,
    /// Native handles the supervisor may own at once.
    pub max_handles: usize,
}

impl ReaperLimits {
    /// Create limits, rejecting a zero ceiling.
    ///
    /// A zero ceiling disables task admission, blocking work, or handle ownership,
    /// so it is refused at construction rather than creating a partial supervisor.
    pub fn new(max_tasks: usize, max_helpers: usize, max_handles: usize) -> Option<Self> {
        if max_tasks == 0 || max_helpers == 0 || max_handles == 0 {
            // When: max_tasks, max_helpers, or max_handles is zero, disabling one
            // supervisor axis; reject the partial limit set as unusable.
            return None;
        }
        Some(Self { max_tasks, max_helpers, max_handles })
    }
}

/// Minimum worker and native-handle capacity needed by one whole transport teardown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReapUnitDemand {
    /// Simultaneous helper slots reserved together when native work starts.
    pub helpers: usize,
    /// Native handles reserved together with the task before retirement.
    pub handles: usize,
}

/// What a task wants the supervisor to do next.
pub enum ReapAction {
    /// Re-poll no earlier than this instant. Drives a timer wait, never a spin.
    PollAfter(Instant),
    /// Run this blocking call on a bounded helper, never on the poll loop.
    RunBlocking(Box<dyn FnOnce() -> ReapResult + Send>),
    /// Terminal disposition.
    Complete(ReapResult),
}

impl core::fmt::Debug for ReapAction {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PollAfter(at) => formatter.debug_tuple("PollAfter").field(at).finish(),
            Self::RunBlocking(_) => formatter.write_str("RunBlocking(..)"),
            Self::Complete(result) => formatter.debug_tuple("Complete").field(result).finish(),
        }
    }
}

/// Helper admission requested by a task's next blocking call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperClaim {
    /// Claim one helper for this call, preserving the ordinary task contract.
    PerCall,
    /// Claim every worker slot for a complete native unit together.
    Grant(usize),
    /// Reuse the task's previously installed grant without incrementing helper counts.
    Held,
}

/// Work the supervisor drives to a terminal disposition.
pub trait ReapTask: Send + 'static {
    /// Owner this task's resources remain charged to.
    fn owner(&self) -> ResourceOwnerId;

    /// Decide the next step.
    fn next_action(&mut self, now: Instant) -> ReapAction;

    /// Record a completed step.
    fn on_completion(&mut self, result: ReapResult);

    /// Force cancellation, returning whether the resource actually settled.
    fn force_cancel(&mut self) -> CancelOutcome;

    /// Describe helper capacity needed for the next call; ordinary tasks use one helper per call.
    fn helper_claim(&self) -> HelperClaim {
        HelperClaim::PerCall
    }

    /// Retain an admitted whole-unit grant; called only after the counter lock is released.
    fn accept_helper_grant(&mut self, _grant: HelperGrant) {}

    /// Clone the task's retained grant for a retry on the same worker slot.
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        None
    }

    /// Keep an unfinished outer helper when a run ends; ordinary tasks retain their historical detach behavior.
    fn keep_abandoned_helper(&mut self, handle: std::thread::JoinHandle<ReapResult>) {
        drop(handle);
    }

    /// Report whole-task completion with every retained worker already finished, without taking a payload lock.
    fn is_collectable(&self) -> bool {
        false
    }

    /// Join only finished helpers after the collector has released its retained-task lock.
    fn join_finished_helpers(&mut self) {}

    /// Preserve an unstarted unit at a normal run cutoff after bounded termination; ordinary tasks remain terminal.
    fn requeue_unstarted(&mut self) -> bool {
        false
    }

    /// Give up whatever charges this task still holds.
    ///
    /// Called during terminal cleanup, after cancellation has already failed.
    ///
    /// The default does nothing, which is sufficient for a task whose charges
    /// are released by its own drop — dropping the task runs the RAII release
    /// on any reservation it owns. Override this when a charge would outlive
    /// the drop: one transferred to another owner, or one behind a handle
    /// shared with a thread that is still running.
    fn surrender_charges(&mut self) {}
}

struct Counters {
    tasks: usize,
    helpers: usize,
    handles: usize,
    admitting: bool,
    drain_by: Option<Instant>,
    unresolved: Vec<ResourceOwnerId>,
}

struct QueuedTask {
    task: Box<dyn ReapTask>,
    work: Option<Box<dyn FnOnce() -> ReapResult + Send>>,
}

#[derive(Default)]
struct TaskQueue {
    ready: VecDeque<QueuedTask>,
    carry_over: VecDeque<QueuedTask>,
}

struct SupervisorState {
    counters: Mutex<Counters>,
    slot_released: Condvar,
    queue: Mutex<TaskQueue>,
    /// Tasks that finished without settling.
    ///
    /// Their resources stay charged to the original owner until terminal
    /// cleanup, so the supervisor keeps the task alive rather than dropping it.
    /// Dropping would run the task's RAII release and quietly zero an owner the
    /// shutdown report is simultaneously naming as unresolved.
    retained: Mutex<Vec<Box<dyn ReapTask>>>,
    limits: ReaperLimits,
}

/// Returns task permits only after removed retained tasks and their fields finish destruction.
struct ReleaseTaskPermits<'a> {
    state: &'a SupervisorState,
    count: usize,
}

// Lifecycle: ReleaseTaskPermits decrements tasks and notifies slot_released after retained destruction, including unwind.
impl Drop for ReleaseTaskPermits<'_> {
    fn drop(&mut self) {
        self.state.counters.lock().tasks -= self.count;
        self.state.slot_released.notify_all();
    }
}

/// Returns a helper slot when its call ends, however it ends.
///
/// A slot released by a statement after the call is lost when the call panics,
/// which pins the pool one slot smaller for the life of the process. Dropping
/// runs on both paths.
struct HelperSlot {
    state: Arc<SupervisorState>,
}

// Lifecycle: HelperSlot Drop decrements state counters helpers through lock when
// its helper call ends, including after the supervisor abandons that call.
impl Drop for HelperSlot {
    fn drop(&mut self) {
        self.state.counters.lock().helpers -= 1;
    }
}

/// Shared helper permits for one complete native unit, retained across all of its workers and retries.
#[derive(Clone)]
pub struct HelperGrant {
    inner: Arc<HelperGrantState>,
}

struct HelperGrantState {
    state: Arc<SupervisorState>,
    occupied: Vec<std::sync::atomic::AtomicBool>,
}

// Lifecycle: HelperGrantState decrements helpers for the whole grant after the task and every worker release their clones.
impl Drop for HelperGrantState {
    fn drop(&mut self) {
        self.state.counters.lock().helpers -= self.occupied.len();
        self.state.slot_released.notify_all();
    }
}

struct GrantWorkerSlot {
    grant: HelperGrant,
    slot: usize,
}

// Lifecycle: GrantWorkerSlot clears its occupied worker slot on success, unwind or failed spawn before releasing the grant clone.
impl Drop for GrantWorkerSlot {
    fn drop(&mut self) {
        self.grant.inner.occupied[self.slot].store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl HelperGrant {
    /// Number of worker slots that this grant keeps reserved for its whole lifetime.
    pub fn capacity(&self) -> usize {
        self.inner.occupied.len()
    }

    /// Start a worker in one named grant slot; slot zero is reserved for the outer teardown helper.
    ///
    /// Refusal or spawn failure drops the closure, so callers retain recoverable native values in shared slots.
    pub fn spawn<T: Send + 'static>(
        &self,
        slot: usize,
        name: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<std::thread::JoinHandle<T>> {
        let Some(occupied) = self.inner.occupied.get(slot) else {
            // When: slot is outside the complete grant, refuse unaccounted helper creation.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker slot is outside its grant",
            ));
        };
        if occupied
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            // When: occupied is already true, retry cannot run a second worker in the same native phase slot.
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "worker grant slot is occupied",
            ));
        }
        let worker_slot = GrantWorkerSlot { grant: self.clone(), slot };
        let scoped = move || {
            let _slot = worker_slot;
            work()
        };
        #[cfg(test)]
        if FAIL_HELPER_SPAWN.get() {
            // When: FAIL_HELPER_SPAWN is active, drop the closure just as an OS refusal would, retaining the task's grant.
            drop(scoped);
            return Err(std::io::Error::other("injected helper spawn failure"));
        }
        std::thread::Builder::new().name(name.to_owned()).spawn(scoped)
    }
}

/// Proof that a reaper slot was reserved before work began.
///
/// The slot is acquired *before* starting an operation that may need handoff, so
/// a caller can never reach the point of needing the reaper only to find it full.
/// Dropping the slot without enqueueing returns it, which is what makes the
/// synchronous-completion path safe.
pub struct ReapSlot {
    state: Arc<SupervisorState>,
    consumed: bool,
}

impl ReapSlot {
    /// Hand a task to the supervisor, transferring ownership.
    pub fn enqueue(mut self, task: Box<dyn ReapTask>) {
        self.state.queue.lock().ready.push_back(QueuedTask { task, work: None });
        self.consumed = true;
    }
}

impl core::fmt::Debug for ReapSlot {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("ReapSlot").field("consumed", &self.consumed).finish()
    }
}

// Lifecycle: ReapSlot Drop decrements counters tasks and calls slot_released
// notify_one unless enqueue marked consumed and transferred settlement ownership.
impl Drop for ReapSlot {
    fn drop(&mut self) {
        if !self.consumed {
            let mut counters = self.state.counters.lock();
            counters.tasks -= 1;
            self.state.slot_released.notify_one();
        }
    }
}

/// One native handle's reservation, retained until its particular native owner releases it.
pub struct ReapHandlePermit {
    state: Arc<SupervisorState>,
}

// Lifecycle: ReapHandlePermit returns one handles count after its native owner's field is closed.
impl Drop for ReapHandlePermit {
    fn drop(&mut self) {
        self.state.counters.lock().handles -= 1;
        self.state.slot_released.notify_all();
    }
}

/// Atomic task-and-native-handle admission for one transport.
pub struct ReapUnit {
    slot: ReapSlot,
    handles: Vec<ReapHandlePermit>,
}

impl ReapUnit {
    /// Separate task custody from individual handle permits so each can follow its actual native owner.
    pub fn into_parts(self) -> (ReapSlot, Vec<ReapHandlePermit>) {
        (self.slot, self.handles)
    }
}

/// Close admission and shorten a driver's drain deadline without running a second supervisor loop.
#[derive(Clone)]
pub struct ReapShutdownHandle {
    state: Arc<SupervisorState>,
}

impl ReapShutdownHandle {
    /// Stop reservations, lower the shared drain deadline, and wake capacity waiters.
    ///
    /// Closing admission does not cancel running work. A later call may shorten,
    /// but never extend, the deadline already observed by the driver.
    pub fn close_admission(&self, drain_by: Instant) {
        let mut counters = self.state.counters.lock();
        counters.admitting = false;
        counters.drain_by =
            Some(counters.drain_by.map_or(drain_by, |current| current.min(drain_by)));
        drop(counters);
        self.state.slot_released.notify_all();
    }
}

/// Process-wide supervisor with fixed task, helper, and handle ceilings.
pub struct ReaperSupervisor {
    state: Arc<SupervisorState>,
    clock: Arc<dyn Clock>,
}

impl ReaperSupervisor {
    /// Create a supervisor.
    pub fn new(limits: ReaperLimits, clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(SupervisorState {
                counters: Mutex::new(Counters {
                    tasks: 0,
                    helpers: 0,
                    handles: 0,
                    admitting: true,
                    drain_by: None,
                    unresolved: Vec::new(),
                }),
                slot_released: Condvar::new(),
                queue: Mutex::new(TaskQueue::default()),
                retained: Mutex::new(Vec::new()),
                limits,
            }),
            clock,
        }
    }

    /// Return a control handle that closes admission without driving queued tasks.
    pub fn shutdown_handle(&self) -> ReapShutdownHandle {
        ReapShutdownHandle { state: self.state.clone() }
    }

    /// Bound this run by the earliest drain deadline published by an exit thread.
    fn effective_deadline(&self, run_deadline: Instant) -> Instant {
        self.state
            .counters
            .lock()
            .drain_by
            .map_or(run_deadline, |drain_by| run_deadline.min(drain_by))
    }

    /// Try to reserve a slot before starting cancellable work.
    ///
    /// A refusal leaves the caller owning whatever it holds: it must complete
    /// synchronously or retry. Returning an error while abandoning the resource
    /// is not an option the API offers.
    pub fn try_reserve_slot(&self) -> Result<ReapSlot, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            // When: counters admitting is false after shutdown; refusal preserves
            // caller ownership instead of creating unreported work.
            return Err(ReapAdmission::ShuttingDown);
        }
        if counters.tasks >= self.state.limits.max_tasks {
            // When: counters tasks reaches state limits max_tasks; try-reserve
            // returns immediately while the caller still owns its resource.
            return Err(ReapAdmission::QueueFull);
        }
        counters.tasks += 1;
        Ok(ReapSlot { state: self.state.clone(), consumed: false })
    }

    /// Reserve a task and all native-handle permits together, or leave every counter unchanged.
    ///
    /// Helper capacity is validated here and claimed as a whole grant only when
    /// the task starts; live panes therefore do not occupy worker threads.
    pub fn try_reserve_unit(&self, demand: ReapUnitDemand) -> Result<ReapUnit, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            // When: admitting is false, preserve caller custody and refuse the entire unit after shutdown.
            return Err(ReapAdmission::ShuttingDown);
        }
        if demand.helpers > self.state.limits.max_helpers
            || demand.handles > self.state.limits.max_handles
        {
            // When: helpers or handles cannot fit the immutable limits, retrying admission cannot make the unit usable.
            return Err(ReapAdmission::BelowMinimumCapacity);
        }
        if counters.tasks >= self.state.limits.max_tasks
            || demand.handles > self.state.limits.max_handles.saturating_sub(counters.handles)
        {
            // When: tasks or handles are saturated, refuse before incrementing either axis so no partial unit leaks capacity.
            return Err(ReapAdmission::QueueFull);
        }
        // Reserve the vector before mutating counters; individual permit construction cannot allocate afterward.
        let mut handles = Vec::with_capacity(demand.handles);
        counters.tasks += 1;
        counters.handles += demand.handles;
        drop(counters);
        handles.extend((0..demand.handles).map(|_| ReapHandlePermit { state: self.state.clone() }));
        Ok(ReapUnit { slot: ReapSlot { state: self.state.clone(), consumed: false }, handles })
    }

    /// Wait for a slot until the deadline.
    pub fn reserve_slot_until(&self, deadline: Instant) -> Result<ReapSlot, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        loop {
            if !counters.admitting {
                // When: counters admitting is false after an unlocked wait;
                // shutdown cannot issue a new slot after closing the gate.
                return Err(ReapAdmission::ShuttingDown);
            }
            if counters.tasks < self.state.limits.max_tasks {
                // When: counters tasks is below state limits max_tasks; claim
                // capacity under the guard so another waiter cannot take it.
                counters.tasks += 1;
                return Ok(ReapSlot { state: self.state.clone(), consumed: false });
            }
            #[cfg(test)]
            reaper_tests::slot_wait_started(&mut counters);
            if self.state.slot_released.wait_until(&mut counters, deadline).timed_out()
                && counters.admitting
                && counters.tasks >= self.state.limits.max_tasks
            {
                // When: timeout finds admitting still true and tasks full; a racing close instead loops to ShuttingDown.
                return Err(ReapAdmission::QueueFull);
            }
        }
    }

    /// Reserve a native handle against the supervisor ceiling.
    pub fn try_reserve_handle(&self) -> Result<(), ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            // When: counters admitting is false; only release_handle decrements
            // handles, so a late reservation would remain visible in live_handles.
            return Err(ReapAdmission::ShuttingDown);
        }
        if counters.handles >= self.state.limits.max_handles {
            // When: counters handles reaches state limits max_handles; this API
            // cannot wait because release_handle has no wake signal.
            return Err(ReapAdmission::QueueFull);
        }
        counters.handles += 1;
        Ok(())
    }

    /// Release a previously reserved native handle.
    ///
    /// Each release must pair with a `try_reserve_handle` that succeeded. An
    /// unpaired release would drive the count below zero, and flooring it at
    /// zero silently converts that into permanent extra headroom: the counter
    /// no longer describes the handles outstanding, so admission keeps saying
    /// yes past the ceiling. Measured with a ceiling of four handles, two
    /// reserved and five released left six outstanding against a limit the
    /// supervisor still reported itself as meeting.
    ///
    /// The debug assertion turns that into a test failure where tests run,
    /// while release builds still floor rather than wrap — an over-count is
    /// wrong, but a wrapped count would admit `usize::MAX` handles.
    pub fn release_handle(&self) {
        let mut counters = self.state.counters.lock();
        debug_assert!(
            counters.handles > 0,
            "released a native handle that was never reserved; the handle count no longer \
             describes what is outstanding"
        );
        counters.handles = counters.handles.saturating_sub(1);
    }

    /// Drive queued tasks until each reaches a terminal disposition or the
    /// deadline elapses.
    ///
    /// Blocking work never runs inline: `RunBlocking` is dispatched to a bounded
    /// helper and the loop keeps servicing other tasks while it runs.
    pub fn run_until(&self, deadline: Instant, cancel: &CancelToken) -> ReaperProgress {
        {
            let mut queue = self.state.queue.lock();
            let mut carried = std::mem::take(&mut queue.carry_over);
            carried.append(&mut queue.ready);
            queue.ready = carried;
        }
        let mut progress = ReaperProgress::default();
        // Tasks that asked to be polled later wait here rather than cycling
        // through the queue, so a deferred task cannot spin the loop.
        let mut deferred: Vec<(Instant, Box<dyn ReapTask>)> = Vec::new();
        // Blocking calls running on helpers, with the task awaiting each.
        let mut in_flight: Vec<(std::thread::JoinHandle<ReapResult>, Box<dyn ReapTask>)> =
            Vec::new();
        // Calls that found no free helper, held with their task so the same
        // call is retried rather than re-requested.
        #[allow(clippy::type_complexity)]
        let mut pending_work: Vec<(
            Box<dyn FnOnce() -> ReapResult + Send>,
            Box<dyn ReapTask>,
        )> = Vec::new();
        loop {
            // Late completed retained units return their grants before pending calls try to claim that capacity.
            progress.settled += self.collect_settled_retained();
            // Collect helpers that finished since the last pass. Only finished
            // handles are joined, so collecting never blocks the loop.
            let mut still_running = Vec::with_capacity(in_flight.len());
            for (handle, mut task) in in_flight.drain(..) {
                if !handle.is_finished() {
                    // When: handle is not is_finished; joining it would hand the
                    // deadline to a native call that may never return.
                    still_running.push((handle, task));
                    continue;
                }
                let result = handle.join().unwrap_or(ReapResult::Failed);
                task.on_completion(result);
                if result.releases_charge() {
                    self.settle(task, result, &mut progress);
                } else {
                    // When: result does not releases_charge; defer with retry_at
                    // instead of settling or immediately respawning the call.
                    deferred.push((self.retry_at(), task));
                }
            }
            in_flight = still_running;

            // Retry calls that found no helper earlier, now that one may be
            // free. The original closure is reused, so no step is skipped.
            if !pending_work.is_empty() {
                let queued = std::mem::take(&mut pending_work);
                for (work, task) in queued {
                    if let Some(pair) = self.start_on_helper(work, task, &mut in_flight) {
                        pending_work.push(pair);
                    }
                }
            }

            // Return any deferred task whose wait has elapsed. This runs every
            // pass rather than only when nothing is in flight: a task deferred
            // because helpers were saturated would otherwise sit until the
            // running calls drained, and be lost entirely if the loop ended
            // first.
            if !deferred.is_empty() {
                let now = self.clock.now();
                let mut queue = self.state.queue.lock();
                let mut still_deferred = Vec::with_capacity(deferred.len());
                for (at, pending) in deferred.drain(..) {
                    if at <= now {
                        queue.ready.push_back(QueuedTask { task: pending, work: None });
                    } else {
                        // When: at remains above now; requeueing before that
                        // instant would turn deferred polling into queue spin.
                        still_deferred.push((at, pending));
                    }
                }
                drop(queue);
                deferred = still_deferred;
            }

            let queued = self.state.queue.lock().ready.pop_front();
            let Some(QueuedTask { mut task, work }) = queued else {
                // When: task is absent while other collections may still own
                // work; drive those owners to a reported disposition before exit.

                // When: in_flight or pending_work is not is_empty; drive blocking
                // work here and leave deferred-only work to the path below.
                if !in_flight.is_empty() || !pending_work.is_empty() {
                    if cancel.is_cancelled()
                        || self.clock.now() >= self.effective_deadline(deadline)
                    {
                        // When: cancel is_cancelled or clock now reaches deadline;
                        // outstanding calls take a terminal disposition here.

                        // Out of time. A call still running is abandoned rather
                        // than joined: joining here is what made the deadline
                        // advisory, and a wedged native call would hold the
                        // loop for as long as it hangs. The thread keeps its
                        // helper slot and is reported through `live_helpers`,
                        // so an unreturned call stays visible instead of
                        // silently blocking teardown.
                        for (handle, mut task) in in_flight.drain(..) {
                            if handle.is_finished() {
                                let result = handle.join().unwrap_or(ReapResult::Failed);
                                task.on_completion(result);
                                self.settle(task, result, &mut progress);
                            } else {
                                // When: handle is not is_finished at cutoff; retain
                                // the task permit while its thread keeps the helper slot.
                                task.keep_abandoned_helper(handle);
                                task.on_completion(ReapResult::TimedOut);
                                self.settle(task, ReapResult::TimedOut, &mut progress);
                            }
                        }
                        for (work, mut task) in pending_work.drain(..) {
                            if self.may_requeue_unstarted(&mut *task, cancel) {
                                // When: may_requeue_unstarted accepts task, preserve its original call outside this expired run.
                                self.state
                                    .queue
                                    .lock()
                                    .carry_over
                                    .push_back(QueuedTask { task, work: Some(work) });
                                continue;
                            }
                            let outcome = task.force_cancel();
                            let result = if outcome.is_settled() {
                                ReapResult::Settled
                            } else {
                                ReapResult::TimedOut
                            };
                            task.on_completion(result);
                            self.settle(task, result, &mut progress);
                        }
                        continue;
                    }
                    // Nothing else is ready. Wait for a helper to finish, but
                    // only up to the deadline: polling completion keeps the
                    // deadline real, where joining would surrender it to
                    // whatever the call decides to do.
                    if in_flight.iter().any(|(handle, _)| handle.is_finished()) {
                        // When: in_flight iter has an is_finished handle; join it
                        // before sleeping so completed charges settle promptly.
                        let mut ready = Vec::with_capacity(in_flight.len());
                        let mut running = Vec::with_capacity(in_flight.len());
                        for entry in in_flight.drain(..) {
                            if entry.0.is_finished() {
                                ready.push(entry);
                            } else {
                                // When: entry is not finished; keep it in flight
                                // because dropping it detaches work and releases charges.
                                running.push(entry);
                            }
                        }
                        in_flight = running;
                        for (handle, mut task) in ready {
                            let result = handle.join().unwrap_or(ReapResult::Failed);
                            task.on_completion(result);
                            if result.releases_charge() {
                                self.settle(task, result, &mut progress);
                            } else {
                                // When: result does not releases_charge after join;
                                // retry after backoff instead of reporting settlement.
                                deferred.push((self.retry_at(), task));
                            }
                        }
                        continue;
                    }
                    self.clock.wait_until(self.effective_deadline(deadline).min(
                        self.clock.now().checked_add(HELPER_POLL_INTERVAL).unwrap_or(deadline),
                    ));
                    continue;
                }
                let Some(next_poll) = deferred.iter().map(|(at, _)| *at).min() else {
                    // When: no queued, running, pending, or deferred task remains,
                    // so this invocation has nothing left it can drive.
                    break;
                };
                if cancel.is_cancelled() || next_poll > self.effective_deadline(deadline) {
                    // When: cancellation arrived or the earliest wakeup exceeds
                    // the deadline, so force deferred work instead of waiting.
                    for (_, mut pending) in deferred.drain(..) {
                        if self.may_requeue_unstarted(&mut *pending, cancel) {
                            // When: may_requeue_unstarted accepts pending, retain its permit outside the expired run.
                            self.state
                                .queue
                                .lock()
                                .carry_over
                                .push_back(QueuedTask { task: pending, work: None });
                            continue;
                        }
                        let outcome = pending.force_cancel();
                        let result = if outcome.is_settled() {
                            ReapResult::Settled
                        } else {
                            ReapResult::TimedOut
                        };
                        pending.on_completion(result);
                        self.settle(pending, result, &mut progress);
                    }
                    break;
                }
                // Keep deferred work dormant, but revisit the shared shutdown deadline between bounded clock waits.
                self.clock.wait_until(
                    next_poll.min(self.effective_deadline(deadline)).min(
                        self.clock.now().checked_add(HELPER_POLL_INTERVAL).unwrap_or(deadline),
                    ),
                );
                let now = self.clock.now();
                let mut queue = self.state.queue.lock();
                let mut still_deferred = Vec::with_capacity(deferred.len());
                for (at, pending) in deferred.drain(..) {
                    if at <= now {
                        queue.ready.push_back(QueuedTask { task: pending, work: None });
                    } else {
                        // When: at remains above now; requeueing before that
                        // instant would turn deferred polling into queue spin.
                        still_deferred.push((at, pending));
                    }
                }
                drop(queue);
                deferred = still_deferred;
                continue;
            };
            let now = self.clock.now();
            if now >= self.effective_deadline(deadline) || cancel.is_cancelled() {
                // When: now reaches deadline or cancel is_cancelled after pop, preserve only eligible unstarted normal-run work.
                if self.may_requeue_unstarted(&mut *task, cancel) {
                    // When: may_requeue_unstarted accepts task, carry its owned call beyond the expired run.
                    self.state.queue.lock().carry_over.push_back(QueuedTask { task, work });
                    continue;
                }
                let outcome = task.force_cancel();
                let result =
                    if outcome.is_settled() { ReapResult::Settled } else { ReapResult::TimedOut };
                task.on_completion(result);
                self.settle(task, result, &mut progress);
                continue;
            }
            let action = match work {
                Some(work) => ReapAction::RunBlocking(work),
                None => task.next_action(now),
            };
            match action {
                ReapAction::Complete(result) => {
                    task.on_completion(result);
                    self.settle(task, result, &mut progress);
                }
                ReapAction::RunBlocking(work) => {
                    // Hand the call to a helper and move on. The result is
                    // collected below once the helper finishes, so a hung
                    // native call cannot hold up unrelated teardowns.
                    if let Some(pair) = self.start_on_helper(work, task, &mut in_flight) {
                        // No helper free. Hold the original call and retry it
                        // when one frees, rather than asking the task for a
                        // closure it has already moved past producing.
                        pending_work.push(pair);
                    }
                }
                ReapAction::PollAfter(at) => {
                    // When: PollAfter at defers progress, preserve only work that can still run within the active deadline.
                    if at > self.effective_deadline(deadline) {
                        // When: at exceeds effective_deadline, carry an eligible unstarted task or retain forced cleanup.
                        if self.may_requeue_unstarted(&mut *task, cancel) {
                            // When: may_requeue_unstarted accepts task, preserve its permit for a later normal run.
                            self.state
                                .queue
                                .lock()
                                .carry_over
                                .push_back(QueuedTask { task, work: None });
                            continue;
                        }
                        let outcome = task.force_cancel();
                        let result = if outcome.is_settled() {
                            ReapResult::Settled
                        } else {
                            ReapResult::TimedOut
                        };
                        task.on_completion(result);
                        self.settle(task, result, &mut progress);
                    } else {
                        // When: at is within deadline; defer this PollAfter task
                        // rather than force-cancelling work that can still run in time.
                        progress.polls += 1;
                        deferred.push((at, task));
                    }
                }
            }
        }
        progress
    }

    /// Test the same complete helper claim used by both normal starts and carry-over readiness.
    fn helper_claim_fits(&self, claim: HelperClaim, counters: &Counters) -> bool {
        let demand = match claim {
            HelperClaim::PerCall => 1,
            HelperClaim::Grant(count) => count,
            HelperClaim::Held => 0,
        };
        demand <= self.state.limits.max_helpers.saturating_sub(counters.helpers)
    }

    /// Return whether a normal driver run can make progress without spinning on an inadmissible carried unit.
    // Lock order: queue releases before counters; only the driver claims grants between this check and its first pop.
    pub fn has_startable_work(&self) -> bool {
        let (ready, claim) = {
            let queue = self.state.queue.lock();
            (
                !queue.ready.is_empty(),
                queue.carry_over.front().map(|entry| entry.task.helper_claim()),
            )
        };
        let counters = self.state.counters.lock();
        counters.admitting
            && (ready || claim.is_some_and(|claim| self.helper_claim_fits(claim, &counters)))
    }

    /// Ask task code only with supervisor locks released; shutdown and cancellation forbid normal-run carry-over.
    fn may_requeue_unstarted(&self, task: &mut dyn ReapTask, cancel: &CancelToken) -> bool {
        let admitting = self.state.counters.lock().admitting;
        !cancel.is_cancelled() && admitting && task.requeue_unstarted()
    }

    /// Start blocking work on a helper without waiting for it.
    ///
    /// Returns the work and its task if no helper was free, so the caller can
    /// retry the *same* call later. Asking the task for a fresh closure instead
    /// would lose the work: `next_action` has already advanced the task's
    /// state by the time it hands the closure over, so a second call reports
    /// the step as done when it never ran.
    ///
    /// Joining here rather than deferring would hold the helper slot for the
    /// call's whole duration, which makes more than one helper unreachable and
    /// lets a single hung native call stall every other owner's teardown.
    #[allow(clippy::type_complexity)]
    fn start_on_helper(
        &self,
        work: Box<dyn FnOnce() -> ReapResult + Send>,
        mut task: Box<dyn ReapTask>,
        in_flight: &mut Vec<(std::thread::JoinHandle<ReapResult>, Box<dyn ReapTask>)>,
    ) -> Option<(Box<dyn FnOnce() -> ReapResult + Send>, Box<dyn ReapTask>)> {
        let claim = task.helper_claim();
        let demand = match claim {
            HelperClaim::PerCall => 1,
            HelperClaim::Grant(count) => count,
            HelperClaim::Held => 0,
        };
        // Admission claims a complete grant under counters, then invokes task code only after unlocking.
        {
            let mut counters = self.state.counters.lock();
            if !self.helper_claim_fits(claim, &counters) {
                // When: helper_claim_fits rejects claim, return untouched work without starting a partial unit.
                return Some((work, task));
            }
            counters.helpers += demand;
        }
        let grant = match claim {
            HelperClaim::PerCall => None,
            HelperClaim::Grant(count) => {
                let grant = HelperGrant {
                    inner: Arc::new(HelperGrantState {
                        state: self.state.clone(),
                        occupied: (0..count)
                            .map(|_| std::sync::atomic::AtomicBool::new(false))
                            .collect(),
                    }),
                };
                task.accept_helper_grant(grant.clone());
                Some(grant)
            }
            HelperClaim::Held => task.held_helper_grant(),
        };
        if let Some(grant) = grant {
            // When: grant supplies the outer slot, a retry reuses its held permits even after a failed spawn.
            return match grant.spawn(0, "sonic-reaper-helper", work) {
                Ok(handle) => {
                    in_flight.push((handle, task));
                    None
                }
                Err(_) => Some((Box::new(|| ReapResult::Failed), task)),
            };
        }
        if claim == HelperClaim::Held {
            // When: Held has no retained grant, do not start an uncounted worker for an invalid task implementation.
            return Some((work, task));
        }
        // The helper releases its own slot when the call returns, rather than
        // the loop releasing it on join. A call abandoned at the deadline is
        // never joined, so a loop-side release would keep its slot forever and
        // the pool would shrink by one on every abandonment until no blocking
        // work could start again — trading a hang for a silent, permanent
        // stall. Releasing here means the slot is held exactly as long as the
        // call actually runs.
        //
        // The release is a drop guard rather than a statement after the call,
        // because a panicking call unwinds straight past a statement. Cleanup
        // work panicking is far more reachable than a native call wedging, and
        // it would leak the slot just as permanently.
        let state = self.state.clone();
        let scoped: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
            let _slot = HelperSlot { state };
            work()
        });
        // A thread the OS refuses is a resource failure, not a panic: this
        // crate exists to stay standing under exhaustion.
        let spawn = || {
            #[cfg(test)]
            if FAIL_HELPER_SPAWN.get() {
                // When: FAIL_HELPER_SPAWN is set on this test thread, discard scoped
                // like an OS spawn refusal and exercise the same error branch.
                drop(scoped);
                return Err(std::io::Error::other("injected helper spawn failure"));
            }
            std::thread::Builder::new().name("sonic-reaper-helper".to_owned()).spawn(scoped)
        };
        match spawn() {
            Ok(handle) => {
                in_flight.push((handle, task));
                None
            }
            Err(_) => {
                self.state.counters.lock().helpers -= 1;
                // The failed spawn drops the closure, not the task permit. Return
                // a synthetic Failed call with the task so retries cannot silently
                // skip its step before cancellation or settlement.
                Some((Box::new(|| ReapResult::Failed), task))
            }
        }
    }

    /// When a call that did not settle may be attempted again.
    fn retry_at(&self) -> Instant {
        self.clock.now().checked_add(FAILED_CALL_BACKOFF).unwrap_or_else(|| self.clock.now())
    }

    // Lock order: counters -> retained for unresolved tasks; release_retained
    // releases retained before its permit guard takes counters.
    fn settle(&self, task: Box<dyn ReapTask>, result: ReapResult, progress: &mut ReaperProgress) {
        let owner = task.owner();
        let mut counters = self.state.counters.lock();
        if result.releases_charge() {
            struct NotifySlotRelease<'a> {
                slot_released: &'a Condvar,
            }

            // Lifecycle: NotifySlotRelease calls slot_released notify_one even when task destruction unwinds.
            impl Drop for NotifySlotRelease<'_> {
                fn drop(&mut self) {
                    self.slot_released.notify_one();
                }
            }

            let _notify = NotifySlotRelease { slot_released: &self.state.slot_released };
            counters.tasks -= 1;
            progress.settled += 1;
            // Native permit destructors may re-enter counters; release the guard before task destruction, but notify afterward.
            drop(counters);
            drop(task);
        } else {
            // When: the result did not release the charge, so name this owner
            // unresolved before moving the task into retained storage.
            progress.unresolved += 1;
            // One entry per owner, not per task. The field names which owners
            // are unresolved, so a second unsettled task from an owner already
            // named adds nothing to the report and would let the vector grow
            // with the task count instead of the owner count — measured at
            // 4,000 entries naming a single distinct owner.
            if !counters.unresolved.contains(&owner) {
                counters.unresolved.push(owner);
            }
            // Keep the task and its permit: retained custody still consumes the
            // task ceiling, and its charge must agree with the unresolved report.
            self.state.retained.lock().push(task);
        }
    }

    /// Stop admitting, wake capacity waiters, drain work, and report the terminal disposition.
    pub fn shutdown(&self, deadline: Instant, cancel: &CancelToken) -> ShutdownReport {
        self.shutdown_handle().close_admission(deadline);
        let progress = self.run_until(deadline, cancel);
        let counters = self.state.counters.lock();
        ShutdownReport {
            settled: progress.settled,
            unresolved_owners: counters.unresolved.clone(),
            live_tasks: counters.tasks,
            live_helpers: counters.helpers,
            live_handles: counters.handles,
        }
    }

    /// Task permits still held by reservations or owned cleanup tasks.
    ///
    /// Includes queued, carried, deferred, pending, in-flight, and retained tasks.
    /// A timeout keeps its permit until completed collection or terminal release ends custody.
    pub fn live_tasks(&self) -> usize {
        self.state.counters.lock().tasks
    }

    /// Held helper permits, including entire retained grants even when none of their worker threads is running.
    pub fn live_helpers(&self) -> usize {
        self.state.counters.lock().helpers
    }

    /// Live handle count.
    pub fn live_handles(&self) -> usize {
        self.state.counters.lock().handles
    }

    /// Tasks retained because they finished without settling.
    ///
    /// Charges remain attributed until opted-in whole-task completion is collected,
    /// [`Self::release_retained`] ends terminal custody, or the supervisor is dropped.
    /// Each retained task also holds its admission permit and
    /// is included in [`Self::live_tasks`]. A non-zero count in a healthy process
    /// means something never gave its resources back.
    pub fn retained_tasks(&self) -> usize {
        self.state.retained.lock().len()
    }

    /// Collect opted-in whole-task completions after all retained worker handles have finished.
    ///
    /// Predicate checks happen under retained custody; joins and task destruction happen after that lock is released.
    // Lock order: counters admission read ends before retained; retained releases before later counters updates and task destruction.
    pub fn collect_settled_retained(&self) -> usize {
        if !self.is_admitting() {
            // When: is_admitting is false, leave late completed custody and its diagnosis for terminal release_retained.
            return 0;
        }
        // Declare the permit guard first so removed task destructors run before counts return even during unwind.
        let mut permits = ReleaseTaskPermits { state: &self.state, count: 0 };
        let mut completed = Vec::new();
        {
            let mut retained = self.state.retained.lock();
            let mut index = 0;
            while index < retained.len() {
                if retained[index].is_collectable() {
                    completed.push(retained.swap_remove(index));
                    permits.count += 1;
                } else {
                    // When: is_collectable is false, incomplete native custody or an unfinished thread must remain retained.
                    index += 1;
                }
            }
        }
        let count = completed.len();
        for task in &mut completed {
            task.join_finished_helpers();
            let owner = task.owner();
            self.state.counters.lock().unresolved.retain(|unresolved| *unresolved != owner);
        }
        drop(completed);
        count
    }

    /// Surrender and destroy retained tasks, returning their admission permits.
    ///
    /// Returns the number of tasks released and wakes waiting reservers.
    ///
    /// Retention keeps an unsettled charge visible, but it also keeps the
    /// owner from closing, and a closed owner's parent from closing after it.
    /// Without this, one wedged transport pins its whole window subtree until
    /// the process exits. This is the terminal cleanup that unwinds it, so a
    /// caller can reclaim a stuck subtree instead of restarting.
    ///
    /// Permits return only after task destruction, including when surrender or a
    /// destructor unwinds. An ordinary `PerCall` helper keeps its one slot, while
    /// a grant worker's clone keeps the entire grant reserved. This method does
    /// not wait for either, and a later worker return does not settle the task.
    /// Owners already reported unresolved stay reported: surrender does not retract
    /// the diagnosis.
    // Release the retained guard before ReleaseTaskPermits locks counters;
    // settlement holds counters before retained, so these locks must not overlap here.
    pub fn release_retained(&self) -> usize {
        // Declared before retained so its drop runs after every task on unwind.
        let mut permits = ReleaseTaskPermits { state: &self.state, count: 0 };
        let mut retained = std::mem::take(&mut *self.state.retained.lock());
        let count = retained.len();
        permits.count = count;
        for task in &mut retained {
            task.surrender_charges();
        }
        drop(retained);
        count
    }

    /// Return whether the supervisor still admits work.
    pub fn is_admitting(&self) -> bool {
        self.state.counters.lock().admitting
    }
}

/// Counts from one supervisor run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ReaperProgress {
    /// Tasks that released their charge.
    pub settled: usize,
    /// Tasks that finished without releasing their charge.
    pub unresolved: usize,
    /// Deferred polls serviced.
    pub polls: usize,
}

/// Terminal disposition of a supervisor.
///
/// A clean shutdown owns nothing: any unresolved owner is reported rather than
/// dropped, because a forgotten charge is the failure this contract exists to
/// make visible.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ShutdownReport {
    /// Tasks that released their charge.
    pub settled: usize,
    /// Owners still holding a charge at shutdown.
    pub unresolved_owners: Vec<ResourceOwnerId>,
    /// Task permits still held, including reservations and retained unsettled tasks.
    pub live_tasks: usize,
    /// Held helper permits; retained whole grants count even with no running worker threads.
    pub live_helpers: usize,
    /// Native handles still held.
    pub live_handles: usize,
}

impl ShutdownReport {
    /// Return whether the supervisor exited owning nothing.
    pub fn is_clean(&self) -> bool {
        self.unresolved_owners.is_empty()
            && self.live_tasks == 0
            && self.live_helpers == 0
            && self.live_handles == 0
    }
}

/// Duration helper for callers building deadlines from a [`Clock`].
pub fn deadline_from(clock: &dyn Clock, budget: Duration) -> Instant {
    clock.now().checked_add(budget).expect("deadline overflow")
}

#[cfg(test)]
#[path = "reaper_tests.rs"]
mod reaper_tests;
