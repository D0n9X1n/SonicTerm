use super::*;

/// A fixed fake clock: every instant is an offset from one captured base, so no
/// test sleeps and no decision depends on elapsed wall time.
struct Clock(Instant);

impl Clock {
    fn new() -> Self {
        Self(Instant::now())
    }

    fn at(&self, millis: u64) -> Instant {
        self.0 + Duration::from_millis(millis)
    }
}

fn gate(state: DeviceState) -> DeviceGate {
    DeviceGate { state, destroy_requested: false }
}

fn destroyed(state: DeviceState) -> DeviceGate {
    DeviceGate { state, destroy_requested: true }
}

fn coordinator(generation: u64) -> RecoveryCoordinator {
    RecoveryCoordinator::new(RecoveryPolicy::proposed(), generation)
}

/// Start the attempt due at `now`, create `candidate`, and rebind all `live`
/// renderers to it with its gate open.
fn recover(
    c: &mut RecoveryCoordinator,
    candidate: u64,
    live: usize,
    now: Instant,
) -> CommitDecision {
    let PollDecision::StartRequest { ticket, .. } = c.poll(now) else {
        panic!("an attempt should be due at {now:?}");
    };
    let rebind = c.request_finished(ticket, RequestOutcome::Created { generation: candidate }, now);
    assert_eq!(rebind, RequestDecision::Rebind { candidate, attempt: c.snapshot().attempts_used });
    let report = CommitReport { candidate, rebound: live, live, gate: gate(DeviceState::Usable) };
    c.finish_rebind(report, now)
}

/// A loss of the committed generation schedules the first attempt at once, and
/// its request is bounded by the proposed 10 s timeout.
#[test]
fn committed_loss_schedules_an_immediate_first_attempt() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    let decision = c.observe_stop(1, gate(DeviceState::Lost), clock.at(0));
    assert_eq!(decision, StopDecision::Lost(Next::Retry { due: clock.at(0), attempt: 1 }));
    let PollDecision::StartRequest { attempt, deadline, .. } = c.poll(clock.at(0)) else {
        panic!("the first attempt should start at once");
    };
    assert_eq!((attempt, deadline), (1, clock.at(10_000)));
}

/// Recovery covers a lost device only: `Unusable` is reported once and schedules
/// nothing, and a later loss of the same generation still recovers.
#[test]
fn unusable_without_loss_is_deferred_until_the_generation_is_lost() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    let first = c.observe_stop(1, gate(DeviceState::Unusable), clock.at(0));
    assert_eq!(first, StopDecision::DeferredUnusable);
    let again = c.observe_stop(1, gate(DeviceState::Unusable), clock.at(1));
    assert_eq!(again, StopDecision::Duplicate);
    assert_eq!(c.poll(clock.at(2)), PollDecision::Idle);
    let lost = c.observe_stop(1, gate(DeviceState::Lost), clock.at(3));
    assert_eq!(lost, StopDecision::Lost(Next::Retry { due: clock.at(3), attempt: 1 }));
}

/// A destroy request alone is not a loss, and a requested loss is still
/// recovered: staleness comes from the generation, never from `destroy_requested`.
#[test]
fn destroy_request_neither_starts_nor_suppresses_recovery() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    let pending = c.observe_stop(1, destroyed(DeviceState::Usable), clock.at(0));
    assert_eq!(pending, StopDecision::NotStopped);
    let lost = c.observe_stop(1, destroyed(DeviceState::Lost), clock.at(1));
    assert_eq!(lost, StopDecision::Lost(Next::Retry { due: clock.at(1), attempt: 1 }));
}

/// Repeated stops of the committed generation, from several windows or from
/// both wake paths, lead to exactly one rebuild.
#[test]
fn repeated_stops_of_one_generation_rebuild_once() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    let first = c.observe_stop(1, gate(DeviceState::Lost), clock.at(0));
    assert!(matches!(first, StopDecision::Lost(Next::Retry { .. })));
    for _ in 0..2 {
        assert_eq!(
            c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
            StopDecision::Duplicate
        );
    }
    let commit = recover(&mut c, 2, 3, clock.at(0));
    assert!(matches!(commit, CommitDecision::Committed { generation: 2, .. }));
    let counts = c.snapshot().counts;
    assert_eq!((counts.rebuilds, counts.duplicate_stops), (1, 2));
}

