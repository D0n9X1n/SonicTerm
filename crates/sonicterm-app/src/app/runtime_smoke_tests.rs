use super::*;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

#[test]
fn failure_codes_are_distinct_and_stable() {
    // Startup, presentation, GPU faults, and native teardown retain distinct failure boundaries.
    assert_eq!(RuntimeSmokeFailure::EventLoop.exit_code(), 10);
    assert_eq!(RuntimeSmokeFailure::Display.exit_code(), 11);
    assert_eq!(RuntimeSmokeFailure::Gpu.exit_code(), 12);
    assert_eq!(RuntimeSmokeFailure::Pty.exit_code(), 13);
    assert_eq!(RuntimeSmokeFailure::Marker.exit_code(), 14);
    assert_eq!(RuntimeSmokeFailure::Present.exit_code(), 15);
    assert_eq!(RuntimeSmokeFailure::WarmLifecycle.exit_code(), 16);
    assert_eq!(RuntimeSmokeFailure::GpuFaultContainment.exit_code(), 17);
    assert_eq!(RuntimeSmokeFailure::GpuDeviceLoss.exit_code(), 18);
    assert_eq!(RuntimeSmokeFailure::GpuDeviceRecovery.exit_code(), 19);
    assert_eq!(RuntimeSmokeFailure::NativeTeardown.exit_code(), 20);
}

#[test]
fn marker_command_cannot_pass_from_terminal_echo() {
    // Protect the round trip: the complete expected marker may exist only in shell output, never in typed input.
    let state = RuntimeSmokeState::new(42);
    let command = String::from_utf8(state.command().to_vec()).expect("ASCII smoke command");
    assert_eq!(state.marker(), "__SONICTERM_SMOKE_42__");
    assert!(!command.contains(state.marker()));
    assert!(command.contains("printf"));
    assert!(command.contains("%s"));
    assert!(command.contains("42"));
}

#[test]
fn marker_detection_reads_the_live_grid() {
    // Protect the production observation seam rather than accepting a PTY write or process launch as success.
    let mut parser = Parser::new(Grid::new(80, 4));
    parser.advance(b"prompt$ printf '__SONICTERM_SMOKE_%s__' '42'\r\n");
    assert!(!grid_contains_marker(parser.grid(), "__SONICTERM_SMOKE_42__"));
    parser.advance(b"__SONICTERM_SMOKE_42__\r\n");
    assert!(grid_contains_marker(parser.grid(), "__SONICTERM_SMOKE_42__"));
}

#[test]
fn success_requires_main_and_adopted_presentations_with_warm_release() {
    // Protect against treating parsed output or an unpresented adopted renderer as a complete smoke.
    let mut state = RuntimeSmokeState::new(7);
    state.begin_marker_wait();
    state.begin_present_wait(3);
    assert!(!state.observe_presented_frame(3));
    assert!(state.observe_presented_frame(4));
    assert!(state.should_maintain_warm_pool());
    assert_eq!(state.renderer_baseline(), sonicterm_gpu::core::live_renderer_count());

    // The dummy id remains an opaque identity; this test never resolves it as a native window.
    let child = winit::window::WindowId::dummy();
    assert!(state.begin_warm_adoption(child, 0));
    assert!(!state.observe_adopted_present(child, 0));
    assert!(state.observe_adopted_present(child, 1));
    assert!(!state.finish_warm_release(child, false));
    assert_eq!(state.outcome(), Some(Err(RuntimeSmokeFailure::WarmLifecycle)));

    let mut successful = RuntimeSmokeState::new(8);
    successful.begin_marker_wait();
    successful.begin_present_wait(0);
    assert!(successful.observe_presented_frame(1));
    assert!(successful.begin_warm_adoption(child, 0));
    assert!(successful.observe_adopted_present(child, 1));
    assert!(successful.finish_warm_release(child, true));
    assert_eq!(successful.outcome(), None);
    assert_eq!(successful.phase, RuntimeSmokePhase::FaultPrecondition);
}

