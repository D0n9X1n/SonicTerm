use super::*;
use sonicterm_gpu::recovery::RecoveryCounts;

fn snapshot() -> RecoverySnapshot {
    RecoverySnapshot {
        committed: 2,
        phase: RecoveryPhase::Active,
        attempts_used: 1,
        worker_busy: false,
        counts: RecoveryCounts { rebuilds: 1, requests: 1, ..RecoveryCounts::default() },
    }
}

/// A replacement is credited only after one committed request on a genuinely new generation.
#[test]
fn exactly_one_new_active_generation_satisfies_recovery() {
    let good = snapshot();
    assert!(one_rebuild(good, 1));
    assert!(!one_rebuild(good, 2));
    assert!(!one_rebuild(RecoverySnapshot { phase: RecoveryPhase::Exhausted, ..good }, 1));
    for counts in [
        RecoveryCounts { requests: 0, ..good.counts },
        RecoveryCounts { requests: 2, ..good.counts },
        RecoveryCounts { rebuilds: 0, ..good.counts },
        RecoveryCounts { rebuilds: 2, ..good.counts },
        RecoveryCounts { failed_attempts: 1, ..good.counts },
    ] {
        assert!(!one_rebuild(RecoverySnapshot { counts, ..good }, 1));
    }
}

/// A fresh probe owns no native window or shell before production window creation establishes identities.
#[test]
fn recovery_probe_starts_without_native_custody() {
    let now = Instant::now();
    let probe = RecoveryProbe::new(now);
    assert_eq!(probe.started, now);
    assert_eq!(probe.stage, Stage::Create);
    assert!(probe.child.is_none());
    assert!(probe.panes.is_empty());
    assert!(probe.original_state.is_none());
    assert_eq!(DEADLINE, Duration::from_secs(24));
    assert!(QUIET_INTERVAL < DEADLINE);
    assert_eq!(probe.recovered_generation, None);
    assert!(!probe.stale_event_observed);
}

/// The native oracle must consume the same marker-bearing plan before compatibility conversion discards the typed outcome.
#[test]
fn recovery_marker_proof_is_bound_to_each_present_callback() {
    for source in [include_str!("window_event.rs"), include_str!("child_window.rs")] {
        let render = source.find("let outcome = r.render_with_outcome(").unwrap();
        let evidence = source.find("smoke.observe_recovery_frame(").unwrap();
        let call = source[evidence..].split_once(");").unwrap().0;
        assert!(call.contains("r.device_generation()") && call.contains("&panes_slice"));
        let compatibility = source.find("outcome.into_render_result()").unwrap();
        assert!(render < evidence && evidence < compatibility);
    }
    let source = include_str!("gpu_recovery_smoke.rs");
    let observe = source.split_once("pub(super) fn observe_frame(").unwrap().1;
    let observe = observe.split_once("impl App").unwrap().0;
    assert!(observe.contains("PresentOutcome::Presented"));
    assert!(observe.contains("proof.window == window"));
    assert!(observe.contains("pane.id == proof.pane"));
    assert!(observe.contains("grid_marker_rows(pane.grid, marker) > proof.marker_rows"));
    assert!(observe.contains("visible_marker(pane.grid, pane.viewport_top_abs, marker)"));
    assert!(observe
        .contains("accepts_proof_generation(self.stage, self.original_generation, generation)"));
    assert!(observe.contains("proof.presented_generation = Some(generation)"));
}

/// A pre-loss present never proves recovery, and post-loss observations are accepted only in the recovery stage.
#[test]
fn marker_generation_must_match_the_active_proof_stage() {
    let now = Instant::now();
    assert!(accepts_proof_generation(Stage::Baseline, 7, 7));
    assert!(!accepts_proof_generation(Stage::Baseline, 7, 8));
    assert!(!accepts_proof_generation(Stage::Recovering, 7, 7));
    assert!(accepts_proof_generation(Stage::Recovering, 7, 8));
    for stage in [Stage::Create, Stage::Capture, Stage::Quiet { until: now }, Stage::Release] {
        assert!(!accepts_proof_generation(stage, 7, 7));
        assert!(!accepts_proof_generation(stage, 7, 8));
    }
}