/// A commit retires the old generation before the app can destroy it, so its
/// later callbacks, including the loss an intentional destroy raises, are stale.
#[test]
fn retired_generation_callbacks_are_stale_after_commit() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let CommitDecision::Committed { generation, retired } = recover(&mut c, 2, 2, clock.at(5))
    else {
        panic!("the attempt should commit");
    };
    assert_eq!((generation, retired.generation(), c.committed()), (2, 1, 2));
    let late = c.observe_stop(1, destroyed(DeviceState::Lost), clock.at(6));
    assert_eq!(late, StopDecision::Stale);
    assert_eq!(c.phase(), RecoveryPhase::Active);
    assert_eq!(c.snapshot().counts.rebuilds, 1);
}

/// A rebind that misses a live renderer, or whose candidate stops while its
/// surfaces are configured, discards the candidate; nothing commits until one
/// attempt rebinds every live renderer.
#[test]
fn partial_commit_discards_the_candidate_and_requires_every_renderer_next() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(0));
    assert!(matches!(created, RequestDecision::Rebind { candidate: 2, .. }));
    let partial =
        CommitReport { candidate: 2, rebound: 2, live: 3, gate: gate(DeviceState::Usable) };
    assert_eq!(
        c.finish_rebind(partial, clock.at(1)),
        CommitDecision::Discarded {
            retired: RetiredGeneration(2),
            next: Next::Retry { due: clock.at(251), attempt: 2 },
        }
    );
    assert_eq!(c.committed(), 1);
    assert_eq!(c.observe_stop(2, gate(DeviceState::Unusable), clock.at(2)), StopDecision::Stale);

    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(251)) else {
        panic!("attempt 2 should start");
    };
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 3 }, clock.at(251));
    assert!(matches!(created, RequestDecision::Rebind { candidate: 3, .. }));
    let stopped =
        CommitReport { candidate: 3, rebound: 3, live: 3, gate: gate(DeviceState::Unusable) };
    assert!(matches!(c.finish_rebind(stopped, clock.at(252)), CommitDecision::Discarded { .. }));
    assert_eq!(c.committed(), 1);

    let CommitDecision::Committed { generation, retired } = recover(&mut c, 4, 3, clock.at(1_252))
    else {
        panic!("attempt 3 should commit");
    };
    assert_eq!((generation, retired.generation()), (4, 1));
    assert_eq!(c.snapshot().counts.discarded, 2);
}

/// A device request that fails during a retry costs one attempt, and the next
/// attempt can still commit.
#[test]
fn fault_during_retry_is_handled_and_the_next_attempt_commits() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    assert_eq!(
        c.request_finished(ticket, RequestOutcome::Failed, clock.at(10)),
        RequestDecision::Failed(Next::Retry { due: clock.at(260), attempt: 2 })
    );
    let commit = recover(&mut c, 2, 2, clock.at(260));
    assert!(matches!(commit, CommitDecision::Committed { generation: 2, .. }));
    let counts = c.snapshot().counts;
    assert_eq!((counts.failed_attempts, counts.rebuilds), (1, 1));
}

/// Attempts follow the proposed schedule exactly (0, 250 ms, 1 s, 4 s, 16 s)
/// and stop at its bound of five.
#[test]
fn retries_follow_the_proposed_schedule_and_stop_at_the_bound() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let mut now = 0;
    for (attempt, gap) in [(1_usize, 250_u64), (2, 1_000), (3, 4_000), (4, 16_000)] {
        if now > 0 {
            assert_eq!(c.poll(clock.at(now - 1)), PollDecision::WaitUntil(clock.at(now)));
        }
        let PollDecision::StartRequest { ticket, attempt: started, .. } = c.poll(clock.at(now))
        else {
            panic!("attempt {attempt} should start at {now} ms");
        };
        assert_eq!(started, attempt);
        assert_eq!(
            c.request_finished(ticket, RequestOutcome::Failed, clock.at(now)),
            RequestDecision::Failed(Next::Retry { due: clock.at(now + gap), attempt: attempt + 1 })
        );
        now += gap;
    }
    let PollDecision::StartRequest { ticket, attempt, .. } = c.poll(clock.at(now)) else {
        panic!("attempt 5 should start at {now} ms");
    };
    assert_eq!((attempt, now), (5, 21_250));
    let last = c.request_finished(ticket, RequestOutcome::Failed, clock.at(now));
    assert_eq!(last, RequestDecision::Failed(Next::Exhausted));
    assert_eq!(c.poll(clock.at(now + 60_000)), PollDecision::Exhausted);
    let after = c.observe_stop(1, gate(DeviceState::Lost), clock.at(now + 60_000));
    assert_eq!(after, StopDecision::Exhausted);
    assert_eq!(c.snapshot().counts.requests, 5);
}