#[test]
fn custom_drop_owner_requires_a_fresh_window_after_warm_release() {
    // A custom native drop target must survive both adoption and independent HWND creation before success.
    let mut state = RuntimeSmokeState::new(9);
    state.verify_fresh_drop_target = true;
    let warm = winit::window::WindowId::from(41);
    let fresh = winit::window::WindowId::from(42);
    state.begin_marker_wait();
    state.begin_present_wait(0);
    assert!(state.observe_presented_frame(1));
    assert!(state.begin_warm_adoption(warm, 3));
    assert!(state.observe_adopted_present(warm, 4));
    assert!(state.finish_warm_release(warm, true));
    assert_eq!(state.outcome(), None);
    assert!(state.needs_fresh_window());
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::WarmLifecycle);
    assert!(state.begin_fresh_window(fresh, 5));
    assert!(!state.is_waiting_for_adopted_present(warm));
    assert!(!state.observe_adopted_present(warm, 6));
    assert!(!state.observe_adopted_present(fresh, 5));
    assert!(state.observe_adopted_present(fresh, 6));
    assert!(state.finish_warm_release(fresh, true));
    assert_eq!(state.outcome(), None);
    assert_eq!(state.phase, RuntimeSmokePhase::FaultPrecondition);
}

#[test]
fn timeout_maps_to_the_boundary_currently_under_test() {
    // Protect actionable exit codes when the watchdog fires during each startup phase.
    let mut state = RuntimeSmokeState::new(1);
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Display);
    state.begin_gpu();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Gpu);
    state.begin_pty();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Pty);
    state.begin_marker_wait();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Marker);
    state.begin_present_wait(0);
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Present);
    assert!(state.observe_presented_frame(1));
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::WarmLifecycle);
}

/// The default scenario is opt-in only through runtime smoke; unknown values fail before winit.
#[test]
fn scenarios_reject_unknown_values() {
    assert_eq!(RuntimeSmokeScenario::parse(None), Ok(RuntimeSmokeScenario::Default));
    assert_eq!(RuntimeSmokeScenario::parse(Some("default")), Ok(RuntimeSmokeScenario::Default));
    assert_eq!(
        RuntimeSmokeScenario::parse(Some("frame-validation")),
        Ok(RuntimeSmokeScenario::FrameValidation)
    );
    assert_eq!(
        RuntimeSmokeScenario::parse(Some("device-recovery")),
        Ok(RuntimeSmokeScenario::DeviceRecovery)
    );
    for value in ["", "FRAME-VALIDATION", "DEVICE-RECOVERY", "retained", "other"] {
        assert_eq!(RuntimeSmokeScenario::parse(Some(value)), Err(RuntimeSmokeFailure::EventLoop));
    }
}

/// Recovery is an explicit scenario and never replaces the default stopped-device containment oracle.
#[test]
fn device_recovery_starts_only_after_initial_marker_presentation() {
    let mut state = RuntimeSmokeState::new(17);
    assert!(!state.recovery_enabled());
    state.scenario = RuntimeSmokeScenario::DeviceRecovery;
    assert!(state.recovery_enabled());
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Display);
    state.begin_marker_wait();
    state.begin_present_wait(4);
    assert!(!state.observe_presented_frame(4));
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Present);
    assert!(state.observe_presented_frame(5));
    assert_eq!(state.phase, RuntimeSmokePhase::RecoveryReady);
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::GpuDeviceRecovery);
    assert!(!state.should_maintain_warm_pool());
    assert!(!state.fault_pending());
}