/// A marker retained only in history is not evidence that the current viewport displayed it.
#[test]
fn visible_marker_rejects_hidden_scrollback_and_obeys_the_viewport() {
    let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(20, 2));
    parser.advance(b"marker\r\nnext\r\nlast");
    let grid = parser.grid();
    assert_eq!(grid_marker_rows(grid, "marker"), 1);
    assert!(grid.scrollback_len() > 0);
    assert!(!visible_marker(grid, None, "marker"));
    assert!(visible_marker(grid, Some(0), "marker"));
    assert!(!visible_marker(grid, Some(u64::MAX), "marker"));
    assert!(visible_marker(grid, None, "last"));
}

/// The stale-event oracle waits for event-loop delivery and excludes callbacks observed before its quiet interval.
#[test]
fn stale_device_event_must_arrive_during_the_quiet_stage() {
    let now = Instant::now();
    let mut probe = RecoveryProbe::new(now);
    probe.original_generation = 7;
    probe.observe_device_event(7);
    assert!(!probe.stale_event_observed);
    probe.stage = Stage::Quiet { until: now + QUIET_INTERVAL };
    probe.observe_device_event(8);
    assert!(!probe.stale_event_observed);
    probe.observe_device_event(7);
    assert!(probe.stale_event_observed);
}

/// Teardown still enforces the committed generation and request count, not merely a shrinking renderer total.
#[test]
fn recovery_release_rechecks_identity_and_uses_production_pool_shrink() {
    let source = include_str!("gpu_recovery_smoke.rs");
    let release = source.split_once("            Stage::Release => {").unwrap().1;
    let release = release.split_once("        Ok(false)").unwrap().0;
    assert!(release.contains("one_rebuild(snapshot, probe.original_generation)"));
    assert!(release.contains("probe.recovered_generation != Some(snapshot.committed)"));
    assert!(release.contains("!probe.stale_event_observed"));
    assert!(release.contains("renderer.device_generation() != snapshot.committed"));
    assert!(source.contains("self.warm_window_pool_maintain(el)"));
    assert!(!source.contains("self.warm_window_pool.clear()"));
    let event = include_str!("event_loop.rs");
    let tagged =
        event.split_once("UserEvent::GpuDeviceGenerationChanged { generation } => {").unwrap().1;
    let tagged = tagged.split_once("UserEvent::GpuRecoveryReady").unwrap().0;
    assert!(
        tagged.find("self.gpu_generation_changed").unwrap()
            < tagged.find("smoke.observe_recovery_device_event").unwrap()
    );
}

/// A slow startup can leave less than 24 seconds; the earlier watchdog must still log the active phase and its elapsed time.
#[test]
fn process_watchdog_reports_the_probe_stage_before_its_own_deadline() {
    let now = Instant::now();
    let mut state = RuntimeSmokeState::new(20);
    state.scenario = super::super::RuntimeSmokeScenario::DeviceRecovery;
    state.phase = RuntimeSmokePhase::RecoveryReady;
    let mut probe = RecoveryProbe::new(now);
    probe.stage = Stage::Recovering;
    state.recovery_probe = Some(probe);
    let log = super::super::runtime_smoke_tests::capture_timeout(|| {
        state.fail_from_watchdog(now + Duration::from_secs(3));
    });
    assert_eq!(state.outcome(), Some(Err(FAILURE)));
    assert!(log.contains("cause=\"watchdog\""));
    assert!(log.contains("stage=Recovering"));
    assert!(log.contains("elapsed_ms=3000"));
}

/// Production recovery runs before smoke observation, while its dedicated timer is included in the event-loop deadline.
#[test]
fn native_recovery_smoke_runs_after_the_production_recovery_service() {
    let source = include_str!("event_loop.rs");
    let wait = source.split_once("pub(super) fn do_about_to_wait(").unwrap().1;
    let production = wait.find("self.service_gpu_recovery(el, Instant::now())").unwrap();
    let warm = wait.find("self.warm_window_pool_maintain(el)").unwrap();
    let smoke = wait.find("self.drive_gpu_recovery_smoke(el, Instant::now())").unwrap();
    assert!(production < warm && warm < smoke);
    assert!(wait.contains("self.gpu_recovery_smoke_deadline()"));
}
