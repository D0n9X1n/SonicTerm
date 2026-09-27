use super::*;
use sonicterm_gpu::core::{SkipReason, SurfaceRetryReason, SuspendedContext};
use sonicterm_gpu::device_errors::DeviceState;

fn gate(state: DeviceState) -> DeviceGate {
    DeviceGate { state, destroy_requested: false }
}

fn recovered_coordinator(now: Instant) -> RecoveryCoordinator {
    let mut coordinator = RecoveryCoordinator::new(RecoveryPolicy::proposed(), 1);
    assert!(matches!(
        coordinator.observe_stop(1, gate(DeviceState::Lost), now),
        StopDecision::Lost(_)
    ));
    let PollDecision::StartRequest { ticket, .. } = coordinator.poll(now) else {
        panic!("loss must admit a request");
    };
    assert!(matches!(
        coordinator.request_finished(ticket, RequestOutcome::Created { generation: 2 }, now),
        RequestDecision::Rebind { .. }
    ));
    let CommitDecision::Committed { retired, .. } = coordinator.finish_rebind(
        CommitReport { candidate: 2, rebound: 3, live: 3, gate: gate(DeviceState::Usable) },
        now,
    ) else {
        panic!("two live renderers and their warm peer must commit together");
    };
    assert_eq!(retired.generation(), 1);
    coordinator
}

/// Idle and exhausted coordinators wake only while an actual result still needs disposal.
#[test]
fn recovery_deadlines_preserve_late_result_disposal_without_idle_polling() {
    let now = Instant::now();
    assert_eq!(deadline(RecoveryPhase::Active, None), None);
    assert_eq!(deadline(RecoveryPhase::Exhausted, None), None);
    let poll = now + Duration::from_secs(1);
    assert_eq!(deadline(RecoveryPhase::Exhausted, Some(poll)), Some(poll));
    let retry = now + Duration::from_millis(250);
    assert_eq!(
        deadline(RecoveryPhase::Scheduled { due: retry, attempt: 2 }, Some(poll)),
        Some(retry)
    );
    assert_eq!(
        deadline(RecoveryPhase::Requesting { ticket: 1, attempt: 1, deadline: retry }, Some(now)),
        Some(now)
    );
}

/// Continuous unrelated input cannot postpone a pending result poll indefinitely.
#[test]
fn recovery_result_poll_keeps_its_first_future_deadline() {
    let now = Instant::now();
    let due = now + RESULT_POLL_INTERVAL;
    assert_eq!(result_poll_deadline(None, true, false, now), Some(due));
    assert_eq!(
        result_poll_deadline(Some(due), true, false, now + Duration::from_millis(99)),
        Some(due)
    );
    assert_eq!(result_poll_deadline(Some(due), true, false, due), Some(due + RESULT_POLL_INTERVAL));
    assert_eq!(result_poll_deadline(Some(due), false, false, now), None);
}

/// Exhausting requests reduces disposal polling without abandoning a still-owned native result.
#[test]
fn exhausted_recovery_keeps_a_slow_disposal_deadline() {
    let now = Instant::now();
    assert_eq!(result_poll_deadline(None, true, true, now), Some(now + EXHAUSTED_POLL_INTERVAL));
    assert_eq!(result_poll_deadline(Some(now), false, true, now), None);
}

/// Cached, skipped, retry, unavailable and failed frames cannot reset a flapping device's budget.
#[test]
fn only_presented_outcomes_start_recovery_stability() {
    let now = Instant::now();
    let outcomes = [
        PresentOutcome::Skipped(SkipReason::Unchanged),
        PresentOutcome::CachedReblit,
        PresentOutcome::AtlasRetry,
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout),
        PresentOutcome::RenderingUnavailable(SuspendedContext {
            generation: 2,
            gate: gate(DeviceState::Lost),
            reports_stop: true,
        }),
        PresentOutcome::Failed(anyhow::anyhow!("synthetic presentation refusal")),
    ];
    for outcome in outcomes {
        let mut coordinator = recovered_coordinator(now);
        observe_frame(&mut coordinator, 2, &outcome, now);
        assert!(matches!(
            coordinator.observe_stop(2, gate(DeviceState::Lost), now + Duration::from_secs(31)),
            StopDecision::Lost(Next::Retry { attempt: 2, .. })
        ));
    }
    let mut coordinator = recovered_coordinator(now);
    observe_frame(&mut coordinator, 2, &PresentOutcome::Presented, now);
    assert!(matches!(
        coordinator.observe_stop(2, gate(DeviceState::Lost), now + Duration::from_secs(31)),
        StopDecision::Lost(Next::Retry { attempt: 1, .. })
    ));
}

/// A real present on an old generation cannot authorize a fresh retry budget for its successor.
#[test]
fn stale_presented_generation_does_not_start_stability() {
    let now = Instant::now();
    let mut coordinator = recovered_coordinator(now);
    observe_frame(&mut coordinator, 1, &PresentOutcome::Presented, now);
    assert!(matches!(
        coordinator.observe_stop(2, gate(DeviceState::Lost), now + Duration::from_secs(31)),
        StopDecision::Lost(Next::Retry { attempt: 2, .. })
    ));
}

/// Normal App construction remains headless-safe and starts no recovery worker or recovery timer.
#[test]
fn headless_app_has_no_recovery_lifecycle() {
    let app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    assert!(app.gpu_recovery.is_none());
    assert_eq!(app.gpu_recovery_deadline(), None);
    assert!(!app.wake_is_gpu_recovery_only);
}

/// The callback integration retains compatibility events and tags real device and completion hints.
#[test]
fn recovery_events_keep_generation_and_ticket_identity() {
    assert_ne!(
        UserEvent::GpuDeviceGenerationChanged { generation: 1 },
        UserEvent::GpuDeviceGenerationChanged { generation: 2 }
    );
    assert_ne!(
        UserEvent::GpuRecoveryReady { ticket: 1 },
        UserEvent::GpuRecoveryReady { ticket: 2 }
    );
    assert_ne!(
        UserEvent::GpuDeviceStateChanged,
        UserEvent::GpuDeviceGenerationChanged { generation: 1 }
    );
}