#[derive(Clone, Default)]
struct TimeoutLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for TimeoutLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn capture_timeout(action: impl FnOnce()) -> String {
    let log = TimeoutLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(sonicterm_logging::DEFAULT_FILTER)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let bytes = log.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

/// The process watchdog keeps the actual startup failure boundary and identifies an unstarted recovery phase.
#[test]
fn recovery_watchdog_before_the_probe_reports_startup_boundary() {
    let mut state = RuntimeSmokeState::new(18);
    state.scenario = RuntimeSmokeScenario::DeviceRecovery;
    state.begin_gpu();
    let log = capture_timeout(|| state.fail_from_watchdog(Instant::now()));
    assert_eq!(state.outcome(), Some(Err(RuntimeSmokeFailure::Gpu)));
    assert!(log.contains("runtime smoke recovery startup timeout"));
    assert!(log.contains("cause=\"watchdog\""));
    assert!(log.contains("phase=Gpu"));
}

/// A watchdog delivered before the first recovery tick preserves the selected recovery boundary.
#[test]
fn recovery_watchdog_before_first_probe_tick_keeps_recovery_boundary() {
    let mut state = RuntimeSmokeState::new(19);
    state.scenario = RuntimeSmokeScenario::DeviceRecovery;
    state.begin_marker_wait();
    state.begin_present_wait(0);
    assert!(state.observe_presented_frame(1));
    let source = include_str!("gpu_recovery_smoke.rs");
    assert!(source.contains("probe.report_timeout(now, \"phase-deadline\")"));
    let log = capture_timeout(|| state.fail_from_watchdog(Instant::now()));
    assert_eq!(state.outcome(), Some(Err(RuntimeSmokeFailure::GpuDeviceRecovery)));
    assert!(log.contains("cause=\"watchdog\""));
    assert!(include_str!("event_loop.rs").contains("smoke.fail_from_watchdog(Instant::now())"));
}

/// Same-text shell commands must add a marker row, not merely rediscover a prior output row.
#[test]
fn repeated_marker_requires_fresh_grid_output() {
    let mut parser = Parser::new(Grid::new(80, 3));
    parser.advance(b"__SONICTERM_SMOKE_42__\r\n");
    assert_eq!(grid_marker_rows(parser.grid(), "__SONICTERM_SMOKE_42__"), 1);
    parser.advance(b"prompt$ printf '__SONICTERM_SMOKE_%s__' '42'\r\n");
    assert_eq!(grid_marker_rows(parser.grid(), "__SONICTERM_SMOKE_42__"), 1);
    parser.advance(b"__SONICTERM_SMOKE_42__\r\n");
    assert_eq!(grid_marker_rows(parser.grid(), "__SONICTERM_SMOKE_42__"), 2);
}

/// Freeze a real device-state identity and both counter families before applying the chosen synthetic fault.
fn fault_fixture(
    kind: GpuFaultKind,
) -> (RuntimeSmokeState, FaultBaseline, Instant, DeviceErrorSnapshot) {
    let mut smoke = RuntimeSmokeState::new(1);
    smoke.render_attempts = 2;
    let now = Instant::now();
    let snapshot = sonicterm_gpu::device_errors::DeviceErrorState::new().snapshot();
    let baseline = smoke.capture_fault_baseline(
        winit::window::WindowId::from(701),
        kind,
        &snapshot,
        SmokeFrameCounts { presents: 4, successful: 3 },
        1,
    );
    smoke.begin_fault(kind, baseline, now);
    (smoke, baseline, now, snapshot)
}

/// A isolated error must actually be recorded, remain usable, and permit a later acknowledged frame.
#[test]
fn isolated_fault_needs_a_record_and_later_frame() {
    let (mut smoke, baseline, now, mut snapshot) = fault_fixture(GpuFaultKind::IsolatedOperation);
    snapshot.counts.isolated = 1;
    smoke.observe_fault(&snapshot, baseline.frames, 1, now);
    assert!(matches!(smoke.phase, RuntimeSmokePhase::FaultIsolated { .. }));
    smoke.observe_fault(&snapshot, SmokeFrameCounts { presents: 5, successful: 4 }, 1, now);
    assert_eq!(smoke.phase, RuntimeSmokePhase::FaultRetainedReady);
    for state in [DeviceState::Unusable, DeviceState::Lost] {
        let (mut smoke, baseline, now, mut snapshot) =
            fault_fixture(GpuFaultKind::IsolatedOperation);
        snapshot.counts.isolated = 1;
        snapshot.state = state;
        smoke.observe_fault(&snapshot, baseline.frames, 1, now);
        assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuFaultContainment)));
    }
}

