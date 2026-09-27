//! Decide when and how often to rebuild the shared GPU device after the
//! committed device generation is lost.
//!
//! The coordinator is pure: it reads no clock and holds no wgpu object, so every
//! transition is testable with a fixed fake clock. Its caller must pass `now` into
//! each call, run each device request on at most one worker thread, rebind every
//! live renderer on the event-loop thread, and destroy a generation only through
//! the [`RetiredGeneration`](crate::recovery::RetiredGeneration) returned for it. Recovery covers a
//! lost device only; a device that stopped as `Unusable` is reported and left
//! stopped.

use std::time::{Duration, Instant};

use crate::device_errors::{DeviceGate, DeviceState};

/// Proposed delay before each recovery attempt, indexed by the attempts already
/// used since the last stable generation. Its length bounds the attempts.
pub const PROPOSED_BACKOFF: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];

/// Proposed longest time one device request may run before its attempt fails.
pub const PROPOSED_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Proposed time after a generation's first presented frame from which its loss
/// starts a fresh budget instead of continuing the current one.
pub const PROPOSED_STABLE_AFTER: Duration = Duration::from_secs(30);

/// Retry policy for rebuilding a lost device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPolicy {
    /// Delay before each attempt, indexed by the attempts already used.
    pub backoff: &'static [Duration],
    /// Longest one device request may run before its attempt fails.
    pub request_timeout: Duration,
    /// How long after its first presented frame a generation's loss resets the budget.
    pub stable_after: Duration,
}

impl RecoveryPolicy {
    /// The proposed policy: five attempts over 21.25 s, 10 s per request, and a
    /// 30 s stability window.
    #[must_use]
    pub fn proposed() -> Self {
        Self {
            backoff: &PROPOSED_BACKOFF,
            request_timeout: PROPOSED_REQUEST_TIMEOUT,
            stable_after: PROPOSED_STABLE_AFTER,
        }
    }

    /// The most attempts one budget allows.
    #[must_use]
    pub fn max_attempts(&self) -> usize {
        self.backoff.len()
    }
}

/// Proof that a device generation is no longer committed.
///
/// Only the coordinator creates one, and only after the generation stopped
/// being the one renderers must bind to, so destroying the device it names can
/// never start another recovery.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a retired generation's device must be released or destroyed"]
pub struct RetiredGeneration(u64);

impl RetiredGeneration {
    /// The retired device generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0
    }
}

/// Where the coordinator is in a recovery episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPhase {
    /// The committed generation has not recorded a loss.
    Active,
    /// An attempt is waiting for its start time.
    Scheduled {
        /// When the attempt may start.
        due: Instant,
        /// The attempt's 1-based number within the budget.
        attempt: usize,
    },
    /// A worker is requesting a device for the attempt.
    Requesting {
        /// The request the attempt waits for.
        ticket: u64,
        /// The attempt's 1-based number within the budget.
        attempt: usize,
        /// When the attempt fails if the request has not returned.
        deadline: Instant,
    },
    /// The event loop is rebinding every live renderer to the candidate.
    Rebinding {
        /// The attempt's 1-based number within the budget.
        attempt: usize,
        /// The generation of the requested device.
        candidate: u64,
    },
    /// The budget is spent. Shells keep running and rendering stays stopped.
    Exhausted,
}

/// What follows a loss, a failed attempt, or a discarded candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Another attempt is scheduled.
    Retry {
        /// When the attempt may start.
        due: Instant,
        /// The attempt's 1-based number within the budget.
        attempt: usize,
    },
    /// The budget is spent and recovery has ended.
    Exhausted,
}

/// What one stop observation means for recovery.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a stop decision says whether recovery was scheduled"]
pub enum StopDecision {
    /// The committed generation was lost; `Next` says what was scheduled.
    Lost(Next),
    /// The committed generation is `Unusable` without a loss. Nothing is
    /// scheduled; this is reported once per generation.
    DeferredUnusable,
    /// The committed generation still accepts work or has not recorded a loss.
    NotStopped,
    /// The observation names a generation that is not committed.
    Stale,
    /// The committed generation's stop is already handled.
    Duplicate,
    /// Recovery already ended.
    Exhausted,
}

