use super::{
    CancelToken, Counters, HelperClaim, QueuedTask, ReapAction, ReapResult, ReapTask,
    ReaperProgress, ReaperSupervisor, FAILED_CALL_BACKOFF, HELPER_POLL_INTERVAL,
};
use std::time::Instant;

impl ReaperSupervisor {
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
    pub(super) fn helper_claim_fits(&self, claim: HelperClaim, counters: &Counters) -> bool {
        let demand = match claim {
            HelperClaim::PerCall => 1,
            HelperClaim::Grant(count) => count,
            HelperClaim::Held => 0,
        };
        demand <= self.state.limits.max_helpers.saturating_sub(counters.helpers)
    }

    /// Return normal-run admission readiness after the caller collects settled retained tasks.
    ///
    /// This query does not discover or release collectable grants. The driver must collect, check control,
    /// then query after every wake, recheck and run return, including while carry-over is blocked.
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

    /// When a call that did not settle may be attempted again.
    fn retry_at(&self) -> Instant {
        self.clock.now().checked_add(FAILED_CALL_BACKOFF).unwrap_or_else(|| self.clock.now())
    }

    /// Bound this run by the earliest drain deadline published by an exit thread.
    fn effective_deadline(&self, run_deadline: Instant) -> Instant {
        self.state
            .counters
            .lock()
            .drain_by
            .map_or(run_deadline, |drain_by| run_deadline.min(drain_by))
    }
}

#[cfg(test)]
#[path = "reaper_run_tests.rs"]
mod reaper_run_tests;