/// A marker emitted before the faulty render stops the device is included in
/// the new baseline, never accepted as proof of post-stop PTY liveness.
#[test]
fn frame_validation_rejects_pre_stop_marker_output() {
    let (mut smoke, baseline, armed, mut snapshot) = fault_fixture(GpuFaultKind::FrameValidation);
    smoke.observe_fault(&snapshot, baseline.frames, 2, armed + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), None, "the fault is only armed");
    let stopped = armed + Duration::from_secs(1);
    smoke.note_render_attempt();
    snapshot.state = DeviceState::Unusable;
    smoke.observe_fault(&snapshot, baseline.frames, 2, stopped);
    assert!(smoke.frame_marker_pending(), "confirmed stop must request a new marker command");
    assert_eq!(smoke.outcome(), None);
    smoke.observe_fault(&snapshot, baseline.frames, 2, stopped + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), None, "no pass before the post-stop command is queued");
    let queued = stopped + FAULT_OBSERVATION;
    smoke.begin_frame_marker_wait(queued);
    smoke.observe_fault(&snapshot, baseline.frames, 2, queued + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), None, "the pre-stop marker is now baseline, not fresh output");
    smoke.observe_fault(&snapshot, baseline.frames, 3, queued + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), Some(Ok(())));
}

/// Delaying the first faulty render beyond 250 ms cannot consume any of the
/// quiet interval, which starts only after the confirmed stop and marker resend.
#[test]
fn delayed_faulty_render_cannot_consume_post_stop_quiet_interval() {
    let (mut smoke, baseline, armed, mut snapshot) = fault_fixture(GpuFaultKind::FrameValidation);
    let stopped = armed + Duration::from_secs(2);
    smoke.observe_fault(&snapshot, baseline.frames, 2, stopped);
    assert_eq!(smoke.outcome(), None);
    smoke.note_render_attempt();
    snapshot.state = DeviceState::Unusable;
    smoke.observe_fault(&snapshot, baseline.frames, 2, stopped);
    assert!(smoke.frame_marker_pending());
    smoke.begin_frame_marker_wait(stopped);
    smoke.observe_fault(&snapshot, baseline.frames, 3, stopped);
    assert_eq!(smoke.outcome(), None, "time while merely armed is not quiet time");
    smoke.observe_fault(
        &snapshot,
        baseline.frames,
        3,
        stopped + FAULT_OBSERVATION - Duration::from_millis(1),
    );
    assert_eq!(smoke.outcome(), None);
    smoke.observe_fault(&snapshot, baseline.frames, 3, stopped + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), Some(Ok(())));
}

/// Starting post-stop proof never resets the overall five-second arming deadline.
#[test]
fn frame_validation_deadline_stays_anchored_to_arming() {
    let (mut smoke, baseline, armed, mut snapshot) = fault_fixture(GpuFaultKind::FrameValidation);
    let stopped = armed + FAULT_DEADLINE - Duration::from_millis(100);
    smoke.note_render_attempt();
    snapshot.state = DeviceState::Unusable;
    smoke.observe_fault(&snapshot, baseline.frames, 2, stopped);
    assert!(smoke.frame_marker_pending());
    smoke.begin_frame_marker_wait(stopped);
    assert_eq!(smoke.fault_started, Some(armed));
    smoke.observe_fault(&snapshot, baseline.frames, 3, stopped + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuFaultContainment)));
}

/// Both raw present and successful-frame totals remain frozen before and
/// after confirming the stopped device; a reset baseline must not hide a present.
#[test]
fn post_stop_marker_wait_keeps_original_frame_counters() {
    for changed in [
        SmokeFrameCounts { presents: 5, successful: 3 },
        SmokeFrameCounts { presents: 4, successful: 4 },
    ] {
        let (mut smoke, baseline, armed, mut snapshot) =
            fault_fixture(GpuFaultKind::FrameValidation);
        smoke.note_render_attempt();
        snapshot.state = DeviceState::Unusable;
        smoke.observe_fault(&snapshot, baseline.frames, 2, armed);
        assert!(smoke.frame_marker_pending());
        smoke.begin_frame_marker_wait(armed);
        smoke.observe_fault(&snapshot, changed, 3, armed + FAULT_OBSERVATION);
        assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuFaultContainment)));
    }
}

