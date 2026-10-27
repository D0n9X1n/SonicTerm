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
    /// Task custody ceiling, including reservations, retained tasks, and sink entries held until process exit.
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
    ///
    /// Opt-in requires a unique owner per collectable unit, such as one retired `PtyTransport`;
    /// collection retracts that owner from unresolved reporting. Shared-owner tasks keep the default `false`.
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
    sink: UnresolvedSink,
    limits: ReaperLimits,
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
                sink: UnresolvedSink::default(),
                limits,
            }),
            clock,
        }
    }

    /// Clone the process-exit sink without sharing a supervisor lock with native payload destruction.
    pub fn unresolved_sink(&self) -> UnresolvedSink {
        self.state.sink.clone()
    }

    fn held_tasks(&self, counters: &Counters) -> usize {
        counters.tasks.saturating_add(self.state.sink.entries())
    }

    /// Return a control handle that closes admission without driving queued tasks.
    pub fn shutdown_handle(&self) -> ReapShutdownHandle {
        ReapShutdownHandle { state: self.state.clone() }
    }

    /// Try to reserve a slot before starting cancellable work.
    ///
    /// A refusal leaves the caller owning whatever it holds. `QueueFull` may last
    /// until process exit because unresolved sink entries retain their capacity.
    /// Retry only within a bound, then use synchronous fallback while preserving
    /// unresolved ownership; returning an error may not abandon the resource.
    pub fn try_reserve_slot(&self) -> Result<ReapSlot, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            // When: counters admitting is false after shutdown; refusal preserves
            // caller ownership instead of creating unreported work.
            return Err(ReapAdmission::ShuttingDown);
        }
        if self.held_tasks(&counters) >= self.state.limits.max_tasks {
            // When: held_tasks reaches max_tasks, task and sink custody leave no capacity for another reservation.
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
        if self.held_tasks(&counters) >= self.state.limits.max_tasks
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
            if self.held_tasks(&counters) < self.state.limits.max_tasks {
                // When: held_tasks is below max_tasks, claim under counters so another waiter cannot take the capacity.
                counters.tasks += 1;
                return Ok(ReapSlot { state: self.state.clone(), consumed: false });
            }
            #[cfg(test)]
            reaper_tests::slot_wait_started(&mut counters);
            if self.state.slot_released.wait_until(&mut counters, deadline).timed_out()
                && counters.admitting
                && self.held_tasks(&counters) >= self.state.limits.max_tasks
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
    // The counters snapshot ends before reading sink payloads; no sink lock overlaps counters or retained custody.
    pub fn shutdown(&self, deadline: Instant, cancel: &CancelToken) -> ShutdownReport {
        self.shutdown_handle().close_admission(deadline);
        let progress = self.run_until(deadline, cancel);
        let mut report = {
            let counters = self.state.counters.lock();
            ShutdownReport {
                settled: progress.settled,
                unresolved_owners: counters.unresolved.clone(),
                live_tasks: self.held_tasks(&counters),
                live_helpers: counters.helpers,
                live_handles: counters.handles,
                unresolved_entries: self.state.sink.entries(),
                open_fallback_duplicates: self.state.sink.open_fallback_duplicates(),
            }
        };
        for owner in self.state.sink.owners() {
            if !report.unresolved_owners.contains(&owner) {
                report.unresolved_owners.push(owner);
            }
        }
        report
    }

    /// Task custody held by reservations, cleanup tasks, and unresolved sink payloads.
    ///
    /// Includes queued, carried, deferred, pending, in-flight, and retained tasks.
    /// Terminal release can replace a task permit with a sink entry without admitting beyond unresolved custody.
    pub fn live_tasks(&self) -> usize {
        self.held_tasks(&self.state.counters.lock())
    }

    /// Held helper permits, including entire retained grants even when none of their worker threads is running.
    pub fn live_helpers(&self) -> usize {
        self.state.counters.lock().helpers
    }

    /// Live handle count.
    pub fn live_handles(&self) -> usize {
        self.state.counters.lock().handles
    }

    /// Return whether the supervisor still admits work.
    pub fn is_admitting(&self) -> bool {
        self.state.counters.lock().admitting
    }
}

// Lifecycle: ReaperSupervisor calls abandon_payloads on sink without native destruction, preserving accounting until process exit.
impl Drop for ReaperSupervisor {
    fn drop(&mut self) {
        self.state.sink.abandon_payloads();
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
    /// Task custody still held, including reservations, retained tasks, and unresolved sink entries.
    pub live_tasks: usize,
    /// Held helper permits; retained whole grants count even with no running worker threads.
    pub live_helpers: usize,
    /// Native handles still held.
    pub live_handles: usize,
    /// Whole unresolved native payloads retained in the process-exit sink.
    pub unresolved_entries: usize,
    /// Slotless cancellation duplicates still open on fallback workers.
    pub open_fallback_duplicates: usize,
}

impl ShutdownReport {
    /// Return whether the supervisor exited owning nothing.
    pub fn is_clean(&self) -> bool {
        self.unresolved_owners.is_empty()
            && self.live_tasks == 0
            && self.live_helpers == 0
            && self.live_handles == 0
            && self.unresolved_entries == 0
            && self.open_fallback_duplicates == 0
    }
}

/// Duration helper for callers building deadlines from a [`Clock`].
pub fn deadline_from(clock: &dyn Clock, budget: Duration) -> Instant {
    clock.now().checked_add(budget).expect("deadline overflow")
}

#[cfg(test)]
#[path = "reaper_tests.rs"]
mod reaper_tests;

#[path = "reaper_run.rs"]
mod run;

#[path = "reaper_helpers.rs"]
mod helpers;

#[path = "reaper_retained.rs"]
mod retained;

#[path = "reaper_unresolved.rs"]
mod unresolved;

pub use unresolved::{FallbackDuplicateToken, UnresolvedSink};

#[cfg(test)]
#[path = "reaper_test_support.rs"]
mod test_support;