/// What the event loop does now.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a poll decision may start a request or report a failed attempt"]
pub enum PollDecision {
    /// Nothing is pending.
    Idle,
    /// Nothing is due before this instant.
    WaitUntil(Instant),
    /// Start one device request on the worker.
    StartRequest {
        /// Identity the worker's result must carry.
        ticket: u64,
        /// The attempt's 1-based number within the budget.
        attempt: usize,
        /// When the attempt fails if the request has not returned.
        deadline: Instant,
    },
    /// The request passed its deadline. Its worker is left to return; its
    /// result will be discarded.
    TimedOut {
        /// The overdue request.
        ticket: u64,
        /// What follows the failed attempt.
        next: Next,
    },
    /// An attempt came due while an earlier request still runs, so it failed
    /// without starting a second worker.
    WorkerBusy {
        /// The request that still runs.
        overdue: u64,
        /// What follows the failed attempt.
        next: Next,
    },
    /// Every live renderer must be rebound to the candidate and the result
    /// reported through [`RecoveryCoordinator::finish_rebind`].
    AwaitingRebind {
        /// The generation being rebound.
        candidate: u64,
    },
    /// Recovery has ended.
    Exhausted,
}

/// The worker's result for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// A device was created and its error handlers and waker were installed.
    Created {
        /// The new device's generation.
        generation: u64,
    },
    /// No device was created.
    Failed,
}

/// What to do with a finished request.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a request decision may hand back a device to release"]
pub enum RequestDecision {
    /// Rebind every live renderer to the candidate, then call
    /// [`RecoveryCoordinator::finish_rebind`].
    Rebind {
        /// The generation to rebind to.
        candidate: u64,
        /// The attempt's 1-based number within the budget.
        attempt: usize,
    },
    /// The request failed, or named the committed generation instead of a new device.
    Failed(Next),
    /// The result reached its deadline before `poll` saw the timeout. The attempt
    /// failed once, the worker is free, and a new device it created is retired.
    TimedOut {
        /// The device the result created, unless it is in use.
        retired: Option<RetiredGeneration>,
        /// What follows the failed attempt.
        next: Next,
    },
    /// The result arrived after its attempt had failed, or matched nothing
    /// pending. A device it created is retired unless it is the committed
    /// generation or the pending candidate, which stay in use.
    Discard(Option<RetiredGeneration>),
}

/// What the app observed after rebinding renderers to a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReport {
    /// The generation the renderers were rebound to.
    pub candidate: u64,
    /// Live renderers whose device-bound objects now come from `candidate`.
    pub rebound: usize,
    /// Live renderers at the time of the commit.
    pub live: usize,
    /// The candidate's gate, read after every surface was configured.
    pub gate: DeviceGate,
}

/// What a rebind report means.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a commit decision hands back the generation to release"]
pub enum CommitDecision {
    /// Every live renderer uses the new generation; release `retired`.
    Committed {
        /// The newly committed generation.
        generation: u64,
        /// The generation that stopped being committed.
        retired: RetiredGeneration,
    },
    /// The candidate is discarded. Close its gate and destroy it through
    /// `retired`; renderers left on it or on the old generation stay stopped
    /// until the next attempt rebinds all of them.
    Discarded {
        /// The discarded candidate.
        retired: RetiredGeneration,
        /// What follows the failed attempt.
        next: Next,
    },
    /// No rebind of that candidate was pending; nothing changed.
    Unexpected,
}