/// Production only queues the frame scenario's marker after observing the
/// stopped device, and starts quiet timing only after the queue accepts it.
#[test]
fn frame_marker_resend_follows_confirmed_stop_in_production() {
    let source = include_str!("runtime_smoke.rs");
    let inject = source.find("renderer.__inject_gpu_fault(kind)").unwrap();
    let observe = source[inject..].find("smoke.observe_fault(&snapshot").unwrap() + inject;
    assert!(source[inject..observe].contains(
        "!matches!(kind, GpuFaultKind::IsolatedOperation | GpuFaultKind::FrameValidation)"
    ));
    let pending = source[observe..].find("if smoke.frame_marker_pending()").unwrap() + observe;
    let resend =
        source[pending..].find("self.queue_gpu_fault_smoke_marker(smoke, failure)?").unwrap()
            + pending;
    let quiet =
        source[resend..].find("smoke.begin_frame_marker_wait(Instant::now())").unwrap() + resend;
    assert!(observe < pending && pending < resend && resend < quiet);
}

/// Neither a present without acknowledgement nor an acknowledged frame may escape containment.
#[test]
fn fault_checks_reject_both_present_counter_kinds() {
    for kind in [GpuFaultKind::RetainedResourceCreation, GpuFaultKind::FrameValidation] {
        for changed in [
            SmokeFrameCounts { presents: 5, successful: 3 },
            SmokeFrameCounts { presents: 4, successful: 4 },
        ] {
            let (mut smoke, _, now, mut snapshot) = fault_fixture(kind);
            smoke.note_render_attempt();
            snapshot.state = DeviceState::Unusable;
            smoke.observe_fault(&snapshot, changed, 2, now + FAULT_OBSERVATION);
            assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuFaultContainment)));
        }
    }
}

/// Missing injection, stalled shell output, and missing loss records fail at their own boundaries.
#[test]
fn fault_deadlines_and_loss_records_fail_closed() {
    for kind in [
        GpuFaultKind::IsolatedOperation,
        GpuFaultKind::RetainedResourceCreation,
        GpuFaultKind::FrameValidation,
        GpuFaultKind::DestroyDevice,
    ] {
        let (mut smoke, baseline, now, snapshot) = fault_fixture(kind);
        let expected = if kind == GpuFaultKind::DestroyDevice {
            RuntimeSmokeFailure::GpuDeviceLoss
        } else {
            RuntimeSmokeFailure::GpuFaultContainment
        };
        assert_eq!(smoke.timeout_failure(), expected);
        smoke.observe_fault(&snapshot, baseline.frames, 1, now + FAULT_DEADLINE);
        assert_eq!(smoke.outcome(), Some(Err(expected)));
    }
    let (mut smoke, baseline, now, mut snapshot) = fault_fixture(GpuFaultKind::DestroyDevice);
    smoke.note_render_attempt();
    snapshot.state = DeviceState::Lost;
    smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuDeviceLoss)));
}

/// Warm completion is not success: retained proof must advance to loss and require a second marker.
#[test]
fn retained_then_destroy_requires_two_fresh_markers() {
    let (mut smoke, baseline, now, mut snapshot) =
        fault_fixture(GpuFaultKind::RetainedResourceCreation);
    let window = baseline.refusal.identity.window;
    snapshot.state = DeviceState::Unusable;
    smoke.note_stopped_redraw_refusal(window, &snapshot);
    let first = smoke.stopped_redraw.unwrap().count;
    smoke.note_stopped_redraw_refusal(window, &snapshot);
    assert_eq!(
        smoke.stopped_redraw.unwrap().count,
        first + 1,
        "silent later refusals are still observations"
    );
    smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION);
    assert_eq!(smoke.next_fault(), Some(GpuFaultKind::DestroyDevice));
    let next = smoke.capture_fault_baseline(
        window,
        GpuFaultKind::DestroyDevice,
        &snapshot,
        baseline.frames,
        2,
    );
    assert!(next.refusal.count > baseline.refusal.count);
    assert_eq!(next.refusal.identity.generation, baseline.refusal.identity.generation);
    let destroyed = now + FAULT_OBSERVATION;
    smoke.begin_fault(GpuFaultKind::DestroyDevice, next, destroyed);
    snapshot.state = DeviceState::Lost;
    snapshot.destroy_requested = true;
    snapshot.lost = Some(sonicterm_gpu::device_errors::DeviceErrorRecord {
        generation: snapshot.generation,
        kind: sonicterm_gpu::device_errors::DeviceErrorKind::Lost,
        operation: "fault.destroy",
        description: "Destroyed".to_owned(),
    });
    smoke.observe_fault(&snapshot, next.frames, 3, destroyed + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), None, "the retained refusal is not fresh destroy evidence");
    smoke.note_stopped_redraw_refusal(window, &snapshot);
    smoke.observe_fault(&snapshot, next.frames, 2, destroyed + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), None, "destroy still needs a second fresh marker");
    smoke.observe_fault(&snapshot, next.frames, 3, destroyed + FAULT_OBSERVATION);
    assert_eq!(smoke.outcome(), Some(Ok(())));
    assert_eq!(smoke.render_attempts, baseline.render_attempts, "refusals are not renderer calls");
}