/// Each new generation that dies before it is stable continues the same budget,
/// so a device that fails at once cannot rebuild without bound.
#[test]
fn budget_spans_generations_when_each_new_device_dies_at_once() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    let mut now = 0;
    for (lost, gap) in [(1_u64, 0_u64), (2, 250), (3, 1_000), (4, 4_000), (5, 16_000)] {
        let decision = c.observe_stop(lost, gate(DeviceState::Lost), clock.at(now));
        let StopDecision::Lost(Next::Retry { due, .. }) = decision else {
            panic!("the loss of generation {lost} should schedule an attempt");
        };
        assert_eq!(due, clock.at(now + gap));
        now += gap;
        let commit = recover(&mut c, lost + 1, 1, clock.at(now));
        assert!(matches!(commit, CommitDecision::Committed { .. }));
    }
    let last = c.observe_stop(6, gate(DeviceState::Lost), clock.at(now));
    assert_eq!(last, StopDecision::Lost(Next::Exhausted));
    assert_eq!(c.snapshot().counts.rebuilds, 5);
}

/// Only a loss at least the stability window after a generation's first
/// presented frame starts a fresh budget; presenting on a stale generation or
/// losing it early continues the budget.
#[test]
fn only_a_stable_generation_starts_a_fresh_budget() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    assert!(matches!(recover(&mut c, 2, 1, clock.at(0)), CommitDecision::Committed { .. }));
    c.observe_presented(2, clock.at(10));
    let early = c.observe_stop(2, gate(DeviceState::Lost), clock.at(29_999));
    assert_eq!(early, StopDecision::Lost(Next::Retry { due: clock.at(30_249), attempt: 2 }));

    assert!(matches!(recover(&mut c, 3, 1, clock.at(30_249)), CommitDecision::Committed { .. }));
    c.observe_presented(2, clock.at(30_300));
    let unpresented = c.observe_stop(3, gate(DeviceState::Lost), clock.at(90_000));
    assert_eq!(unpresented, StopDecision::Lost(Next::Retry { due: clock.at(91_000), attempt: 3 }));

    assert!(matches!(recover(&mut c, 4, 1, clock.at(91_000)), CommitDecision::Committed { .. }));
    c.observe_presented(4, clock.at(91_010));
    let stable = c.observe_stop(4, gate(DeviceState::Lost), clock.at(121_010));
    assert_eq!(stable, StopDecision::Lost(Next::Retry { due: clock.at(121_010), attempt: 1 }));
}

/// A request that never returns fails at its deadline; later attempts fail as
/// busy instead of starting a second worker, the budget still ends on schedule,
/// and the late device comes back retired.
#[test]
fn hung_request_never_starts_a_second_worker_and_still_exhausts() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, deadline, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    assert_eq!(c.poll(clock.at(9_999)), PollDecision::WaitUntil(deadline));
    assert_eq!(
        c.poll(clock.at(10_000)),
        PollDecision::TimedOut { ticket, next: Next::Retry { due: clock.at(10_250), attempt: 2 } }
    );
    for (at, next) in [
        (10_250_u64, Next::Retry { due: clock.at(11_250), attempt: 3 }),
        (11_250, Next::Retry { due: clock.at(15_250), attempt: 4 }),
        (15_250, Next::Retry { due: clock.at(31_250), attempt: 5 }),
        (31_250, Next::Exhausted),
    ] {
        assert_eq!(c.poll(clock.at(at)), PollDecision::WorkerBusy { overdue: ticket, next });
    }
    let counts = c.snapshot().counts;
    assert_eq!((counts.requests, counts.worker_busy, counts.timed_out), (1, 4, 1));
    assert_eq!(
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(40_000)),
        RequestDecision::Discard(Some(RetiredGeneration(2)))
    );
    assert_eq!(c.committed(), 1);
    assert!(!c.snapshot().worker_busy);
    assert_eq!(c.phase(), RecoveryPhase::Exhausted);
}