/// Diagnostic counters since the coordinator was created.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryCounts {
    /// Candidates committed.
    pub rebuilds: u64,
    /// Device requests started.
    pub requests: u64,
    /// Attempts that failed, timed out, found the worker busy, or were discarded.
    pub failed_attempts: u64,
    /// Requests that passed their deadline.
    pub timed_out: u64,
    /// Attempts that came due while an earlier request still ran.
    pub worker_busy: u64,
    /// Candidates discarded after a rebind that missed a renderer or whose gate closed.
    pub discarded: u64,
    /// Results of requests whose attempt had already failed.
    pub late_results: u64,
    /// Results or reports that matched nothing pending.
    pub unexpected: u64,
    /// Stops for generations that were not committed.
    pub stale_stops: u64,
    /// Repeated stops of the committed generation.
    pub duplicate_stops: u64,
}

/// A point-in-time copy of the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoverySnapshot {
    /// The committed generation.
    pub committed: u64,
    /// The current phase.
    pub phase: RecoveryPhase,
    /// Attempts used since the last stable generation.
    pub attempts_used: usize,
    /// Whether a timed-out request's worker has not returned yet.
    pub worker_busy: bool,
    /// Diagnostic counters.
    pub counts: RecoveryCounts,
}

/// Decides every recovery step for the one shared device.
#[derive(Debug)]
pub struct RecoveryCoordinator {
    policy: RecoveryPolicy,
    committed: u64,
    first_present_at: Option<Instant>,
    unusable_reported: bool,
    phase: RecoveryPhase,
    attempts_used: usize,
    next_ticket: u64,
    overdue_ticket: Option<u64>,
    counts: RecoveryCounts,
}

impl RecoveryCoordinator {
    /// Start with `generation` committed and a full budget.
    #[must_use]
    pub fn new(policy: RecoveryPolicy, generation: u64) -> Self {
        Self {
            policy,
            committed: generation,
            first_present_at: None,
            unusable_reported: false,
            phase: RecoveryPhase::Active,
            attempts_used: 0,
            next_ticket: 1,
            overdue_ticket: None,
            counts: RecoveryCounts::default(),
        }
    }

    /// The generation every live renderer must be bound to.
    #[must_use]
    pub fn committed(&self) -> u64 {
        self.committed
    }

    /// The current phase.
    #[must_use]
    pub fn phase(&self) -> RecoveryPhase {
        self.phase
    }

    /// A point-in-time copy for logs and tests.
    #[must_use]
    pub fn snapshot(&self) -> RecoverySnapshot {
        RecoverySnapshot {
            committed: self.committed,
            phase: self.phase,
            attempts_used: self.attempts_used,
            worker_busy: self.overdue_ticket.is_some(),
            counts: self.counts,
        }
    }

    /// Record a presented frame on `generation`.
    ///
    /// The first one on the committed generation starts its stability window: a
    /// loss at least `stable_after` later starts a fresh budget.
    pub fn observe_presented(&mut self, generation: u64, now: Instant) {
        if generation == self.committed && self.first_present_at.is_none() {
            self.first_present_at = Some(now);
        }
    }

    /// Classify a stop observation for `generation`, whose gate read `gate`.
    ///
    /// Only a loss of the committed generation schedules an attempt. Staleness
    /// is decided by generation alone, never by `gate.destroy_requested`: a
    /// destroy of the committed device is a loss to recover, and a destroy of a
    /// retired device names a generation that is no longer committed. `gate`
    /// is read only when `generation` is committed.
    pub fn observe_stop(
        &mut self,
        generation: u64,
        gate: DeviceGate,
        now: Instant,
    ) -> StopDecision {
        if self.phase == RecoveryPhase::Exhausted {
            // When: `phase` is `Exhausted`, the spent budget ends recovery for the process.
            return StopDecision::Exhausted;
        }
        if generation != self.committed {
            // When: `generation` differs from `committed`, it names a retired or discarded device.
            self.counts.stale_stops += 1;
            return StopDecision::Stale;
        }
        let phase = self.phase;
        match (gate.state, phase) {
            (DeviceState::Usable, _) => StopDecision::NotStopped,
            (DeviceState::Unusable, RecoveryPhase::Active) if !self.unusable_reported => {
                self.unusable_reported = true;
                StopDecision::DeferredUnusable
            }
            (DeviceState::Lost, RecoveryPhase::Active) => {
                StopDecision::Lost(self.schedule_after_loss(now))
            }
            (DeviceState::Unusable | DeviceState::Lost, _) => {
                self.counts.duplicate_stops += 1;
                StopDecision::Duplicate
            }
        }
    }