/// The separate frame-validation process injects after main presentation, before warm teardown.
#[test]
fn frame_scenario_skips_warm_phases_and_uses_real_render_attempts() {
    let mut smoke = RuntimeSmokeState::new(1);
    smoke.scenario = RuntimeSmokeScenario::FrameValidation;
    smoke.begin_marker_wait();
    smoke.begin_present_wait(0);
    assert!(smoke.observe_presented_frame(1));
    assert_eq!(smoke.next_fault(), Some(GpuFaultKind::FrameValidation));
    assert!(!smoke.should_maintain_warm_pool());
    let main = include_str!("window_event.rs");
    let begin = main.find("let outcome = r.render_with_outcome(").unwrap();
    assert!(
        main[begin..].find("smoke.note_render_attempt()").unwrap()
            < main[begin..].find("t.lap(\"render\")").unwrap()
    );
    let idle = include_str!("event_loop.rs");
    assert!(idle.contains("self.new_tab(\"runtime smoke warm child\")"));
    assert!(idle.contains("self.drive_gpu_fault_smoke(Instant::now())"));
    let shell = include_str!("../shell.rs");
    let smoke = &shell[shell.find("fn run_smoke(").unwrap()..];
    let selection = smoke.find("spec.selected_scenario()").unwrap();
    let refusal = smoke.find("return ShellRunResult::smoke(Err(failure), true)").unwrap();
    let event_loop = smoke.find("EventLoop::<UserEvent>").unwrap();
    assert!(selection < refusal && refusal < event_loop);
    assert!(event_loop < smoke.find("self.into_app(").unwrap());
}

/// A platform-pinned scenario is not parsed a second time from mutable process environment.
#[test]
fn pinned_scenario_survives_environment_selection() {
    let spec = RuntimeSmokeSpec::new(
        "/bin/sh",
        "marker",
        b"echo mar'ker'\n".to_vec(),
        PathBuf::from("config"),
        PathBuf::from("logs"),
    )
    .unwrap()
    .with_scenario(RuntimeSmokeScenario::FrameValidation);
    assert_eq!(spec.selected_scenario(), Ok(RuntimeSmokeScenario::FrameValidation));
}

/// Installing a scenario alone never enables fault work in a normal App or shifts an armed probe deadline.
#[test]
fn fault_polling_is_default_disabled_and_uses_a_fixed_deadline() {
    let mut app = super::super::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    assert!(app.gpu_fault_smoke_deadline().is_none());
    assert!(!app.drive_gpu_fault_smoke(Instant::now()));
    let (mut smoke, _, now, _) = fault_fixture(GpuFaultKind::FrameValidation);
    let due = now + FAULT_POLL_INTERVAL;
    smoke.next_probe_at = Some(due);
    app.runtime_smoke = Some(smoke);
    assert_eq!(app.gpu_fault_smoke_deadline(), Some(due));
    assert!(!app.drive_gpu_fault_smoke(now));
    assert_eq!(app.gpu_fault_smoke_deadline(), Some(due));
}

