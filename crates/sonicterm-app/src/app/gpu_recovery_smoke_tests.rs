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

/// The recovery-marker scan over one checkout of the two adapters and the probe, read as LF.
fn check_recovery_marker_proof(main: &str, child: &str, probe: &str) {
    for source in [main.replace("\r\n", "\n"), child.replace("\r\n", "\n")] {
        let sample = source.find("smoke.recovery_marker_sample(").unwrap();
        let render = source.find("r.render_releasing(").unwrap();
        let evidence = source.find("smoke.observe_recovery_frame(").unwrap();
        let call = source[evidence..].split_once(");").unwrap().0;
        assert!(call.contains("r.device_generation()") && call.contains("sample"));
        assert!(!source.contains("panes_slice"));
        let compatibility = source.find("outcome.into_render_result()").unwrap();
        assert!(sample < render && render < evidence && evidence < compatibility);
    }
    let source = probe.replace("\r\n", "\n");
    let observe = source.split_once("pub(super) fn observe_frame(").unwrap().1;
    let observe = observe.split_once("impl App").unwrap().0;
    assert!(observe.contains("PresentOutcome::Presented"));
    assert!(observe.contains("proof.window == window"));
    assert!(observe.contains("mark.pane == proof.pane"));
    assert!(observe.contains("mark_proves(mark, proof.marker_rows)"));
    // The grid reads live in the sample, taken while the guards are held.
    let sample = source.split_once("pub(super) fn marker_sample<").unwrap().1;
    let sample = sample.split_once("pub(super) fn observe_frame(").unwrap().0;
    assert!(sample.contains("mark_of(pane, grid, viewport_top_abs, marker)"));
    let mark = source.split_once("pub(in crate::app) fn mark_of(").unwrap().1;
    let mark = mark.split_once("\n}\n").unwrap().0;
    assert!(mark.contains("grid_marker_rows(grid, marker)"));
    assert!(mark.contains("visible_marker(grid, viewport_top_abs, marker)"));
    assert!(!observe.split_once("pub(super) fn observe_device_event").unwrap().0.contains("grid"));
    assert!(observe
        .contains("accepts_proof_generation(self.stage, self.original_generation, generation)"));
    assert!(observe.contains("proof.presented_generation = Some(generation)"));
}

/// The native oracle must consume the same marker-bearing plan before compatibility conversion discards the typed outcome.
#[test]
fn recovery_marker_proof_is_bound_to_each_present_callback() {
    // The marker facts are copied from the held guards before the call that releases them, and the
    // verdict is applied with the call's outcome before compatibility conversion. Windows CI checks
    // sources out with CRLF line ends, so the scan runs on a CRLF copy too.
    scan_marker_proof_in_both_line_ends([
        include_str!("window_event.rs"),
        include_str!("child_window_redraw.rs"),
        include_str!("gpu_recovery_smoke.rs"),
    ]);
}

/// Run the marker-proof scan over the checkout as given and over its CRLF form, built from the
/// LF-normalized text so an already-CRLF checkout never becomes `\r\r\n`.
fn scan_marker_proof_in_both_line_ends(sources: [&str; 3]) {
    check_recovery_marker_proof(sources[0], sources[1], sources[2]);
    let crlf = sources.map(|text| text.replace("\r\n", "\n").replace('\n', "\r\n"));
    check_recovery_marker_proof(&crlf[0], &crlf[1], &crlf[2]);
}

/// A Windows checkout hands include_str! CRLF text; the scan must still read it as LF.
#[test]
fn marker_proof_scan_accepts_a_checkout_that_is_already_crlf() {
    // Building the CRLF variant from CRLF text would yield `\r\r\n`, which one normalization
    // leaves as `\r\n`, so every LF delimiter lookup in the scan would miss.
    let crlf = [
        include_str!("window_event.rs"),
        include_str!("child_window_redraw.rs"),
        include_str!("gpu_recovery_smoke.rs"),
    ]
    .map(|text| text.replace("\r\n", "\n").replace('\n', "\r\n"));
    scan_marker_proof_in_both_line_ends([&crlf[0], &crlf[1], &crlf[2]]);
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
    assert!(source.contains("self.warm_window_pool_maintain(event_loop)"));
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
    let production = wait.find("self.service_gpu_recovery(event_loop, Instant::now())").unwrap();
    let warm = wait.find("self.warm_window_pool_maintain(event_loop)").unwrap();
    let smoke = wait.find("self.drive_gpu_recovery_smoke(event_loop, Instant::now())").unwrap();
    assert!(production < warm && warm < smoke);
    assert!(wait.contains("self.gpu_recovery_smoke_deadline()"));
}

/// Marker facts copied from an unchanged grid agree with reading the grid directly, and the verdict
/// is the drawn frame's: after the grid is rewritten its own facts no longer prove the marker, while
/// the facts copied for the drawn frame still do.
#[test]
fn a_marker_sample_agrees_with_the_grid_and_keeps_the_drawn_frames_verdict() {
    let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(20, 3));
    drop(parser.advance(b"marker one"));
    let drawn = mark_of(7, parser.grid(), None, "marker");
    let read = RecoveryMark {
        pane: 7,
        marker_rows: grid_marker_rows(parser.grid(), "marker"),
        visible: visible_marker(parser.grid(), None, "marker"),
    };
    assert_eq!(drawn, read);
    assert!(mark_proves(&drawn, 0));
    drop(parser.advance(b"\x1b[2J\x1b[3J\x1b[H"));
    assert!(!mark_proves(&mark_of(7, parser.grid(), None, "marker"), 0), "the rewrite hides it");
    assert!(mark_proves(&drawn, 0), "the drawn frame's verdict stands");
}