    /// Decide what the event loop does at `now`.
    ///
    /// At most one request runs at a time. An attempt that comes due while a
    /// timed-out request still runs fails without starting a second worker, so a
    /// request that never returns still spends the budget on schedule.
    pub fn poll(&mut self, now: Instant) -> PollDecision {
        let phase = self.phase;
        match phase {
            RecoveryPhase::Active => PollDecision::Idle,
            RecoveryPhase::Exhausted => PollDecision::Exhausted,
            RecoveryPhase::Scheduled { due, .. } if now < due => PollDecision::WaitUntil(due),
            RecoveryPhase::Scheduled { attempt, .. } => self.start_due_attempt(attempt, now),
            RecoveryPhase::Requesting { deadline, .. } if now < deadline => {
                PollDecision::WaitUntil(deadline)
            }
            RecoveryPhase::Requesting { ticket, .. } => self.time_out_request(ticket, now),
            RecoveryPhase::Rebinding { candidate, .. } => {
                PollDecision::AwaitingRebind { candidate }
            }
        }
    }

    /// Classify the worker's result for `ticket`.
    ///
    /// A result at or after its deadline times the attempt out once, even before
    /// `poll` sees it. A result for a request whose attempt already failed frees
    /// the worker and is discarded, as is a result that matches nothing pending. A
    /// device such a result created comes back retired unless it is still in use.
    pub fn request_finished(
        &mut self,
        ticket: u64,
        outcome: RequestOutcome,
        now: Instant,
    ) -> RequestDecision {
        if self.overdue_ticket == Some(ticket) {
            // When: `ticket` equals `overdue_ticket`, its attempt already failed at the deadline.
            self.overdue_ticket = None;
            self.counts.late_results += 1;
            return RequestDecision::Discard(self.retire_discarded(outcome));
        }
        let RecoveryPhase::Requesting { ticket: pending, attempt, deadline } = self.phase else {
            // When: `phase` is not `Requesting`, no attempt waits for this worker result.
            self.counts.unexpected += 1;
            return RequestDecision::Discard(self.retire_discarded(outcome));
        };
        if ticket != pending {
            // When: `ticket` differs from the `pending` request, the result belongs to no attempt.
            self.counts.unexpected += 1;
            return RequestDecision::Discard(self.retire_discarded(outcome));
        }
        if now >= deadline {
            // When: `now` reached `deadline` before `poll` timed the attempt out; count it once
            // and leave the worker free, since it has returned.
            self.counts.failed_attempts += 1;
            self.counts.timed_out += 1;
            let retired = self.retire_discarded(outcome);
            return RequestDecision::TimedOut { retired, next: self.next_attempt(now) };
        }
        match outcome {
            RequestOutcome::Created { generation } if generation != self.committed => {
                self.phase = RecoveryPhase::Rebinding { attempt, candidate: generation };
                RequestDecision::Rebind { candidate: generation, attempt }
            }
            RequestOutcome::Created { .. } => {
                // The committed generation's identity cannot name a new device, so the result is
                // refused before any rebind, and no token is minted for the device in use.
                self.counts.unexpected += 1;
                self.counts.failed_attempts += 1;
                RequestDecision::Failed(self.next_attempt(now))
            }
            RequestOutcome::Failed => {
                self.counts.failed_attempts += 1;
                RequestDecision::Failed(self.next_attempt(now))
            }
        }
    }

