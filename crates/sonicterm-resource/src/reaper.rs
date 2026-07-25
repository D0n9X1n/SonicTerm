//! Bounded reaper supervisor for resources that outlive their owner's close.

use crate::{cancel::CancelToken, clock::Clock};
use parking_lot::{Condvar, Mutex};
use sonicterm_types::{CancelOutcome, ReapAdmission, ReapResult, ResourceOwnerId};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

/// Fixed ceilings for one supervisor.
///
/// Limits are immutable after construction so admission cannot be widened while
/// work is in flight.
#[derive(Clone, Copy, Debug)]
pub struct ReaperLimits {
    /// Concurrently owned tasks.
    pub max_tasks: usize,
    /// Concurrent blocking helpers.
    pub max_helpers: usize,
    /// Native handles the supervisor may own at once.
    pub max_handles: usize,
}

impl ReaperLimits {
    /// Create limits, rejecting a zero ceiling.
    ///
    /// A zero ceiling would admit nothing while still accepting reservations,
    /// so it is refused at construction rather than deadlocking a caller later.
    pub fn new(max_tasks: usize, max_helpers: usize, max_handles: usize) -> Option<Self> {
        if max_tasks == 0 || max_helpers == 0 || max_handles == 0 {
            return None;
        }
        Some(Self { max_tasks, max_helpers, max_handles })
    }
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
}

struct Counters {
    tasks: usize,
    helpers: usize,
    handles: usize,
    admitting: bool,
    unresolved: Vec<ResourceOwnerId>,
}

struct SupervisorState {
    counters: Mutex<Counters>,
    slot_released: Condvar,
    queue: Mutex<VecDeque<Box<dyn ReapTask>>>,
    /// Tasks that finished without settling.
    ///
    /// Their resources stay charged to the original owner until terminal
    /// cleanup, so the supervisor keeps the task alive rather than dropping it.
    /// Dropping would run the task's RAII release and quietly zero an owner the
    /// shutdown report is simultaneously naming as unresolved.
    retained: Mutex<Vec<Box<dyn ReapTask>>>,
    limits: ReaperLimits,
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
        self.state.queue.lock().push_back(task);
        self.consumed = true;
    }
}

impl core::fmt::Debug for ReapSlot {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("ReapSlot").field("consumed", &self.consumed).finish()
    }
}