/// Both typed redraw calls retain one B9 attempt observation after compatibility
/// classification and before render timing, including failed or suspended frames.
#[test]
fn typed_redraws_preserve_both_smoke_attempt_observations() {
    for source in [include_str!("window_event.rs"), include_str!("child_window.rs")] {
        let call = source.find("let outcome = r.render_with_outcome(").unwrap();
        let result = source[call..].find("outcome.into_render_result()").unwrap() + call;
        let observed = source[call..].find("smoke.note_render_attempt()").unwrap() + call;
        let timing = source[call..].find("t.lap(\"render\")").unwrap() + call;
        assert!(call < result && result < observed && observed < timing);
        assert_eq!(source[call..timing].matches("smoke.note_render_attempt()").count(), 1);
    }
}

/// Build matching retained/lost device evidence while preserving the baseline's exact generation.
fn stopped_fault_snapshot(kind: GpuFaultKind, snapshot: &mut DeviceErrorSnapshot) {
    snapshot.state =
        if kind == GpuFaultKind::DestroyDevice { DeviceState::Lost } else { DeviceState::Unusable };
    snapshot.destroy_requested = kind == GpuFaultKind::DestroyDevice;
    if snapshot.destroy_requested {
        snapshot.lost = Some(sonicterm_gpu::device_errors::DeviceErrorRecord {
            generation: snapshot.generation,
            kind: sonicterm_gpu::device_errors::DeviceErrorKind::Lost,
            operation: "fault.destroy",
            description: "Destroyed".to_owned(),
        });
    }
}

/// Only a fresh refusal on the faulted main, generation, state, and destroy flag can establish retained/loss proof.
#[test]
fn retained_and_destroy_refusal_identity_and_count_fail_closed_at_deadline() {
    for kind in [GpuFaultKind::RetainedResourceCreation, GpuFaultKind::DestroyDevice] {
        for defect in ["window", "generation", "state", "destroy", "stale", "absent"] {
            let (mut smoke, baseline, now, mut snapshot) = fault_fixture(kind);
            stopped_fault_snapshot(kind, &mut snapshot);
            let mut observed = snapshot.clone();
            let mut window = baseline.refusal.identity.window;
            match defect {
                "window" => window = winit::window::WindowId::from(702),
                "generation" => observed.generation += 1,
                "state" => {
                    observed.state = if snapshot.state == DeviceState::Lost {
                        DeviceState::Unusable
                    } else {
                        DeviceState::Lost
                    }
                }
                "destroy" => observed.destroy_requested = !observed.destroy_requested,
                "stale" => smoke.stopped_redraw = Some(baseline.refusal),
                "absent" => {}
                _ => unreachable!(),
            }
            if defect != "stale" && defect != "absent" {
                smoke.note_stopped_redraw_refusal(window, &observed);
            }
            smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION);
            assert_eq!(smoke.outcome(), None, "{kind:?} accepted {defect} evidence");
            assert_eq!(smoke.render_attempts, baseline.render_attempts);
            smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_DEADLINE);
            let expected = if kind == GpuFaultKind::DestroyDevice {
                RuntimeSmokeFailure::GpuDeviceLoss
            } else {
                RuntimeSmokeFailure::GpuFaultContainment
            };
            assert_eq!(smoke.outcome(), Some(Err(expected)), "{kind:?} {defect}");
        }
    }
}