    /// Commit or discard the candidate named in `report`.
    ///
    /// The candidate is committed only when every live renderer was rebound to it
    /// and its gate still accepts work after every surface was configured. Any
    /// other report discards it: the app closes its gate and destroys it through
    /// the returned token, and the next attempt rebinds every live renderer,
    /// including any left on the discarded candidate.
    pub fn finish_rebind(&mut self, report: CommitReport, now: Instant) -> CommitDecision {
        let RecoveryPhase::Rebinding { candidate, .. } = self.phase else {
            // When: `phase` is not `Rebinding`, no candidate waits for this commit report.
            self.counts.unexpected += 1;
            return CommitDecision::Unexpected;
        };
        if report.candidate != candidate {
            // When: `report.candidate` differs from the pending `candidate`, it reports another device.
            self.counts.unexpected += 1;
            return CommitDecision::Unexpected;
        }
        if report.rebound == report.live && report.gate.accepts_gpu_work() {
            // When: `rebound` equals `live` and the candidate `gate` accepts work, no renderer was left behind.
            let retired = RetiredGeneration(self.committed);
            self.committed = candidate;
            self.first_present_at = None;
            self.unusable_reported = false;
            self.phase = RecoveryPhase::Active;
            self.counts.rebuilds += 1;
            return CommitDecision::Committed { generation: candidate, retired };
        }
        self.counts.failed_attempts += 1;
        self.counts.discarded += 1;
        CommitDecision::Discarded {
            retired: RetiredGeneration(candidate),
            next: self.next_attempt(now),
        }
    }

    fn schedule_after_loss(&mut self, now: Instant) -> Next {
        if self.committed_is_stable(now) {
            // A loss at least `stable_after` after the first presented frame ends the
            // flapping that spent the earlier budget.
            self.attempts_used = 0;
        }
        self.next_attempt(now)
    }

    fn committed_is_stable(&self, now: Instant) -> bool {
        self.first_present_at
            .is_some_and(|first| now.saturating_duration_since(first) >= self.policy.stable_after)
    }

    fn next_attempt(&mut self, now: Instant) -> Next {
        let Some(&delay) = self.policy.backoff.get(self.attempts_used) else {
            // When: `backoff` has no entry for `attempts_used`, this budget has no attempt left.
            self.phase = RecoveryPhase::Exhausted;
            return Next::Exhausted;
        };
        let due = later(now, delay);
        let attempt = self.attempts_used + 1;
        self.phase = RecoveryPhase::Scheduled { due, attempt };
        Next::Retry { due, attempt }
    }

    fn start_due_attempt(&mut self, attempt: usize, now: Instant) -> PollDecision {
        self.attempts_used += 1;
        if let Some(overdue) = self.overdue_ticket {
            // When: `overdue_ticket` is set, a second worker could strand another blocked thread.
            self.counts.failed_attempts += 1;
            self.counts.worker_busy += 1;
            return PollDecision::WorkerBusy { overdue, next: self.next_attempt(now) };
        }
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.counts.requests += 1;
        let deadline = later(now, self.policy.request_timeout);
        self.phase = RecoveryPhase::Requesting { ticket, attempt, deadline };
        PollDecision::StartRequest { ticket, attempt, deadline }
    }

    fn time_out_request(&mut self, ticket: u64, now: Instant) -> PollDecision {
        self.overdue_ticket = Some(ticket);
        self.counts.failed_attempts += 1;
        self.counts.timed_out += 1;
        PollDecision::TimedOut { ticket, next: self.next_attempt(now) }
    }

    /// The token for a device a discarded result created. None is minted for the
    /// committed generation or the pending candidate, because renderers use them.
    fn retire_discarded(&self, outcome: RequestOutcome) -> Option<RetiredGeneration> {
        match outcome {
            RequestOutcome::Created { generation } if !self.is_in_use(generation) => {
                Some(RetiredGeneration(generation))
            }
            RequestOutcome::Created { .. } | RequestOutcome::Failed => None,
        }
    }

    /// Whether renderers use `generation`: it is committed or the pending candidate.
    fn is_in_use(&self, generation: u64) -> bool {
        let pending = match self.phase {
            RecoveryPhase::Rebinding { candidate, .. } => Some(candidate),
            _ => None,
        };
        generation == self.committed || pending == Some(generation)
    }
}

/// `now` plus `delay`. An unrepresentable instant falls back to `now`, which
/// only makes the attempt due at once.
fn later(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or(now)
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;