/// A result that arrives after `poll` timed its attempt out frees the worker,
/// comes back retired without a second timeout, its generation stays stale, and
/// the next due attempt starts a fresh request.
#[test]
fn late_result_is_discarded_and_frees_the_worker() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket: first, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    assert!(matches!(c.poll(clock.at(10_000)), PollDecision::TimedOut { .. }));
    assert_eq!(
        c.request_finished(first, RequestOutcome::Created { generation: 2 }, clock.at(10_001)),
        RequestDecision::Discard(Some(RetiredGeneration(2)))
    );
    assert_eq!(c.observe_stop(2, gate(DeviceState::Lost), clock.at(10_002)), StopDecision::Stale);
    assert_eq!(c.snapshot().counts.timed_out, 1);
    let CommitDecision::Committed { generation, retired } = recover(&mut c, 3, 1, clock.at(10_250))
    else {
        panic!("attempt 2 should start a fresh request and commit");
    };
    assert_eq!((generation, retired.generation()), (3, 1));
    assert_eq!(c.snapshot().counts.requests, 2);
}

/// Results and reports that match nothing pending change nothing, and any
/// device such a result created comes back retired.
#[test]
fn unmatched_results_and_reports_change_nothing() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert_eq!(
        c.request_finished(9, RequestOutcome::Created { generation: 5 }, clock.at(0)),
        RequestDecision::Discard(Some(RetiredGeneration(5)))
    );
    let stray = CommitReport { candidate: 5, rebound: 1, live: 1, gate: gate(DeviceState::Usable) };
    assert_eq!(c.finish_rebind(stray, clock.at(0)), CommitDecision::Unexpected);
    assert_eq!(c.phase(), RecoveryPhase::Active);

    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(1)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(1)) else {
        panic!("attempt 1 should start");
    };
    let other = c.request_finished(ticket + 1, RequestOutcome::Failed, clock.at(2));
    assert_eq!(other, RequestDecision::Discard(None));
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 6 }, clock.at(3));
    assert_eq!(created, RequestDecision::Rebind { candidate: 6, attempt: 1 });
    assert_eq!(c.poll(clock.at(3)), PollDecision::AwaitingRebind { candidate: 6 });
    let wrong = CommitReport { candidate: 7, rebound: 1, live: 1, gate: gate(DeviceState::Usable) };
    assert_eq!(c.finish_rebind(wrong, clock.at(4)), CommitDecision::Unexpected);
    assert_eq!(c.phase(), RecoveryPhase::Rebinding { attempt: 1, candidate: 6 });
    assert_eq!(c.snapshot().counts.unexpected, 4);
}

/// The proposed policy is five attempts over 21.25 s with a 10 s request bound
/// and a 30 s stability window; changing the policy must change this test.
#[test]
fn proposed_policy_pins_the_reviewed_constants() {
    let policy = RecoveryPolicy::proposed();
    assert_eq!(policy.max_attempts(), 5);
    assert_eq!(policy.backoff.iter().sum::<Duration>(), Duration::from_millis(21_250));
    assert_eq!(policy.request_timeout, Duration::from_secs(10));
    assert_eq!(policy.stable_after, Duration::from_secs(30));
}

/// A result that reaches its deadline before `poll` sees the timeout, exactly at or just after
/// it, times the attempt out once, retires the new device, and frees the worker instead of
/// leaving it overdue, so the next attempt starts a fresh request.
#[test]
fn result_at_or_after_its_deadline_times_out_once_before_poll() {
    for at in [10_000_u64, 10_001] {
        let clock = Clock::new();
        let mut c = coordinator(1);
        assert!(matches!(
            c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
            StopDecision::Lost(_)
        ));
        let PollDecision::StartRequest { ticket, deadline, .. } = c.poll(clock.at(0)) else {
            panic!("attempt 1 should start");
        };
        assert_eq!(deadline, clock.at(10_000));
        assert_eq!(
            c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(at)),
            RequestDecision::TimedOut {
                retired: Some(RetiredGeneration(2)),
                next: Next::Retry { due: clock.at(at + 250), attempt: 2 },
            },
            "a result at {at} ms"
        );
        let snapshot = c.snapshot();
        assert_eq!((snapshot.counts.timed_out, snapshot.counts.failed_attempts), (1, 1));
        assert!(!snapshot.worker_busy);
        assert_eq!(c.committed(), 1);
        let PollDecision::StartRequest { attempt, .. } = c.poll(clock.at(at + 250)) else {
            panic!("attempt 2 should start a fresh request after a result at {at} ms");
        };
        assert_eq!((attempt, c.snapshot().counts.timed_out), (2, 1));
    }
}