/// Matching refusal proof does not replace fresh marker, frozen frame totals, loss records, or quiet time.
#[test]
fn fresh_stopped_refusal_preserves_every_other_containment_boundary() {
    for kind in [GpuFaultKind::RetainedResourceCreation, GpuFaultKind::DestroyDevice] {
        let (mut smoke, baseline, now, mut snapshot) = fault_fixture(kind);
        stopped_fault_snapshot(kind, &mut snapshot);
        smoke.note_stopped_redraw_refusal(baseline.refusal.identity.window, &snapshot);
        smoke.observe_fault(
            &snapshot,
            baseline.frames,
            2,
            now + FAULT_OBSERVATION - Duration::from_nanos(1),
        );
        assert_eq!(smoke.outcome(), None, "quiet interval stays required");
        smoke.observe_fault(&snapshot, baseline.frames, 1, now + FAULT_OBSERVATION);
        assert_eq!(smoke.outcome(), None, "a previous marker is not fresh output");
        smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION);
        if kind == GpuFaultKind::DestroyDevice {
            assert_eq!(smoke.outcome(), Some(Ok(())));
        } else {
            assert_eq!(smoke.next_fault(), Some(GpuFaultKind::DestroyDevice));
        }
        assert_eq!(smoke.render_attempts, baseline.render_attempts);

        for changed in [
            SmokeFrameCounts { presents: 5, successful: 3 },
            SmokeFrameCounts { presents: 4, successful: 4 },
        ] {
            let (mut smoke, baseline, now, mut snapshot) = fault_fixture(kind);
            stopped_fault_snapshot(kind, &mut snapshot);
            smoke.note_stopped_redraw_refusal(baseline.refusal.identity.window, &snapshot);
            smoke.observe_fault(&snapshot, changed, 2, now + FAULT_OBSERVATION);
            assert!(
                matches!(smoke.outcome(), Some(Err(_))),
                "a fresh refusal cannot excuse a present"
            );
        }
    }
    let (mut smoke, baseline, now, mut snapshot) = fault_fixture(GpuFaultKind::DestroyDevice);
    snapshot.state = DeviceState::Lost;
    snapshot.destroy_requested = true;
    smoke.note_stopped_redraw_refusal(baseline.refusal.identity.window, &snapshot);
    smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION);
    assert_eq!(
        smoke.outcome(),
        Some(Err(RuntimeSmokeFailure::GpuDeviceLoss)),
        "loss record stays required"
    );
}

/// Even a correctly identified stopped refusal cannot stand in for FrameValidation's real faulty render call.
#[test]
fn frame_validation_refusals_alone_cannot_exercise_the_armed_frame() {
    let (mut smoke, baseline, now, mut snapshot) = fault_fixture(GpuFaultKind::FrameValidation);
    snapshot.state = DeviceState::Unusable;
    for tick in 1..=3 {
        smoke.note_stopped_redraw_refusal(baseline.refusal.identity.window, &snapshot);
        smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_OBSERVATION * tick);
        assert!(!smoke.frame_marker_pending());
        assert_eq!(smoke.outcome(), None);
    }
    assert_eq!(smoke.render_attempts, baseline.render_attempts);
    smoke.observe_fault(&snapshot, baseline.frames, 2, now + FAULT_DEADLINE);
    assert_eq!(smoke.outcome(), Some(Err(RuntimeSmokeFailure::GpuFaultContainment)));
}

/// Refusal observation precedes one-time report extraction and every parked/hidden return, without pretending to render.
#[test]
fn production_stopped_redraw_records_first_and_silent_refusals_separately() {
    let source = include_str!("redraw.rs");
    let start = source.find("pub(super) fn begin_window_redraw(").unwrap();
    let end = source[start..].find("pub(super) fn snapshot_window_redraw(").unwrap() + start;
    let entry = &source[start..end];
    let refuse = entry.find("!renderer.device_accepts_gpu_work()").unwrap();
    let record = entry
        .find("smoke.note_stopped_redraw_refusal(id, &renderer.device_error_snapshot())")
        .unwrap();
    let report = entry.find("renderer.take_stopped_render_outcome()").unwrap();
    let parked = entry.find("!window.frame_deadlines_allowed()").unwrap();
    assert!(refuse < record && record < report && report < parked);
    assert!(!entry.contains("note_render_attempt"));
    assert!(!entry.contains("request_redraw("));
    let smoke = include_str!("runtime_smoke.rs");
    let capture = smoke.find("let baseline = smoke.capture_fault_baseline(").unwrap();
    let arm = smoke[capture..].find("smoke.begin_fault(kind, baseline, now)").unwrap() + capture;
    let inject = smoke[arm..].find("renderer.__inject_gpu_fault(kind)").unwrap() + arm;
    assert!(capture < arm && arm < inject);
}