impl Drop for ReapSlot {
    fn drop(&mut self) {
        if !self.consumed {
            let mut counters = self.state.counters.lock();
            counters.tasks -= 1;
            self.state.slot_released.notify_one();
        }
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
                    unresolved: Vec::new(),
                }),
                slot_released: Condvar::new(),
                queue: Mutex::new(VecDeque::new()),
                retained: Mutex::new(Vec::new()),
                limits,
            }),
            clock,
        }
    }

    /// Try to reserve a slot before starting cancellable work.
    ///
    /// A refusal leaves the caller owning whatever it holds: it must complete
    /// synchronously or retry. Returning an error while abandoning the resource
    /// is not an option the API offers.
    pub fn try_reserve_slot(&self) -> Result<ReapSlot, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            return Err(ReapAdmission::ShuttingDown);
        }
        if counters.tasks >= self.state.limits.max_tasks {
            return Err(ReapAdmission::QueueFull);
        }
        counters.tasks += 1;
        Ok(ReapSlot { state: self.state.clone(), consumed: false })
    }

    /// Wait for a slot until the deadline.
    pub fn reserve_slot_until(&self, deadline: Instant) -> Result<ReapSlot, ReapAdmission> {
        let mut counters = self.state.counters.lock();
        loop {
            if !counters.admitting {
                return Err(ReapAdmission::ShuttingDown);
            }
            if counters.tasks < self.state.limits.max_tasks {
                counters.tasks += 1;
                return Ok(ReapSlot { state: self.state.clone(), consumed: false });
            }
            if self.state.slot_released.wait_until(&mut counters, deadline).timed_out()
                && counters.tasks >= self.state.limits.max_tasks
            {
                return Err(ReapAdmission::QueueFull);
            }
        }
    }

    /// Reserve a native handle against the supervisor ceiling.
    pub fn try_reserve_handle(&self) -> Result<(), ReapAdmission> {
        let mut counters = self.state.counters.lock();
        if !counters.admitting {
            return Err(ReapAdmission::ShuttingDown);
        }
        if counters.handles >= self.state.limits.max_handles {
            return Err(ReapAdmission::QueueFull);
        }
        counters.handles += 1;
        Ok(())
    }

    /// Release a previously reserved native handle.
    pub fn release_handle(&self) {
        let mut counters = self.state.counters.lock();
        counters.handles = counters.handles.saturating_sub(1);
    }

    /// Drive queued tasks until each reaches a terminal disposition or the
    /// deadline elapses.
    ///
    /// Blocking work never runs inline: `RunBlocking` is dispatched to a bounded
    /// helper and the loop keeps servicing other tasks while it runs.
    pub fn run_until(&self, deadline: Instant, cancel: &CancelToken) -> ReaperProgress {
        let mut progress = ReaperProgress::default();
        // Tasks that asked to be polled later wait here rather than cycling
        // through the queue, so a deferred task cannot spin the loop.
        let mut deferred: Vec<(Instant, Box<dyn ReapTask>)> = Vec::new();
        loop {
            let task = self.state.queue.lock().pop_front();
            let Some(mut task) = task else {
                let Some(next_poll) = deferred.iter().map(|(at, _)| *at).min() else { break };
                if cancel.is_cancelled() || next_poll > deadline {
                    // Nothing will become ready in time: settle what is left.
                    for (_, mut pending) in deferred.drain(..) {
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
                // Sleep to the earliest deadline instead of re-polling work
                // that has already said it is not ready.
                self.clock.wait_until(next_poll);
                let now = self.clock.now();
                let mut queue = self.state.queue.lock();
                let mut still_deferred = Vec::with_capacity(deferred.len());
                for (at, pending) in deferred.drain(..) {
                    if at <= now {
                        queue.push_back(pending);
                    } else {
                        still_deferred.push((at, pending));
                    }
                }
                drop(queue);
                deferred = still_deferred;
                continue;
            };
            let now = self.clock.now();
            if now >= deadline || cancel.is_cancelled() {
                // Out of time: force cancellation, and keep the charge if the
                // resource did not actually settle.
                let outcome = task.force_cancel();
                let result =
                    if outcome.is_settled() { ReapResult::Settled } else { ReapResult::TimedOut };
                task.on_completion(result);
                self.settle(task, result, &mut progress);
                continue;
            }
            match task.next_action(now) {
                ReapAction::Complete(result) => {
                    task.on_completion(result);
                    self.settle(task, result, &mut progress);
                }
                ReapAction::RunBlocking(work) => {
                    let result = self.run_on_helper(work);
                    task.on_completion(result);
                    if result == ReapResult::Settled || result == ReapResult::Escalated {
                        self.settle(task, result, &mut progress);
                    } else {
                        self.state.queue.lock().push_back(task);
                    }
                }
                ReapAction::PollAfter(at) => {
                    if at > deadline {
                        let outcome = task.force_cancel();
                        let result = if outcome.is_settled() {
                            ReapResult::Settled
                        } else {
                            ReapResult::TimedOut
                        };
                        task.on_completion(result);
                        self.settle(task, result, &mut progress);
                    } else {
                        progress.polls += 1;
                        deferred.push((at, task));
                    }
                }
            }
        }
        progress
    }

    fn run_on_helper(&self, work: Box<dyn FnOnce() -> ReapResult + Send>) -> ReapResult {
        {
            let mut counters = self.state.counters.lock();
            if counters.helpers >= self.state.limits.max_helpers {
                return ReapResult::TimedOut;
            }
            counters.helpers += 1;
        }
        let handle = std::thread::spawn(work);
        let result = handle.join().unwrap_or(ReapResult::Failed);
        self.state.counters.lock().helpers -= 1;
        result
    }

    fn settle(&self, task: Box<dyn ReapTask>, result: ReapResult, progress: &mut ReaperProgress) {
        let owner = task.owner();
        let mut counters = self.state.counters.lock();
        counters.tasks -= 1;
        if result.releases_charge() {
            progress.settled += 1;
            drop(task);
        } else {
            progress.unresolved += 1;
            counters.unresolved.push(owner);
            // Keep the task alive so whatever it holds stays charged to this
            // owner. Dropping it here would release the charge and leave the
            // ledger disagreeing with the report that just named the owner.
            self.state.retained.lock().push(task);
        }
        self.state.slot_released.notify_one();
    }

    /// Stop admitting, cancel everything, and report the terminal disposition.
    pub fn shutdown(&self, deadline: Instant, cancel: &CancelToken) -> ShutdownReport {
        self.state.counters.lock().admitting = false;
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

    /// Live task count, for tests and diagnostics.
    pub fn live_tasks(&self) -> usize {
        self.state.counters.lock().tasks
    }

    /// Live helper count.
    pub fn live_helpers(&self) -> usize {
        self.state.counters.lock().helpers
    }

    /// Live handle count.
    pub fn live_handles(&self) -> usize {
        self.state.counters.lock().handles
    }

    /// Tasks retained because they finished without settling.
    ///
    /// Their charges stay attributed to the original owner and are released
    /// only when the supervisor itself is dropped, which is the terminal
    /// cleanup the contract defers them to. A non-zero count in a healthy
    /// process means something never gave its resources back.
    pub fn retained_tasks(&self) -> usize {
        self.state.retained.lock().len()
    }

    /// Return whether the supervisor still admits work.
    pub fn is_admitting(&self) -> bool {
        self.state.counters.lock().admitting
    }
}

/// Counts from one supervisor run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
pub struct ShutdownReport {
    /// Tasks that released their charge.
    pub settled: usize,
    /// Owners still holding a charge at shutdown.
    pub unresolved_owners: Vec<ResourceOwnerId>,
    /// Tasks still owned.
    pub live_tasks: usize,
    /// Helpers still running.
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