/// A result one millisecond before its deadline still rebinds its candidate.
#[test]
fn result_just_before_its_deadline_still_rebinds() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(9_999));
    assert_eq!(created, RequestDecision::Rebind { candidate: 2, attempt: 1 });
    assert_eq!(c.snapshot().counts.timed_out, 0);
}

/// Stability is measured from the first presented frame, not from the commit: a generation
/// committed at 0 and first presented at 29.999 s is not stable when lost at 30 s.
#[test]
fn stability_counts_from_the_first_presented_frame_not_the_commit() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    assert!(matches!(recover(&mut c, 2, 1, clock.at(0)), CommitDecision::Committed { .. }));
    c.observe_presented(2, clock.at(29_999));
    let lost = c.observe_stop(2, gate(DeviceState::Lost), clock.at(30_000));
    assert_eq!(lost, StopDecision::Lost(Next::Retry { due: clock.at(30_250), attempt: 2 }));
}

/// A loss 30 s after the first presented frame starts a fresh budget: attempt 1, no delay.
#[test]
fn loss_thirty_seconds_after_the_first_presented_frame_resets_the_budget() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    assert!(matches!(recover(&mut c, 2, 1, clock.at(0)), CommitDecision::Committed { .. }));
    c.observe_presented(2, clock.at(29_999));
    let lost = c.observe_stop(2, gate(DeviceState::Lost), clock.at(59_999));
    assert_eq!(lost, StopDecision::Lost(Next::Retry { due: clock.at(59_999), attempt: 1 }));
}

/// A second result for the ticket whose device was committed is refused without a token, so
/// the app can never destroy the committed generation.
#[test]
fn duplicate_result_after_commit_never_retires_the_committed_generation() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(1));
    assert_eq!(created, RequestDecision::Rebind { candidate: 2, attempt: 1 });
    let report =
        CommitReport { candidate: 2, rebound: 1, live: 1, gate: gate(DeviceState::Usable) };
    assert!(matches!(
        c.finish_rebind(report, clock.at(2)),
        CommitDecision::Committed { generation: 2, .. }
    ));
    let duplicate =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(3));
    assert_eq!(duplicate, RequestDecision::Discard(None));
    assert_eq!((c.committed(), c.phase()), (2, RecoveryPhase::Active));
}

/// A second result for the ticket whose device is being rebound is refused without a token, so
/// the pending candidate stays pending and can still commit.
#[test]
fn duplicate_result_while_rebinding_never_retires_the_pending_candidate() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    let created =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(1));
    assert_eq!(created, RequestDecision::Rebind { candidate: 2, attempt: 1 });
    let duplicate =
        c.request_finished(ticket, RequestOutcome::Created { generation: 2 }, clock.at(2));
    assert_eq!(duplicate, RequestDecision::Discard(None));
    assert_eq!(c.phase(), RecoveryPhase::Rebinding { attempt: 1, candidate: 2 });
    let report =
        CommitReport { candidate: 2, rebound: 1, live: 1, gate: gate(DeviceState::Usable) };
    assert!(matches!(
        c.finish_rebind(report, clock.at(3)),
        CommitDecision::Committed { generation: 2, .. }
    ));
}

/// A result that names the committed generation cannot be a new device: the attempt fails
/// before any rebind and no token is minted for the committed device.
#[test]
fn result_naming_the_committed_generation_is_refused_before_rebind() {
    let clock = Clock::new();
    let mut c = coordinator(1);
    assert!(matches!(
        c.observe_stop(1, gate(DeviceState::Lost), clock.at(0)),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = c.poll(clock.at(0)) else {
        panic!("attempt 1 should start");
    };
    assert_eq!(
        c.request_finished(ticket, RequestOutcome::Created { generation: 1 }, clock.at(5)),
        RequestDecision::Failed(Next::Retry { due: clock.at(255), attempt: 2 })
    );
    assert_eq!(c.committed(), 1);
    assert!(matches!(c.phase(), RecoveryPhase::Scheduled { attempt: 2, .. }));
}
