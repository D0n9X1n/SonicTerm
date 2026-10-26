//! Synthetic driver and observation phase boundaries.

use super::super::probe_tests::*;
use super::*;

#[test]
fn startup_records_the_presenter_on_every_host() {
    // macOS results need the presenter to show whether a run stayed on the hardware path.
    let source = concat!(
        include_str!("probe.rs"),
        "\n",
        include_str!("probe_api.rs"),
        "\n",
        include_str!("probe_atlas.rs"),
        "\n",
        include_str!("probe_bootstrap.rs"),
        "\n",
        include_str!("probe_callbacks.rs"),
        "\n",
        include_str!("probe_checkpoint.rs"),
        "\n",
        include_str!("probe_input.rs"),
        "\n",
        include_str!("probe_occlusion.rs"),
        "\n",
        include_str!("probe_phases.rs"),
        "\n",
        include_str!("probe_sampling.rs"),
        "\n",
        include_str!("probe_scan.rs")
    )
    .replace("\r\n", "\n");
    let start = source.find("fn end_startup(").expect("end_startup exists");
    let body = &source[start..start + source[start..].find("\n    }\n").unwrap()];
    assert!(body.contains("self.presenter = self.presenter_record();"), "{body}");
    assert!(!body.contains("cfg!(windows)"), "no host gate: {body}");
    let start = source.find("fn presenter_record(&self)").expect("presenter_record exists");
    let body = &source[start..start + source[start..].find("\n    }\n").unwrap()];
    assert!(body.contains("presenter_record_for("), "{body}");
}

#[test]
fn atlas_readings_join_the_latest_record_of_their_checkpoint_in_attempt_order() {
    // Each sampling attempt's atlas reading belongs to the checkpoint that attempt sampled, and only to
    // its latest record, so perf-compare can pair a memory sample with the reading of the same attempt.
    let reading = |attempt| AtlasReading {
        attempt,
        main_window: None,
        counted_glyph_atlas_growths: None,
        closed_glyph_atlas_growths: None,
    };
    let mut records = vec![
        CheckpointRecord { index: 0, label: "settled", ..CheckpointRecord::default() },
        CheckpointRecord { index: 1, label: "end", ..CheckpointRecord::default() },
    ];
    attach_atlas_readings(&mut records, vec![(1, reading(1)), (1, reading(2))]);
    assert!(records[0].atlas_readings.is_empty());
    let attempts: Vec<u32> = records[1].atlas_readings.iter().map(|found| found.attempt).collect();
    assert_eq!(attempts, [1, 2]);
}

/// A phase meter hands the update count it started with to the record it finishes.
#[test]
fn a_phase_meter_records_the_update_count_it_started_with() {
    assert_eq!(
        PhaseMeter::start(
            "stream",
            crate::scenarios::PhaseKind::Sustained,
            None,
            false,
            None,
            Some(300)
        )
        .finish(None, crate::transition::Completion::default())
        .updates,
        Some(300)
    );
    assert_eq!(
        PhaseMeter::start("idle", crate::scenarios::PhaseKind::Sustained, None, false, None, None)
            .finish(None, crate::transition::Completion::default())
            .updates,
        None
    );
}

/// Closing a sample that no frame was credited with takes its armed watch, so the worker stops
/// paying for it: a later take of the same token finds it already taken.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn closing_an_unattributed_sample_disarms_its_watch() {
    use sonicterm_app::app::TakeOutcome;
    let (mut probe, pane) = typing_probe("S2", "default");
    assert_eq!(probe.open_typing_sample(Instant::now), Some('a'));
    let ArmState::Armed(token) = probe.open_sample.as_ref().expect("open").arm else {
        panic!("precondition: S2/default arms its sample");
    };
    probe.close_sample(UnattributedReason::NoCandidate);
    assert_eq!(probe.app.take_echo_watch(pane, token), TakeOutcome::AlreadyTaken);
    assert_eq!(probe.samples.last().map(|sample| sample.split_reason), Some("not-credited"));
}

/// With the feature, S2/default and S2/flood both arm their samples, so both sides of a comparison pay
/// the same watch cost; no other scenario is in scope.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn open_typing_sample_arms_for_s2_default_and_flood() {
    for variant in ["default", "flood"] {
        let (mut probe, _) = typing_probe("S2", variant);
        probe.open_typing_sample(Instant::now);
        let arm = probe.open_sample.as_ref().expect("open").arm;
        assert!(matches!(arm, ArmState::Armed(_)), "{variant}");
    }
    assert!(!crate::record::split_in_scope("S3", "default"));
    assert!(!crate::record::split_in_scope("S1", "flood"));
}

/// S2/flood still credits its samples exactly as before and is now in the timeline's scope: without
/// the feature its split reads `unsupported` and its timeline `unavailable` (`cfg-off`); with it the
/// sample is armed and carries a timeline.
#[test]
fn s2_flood_attributes_as_before_and_carries_a_timeline() {
    let (mut probe, pane) = typing_probe("S2", "flood");
    assert_eq!(probe.open_typing_sample(Instant::now), Some('a'));
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"a");
    credit_now(&mut probe);
    let sample = probe.samples.last().expect("a credited sample");
    assert_eq!(sample.reason, crate::record::CREDITED);
    assert!(sample.latency_ms.is_some());
    let timeline = sample.echo_timeline.expect("an in-scope credited sample has a timeline");
    if cfg!(feature = "perf-echo-trace") {
        assert_ne!(sample.split_reason, "unsupported", "the sample was armed");
    } else {
        // When: the build has no echo trace, nothing is armed and the timeline is cfg-off.
        assert_eq!((sample.split, sample.split_reason), (None, "unsupported"));
        assert_eq!(timeline.reason, Some("cfg-off"));
    }
}

/// A resize on the event loop after the echo appeared, which the worker never sees, changes the
/// grid's size generation; the harness's own snapshots catch it and the split is refused.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn a_harness_snapshot_identity_change_reads_row_identity_changed() {
    let (mut probe, pane) = typing_probe("S2", "default");
    probe.open_typing_sample(Instant::now);
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"a");
    let generation = |probe: &Probe| {
        let panes = probe.app.main_panes().expect("main");
        panes[&pane].parser.lock().grid().size_generation()
    };
    let before = generation(&probe);
    // 80 x 30 cells: more rows, the same columns, so row 0 keeps the echo.
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 600.0);
    assert!(probe.app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
    probe.app.__test_resize_visible_panes();
    assert_ne!(generation(&probe), before, "precondition: the resize changed the grid's size");
    credit_now(&mut probe);
    let sample = probe.samples.last().expect("a credited sample");
    assert_eq!(sample.reason, crate::record::CREDITED, "the attribution itself is unchanged");
    assert_eq!(sample.split_reason, "row-identity-changed");
}

/// Without the feature the adapter reports `Unsupported` itself, so the run is an untrimmed
/// baseline; with it the App's real rules apply, and an uncovered window is skipped with no trim
/// number.
#[test]
fn the_trim_adapter_reports_what_this_build_can_do() {
    let (mut probe, _pane) = typing_probe("S12", "default");
    let main = sonicterm_app::app::synthetic_main_window_id();
    let expected = if cfg!(feature = "perf-hook-trim") {
        TrimHookOutcome::Skipped
    } else {
        TrimHookOutcome::Unsupported
    };
    assert_eq!(trim_covered(&mut probe.app, main), (expected, None));
}

/// A requested trim waits while the App is not covered, lapses as `not-reached` when the hold
/// ends first, and is called exactly once on the first turn the App holds `Occluded(true)`.
#[test]
fn a_requested_trim_is_called_once_after_the_cover_is_delivered() {
    let (mut probe, _pane) = typing_probe("S12", "default");
    probe.main_id = Some(sonicterm_app::app::synthetic_main_window_id());

    probe.trim_pending = true;
    probe.service_trim(false);
    assert!(probe.trim_pending, "no occlusion delivered yet, so the trim waits");
    probe.service_trim(true);
    assert_eq!((probe.trim_pending, probe.trim_hook), (false, TrimHookOutcome::NotReached));

    probe.trim_pending = true;
    probe.occlusion.delivered = Some(true);
    probe.service_trim(false);
    assert!(!probe.trim_pending, "the hook was called");
    assert_ne!(probe.trim_hook, TrimHookOutcome::NotReached);
    let called = probe.trim_hook;
    probe.service_trim(true);
    assert_eq!(probe.trim_hook, called, "a second turn calls nothing");
}

/// The cover act only opens the cover; the trim act asks for the trim, the turn handler services
/// it after any overdue occlusion is delivered, and the end of a phase lapses one still waiting.
/// The trim step never touches the plan's stage.
#[test]
fn the_trim_act_is_serviced_after_delivery_and_lapses_with_its_phase() {
    let perform = method("perform");
    assert!(perform.contains("Act::Cover => self.cover(event_loop),"), "{perform}");
    assert!(perform.contains("Act::TrimCovered => self.request_trim(event_loop),"));
    let turn = method("about_to_wait_turn");
    let overdue = turn.find("self.deliver_overdue_occlusion(").expect("overdue delivery");
    let service = turn.find("self.service_trim(false);").expect("trim service");
    assert!(overdue < service, "the trim sees an occlusion delivered on the same turn");
    assert!(method("end_phase").contains("self.service_trim(true);"));
    assert!(!method("service_trim").contains("stage"), "the trim never changes the plan's steps");
}

/// Only a main-window redraw feeds a phase's presentation trace: a native dispatch that presents still
/// counts as a presented frame, but neither starts the trace nor counts as a nonpresenting redraw.
#[test]
fn only_main_window_redraws_feed_the_presentation_trace() {
    let mut meter =
        PhaseMeter::start("open", crate::scenarios::PhaseKind::Transition, None, false, None, None);
    let start = meter.started;
    let at = |offset_ms: u64| start + std::time::Duration::from_millis(offset_ms);
    meter.observe_dispatch(Dispatch::Native, at(1), at(2), (0, 1), None);
    meter.observe_dispatch(Dispatch::Harness, at(2), at(3), (1, 1), None);
    meter.observe_dispatch(Dispatch::Redraw, at(4), at(5), (1, 1), None);
    meter.observe_dispatch(Dispatch::Redraw, at(6), at(8), (1, 2), None);
    meter.observe_dispatch(Dispatch::Redraw, at(9), at(10), (2, 2), None);
    meter.observe_dispatch(Dispatch::Redraw, at(11), at(12), (2, 4), None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!((record.presented_frames, record.redraw_requested), (4, 4));
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(8.0), Some(12.0)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(2), Some(4)));
    assert_eq!((record.present_missing, record.nonpresenting_redraws), (None, 2));
}

/// A phase whose redraws never presented records `no-presentation` and every redraw as nonpresenting.
#[test]
fn a_phase_without_a_presenting_redraw_records_no_presentation() {
    let mut meter =
        PhaseMeter::start("hold", crate::scenarios::PhaseKind::Hold, None, false, None, None);
    let start = meter.started;
    meter.observe_dispatch(Dispatch::Redraw, start, start, (3, 3), None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(record.present_missing, Some("no-presentation"));
    assert_eq!((record.first_present_ms, record.first_present_seq), (None, None));
    assert_eq!(record.nonpresenting_redraws, 1);
    let written = serde_json::to_value(&record).expect("a phase record serializes");
    assert!(written.get("first_present_ms").is_none(), "{written}");
    assert_eq!(written["present_missing"], "no-presentation");
}

/// `forward` feeds the meter from its own readings around the dispatch: the dispatch kind, its span
/// and the presented counts before and after it.
#[test]
fn forward_feeds_the_meter_from_its_dispatch_readings() {
    let forward = method("forward");
    // Compare without whitespace, so rustfmt's line wrapping does not decide the result.
    let compact: String = forward.chars().filter(|glyph| !glyph.is_whitespace()).collect();
    let adapter = compact.find("dispatch(&mutself.app,event_loop);").expect("forward dispatches");
    let fed = compact
        .find("meter.observe_dispatch(kind,started,ended,(frames_before,frames_after),allocations")
        .expect("forward feeds the meter");
    assert!(adapter < fed);
}

/// The real `forward` feeds its meter: a main-window redraw that presents nothing counts as a redraw and a
/// nonpresenting redraw, so the phase records `no-presentation`. The loop is generic, so a unit stands in for it.
#[test]
fn a_forwarded_redraw_reaches_the_phases_presentation_trace() {
    let act = Instant::now();
    let (mut probe, _phase) = release_probe("media-free", act, 7);
    let redraw = Invocation::of(InvocationKind::WindowEvent { redraw: true }, None, false);
    probe.forward(&(), Dispatch::Redraw, redraw, |_app, _loop| {});
    let record = probe
        .meter
        .take()
        .expect("the phase is open")
        .finish(None, crate::transition::Completion::default());
    assert_eq!((record.redraw_requested, record.nonpresenting_redraws), (1, 1));
    assert_eq!(record.present_missing, Some("no-presentation"));
}

/// A transition that presents exactly one frame records that frame as both its first and last presentation.
#[test]
fn a_one_frame_transition_records_one_presentation_as_first_and_last() {
    let mut meter = PhaseMeter::start(
        "image",
        crate::scenarios::PhaseKind::Transition,
        Some(crate::scenarios::Endpoint::ImageRegisteredThenPresented),
        false,
        None,
        None,
    );
    let start = meter.started;
    let at = |offset_ms: u64| start + std::time::Duration::from_millis(offset_ms);
    meter.observe_dispatch(Dispatch::Redraw, at(30), at(40), (5, 6), None);
    let completion = crate::transition::Completion { elapsed_ms: Some(40.0), missing: None };
    let record = meter.finish(None, completion);
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(40.0), Some(40.0)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(6), Some(6)));
    assert_eq!((record.presented_frames, record.nonpresenting_redraws), (1, 0));
    assert_eq!(record.completion_ms, Some(40.0));
}

/// A sentinel transition completes on its parsed sentinel even when no redraw presented: completion and
/// `no-presentation` are recorded together, so completion never implies a presented frame.
#[test]
fn a_sentinel_completion_without_a_presentation_records_both() {
    let mut meter = PhaseMeter::start(
        "print",
        crate::scenarios::PhaseKind::Transition,
        Some(crate::scenarios::Endpoint::SentinelParsed),
        false,
        None,
        None,
    );
    let start = meter.started;
    meter.observe_dispatch(Dispatch::Native, start, start, (2, 2), None);
    let completion = crate::transition::Completion { elapsed_ms: Some(12.5), missing: None };
    let record = meter.finish(None, completion);
    assert_eq!((record.completion_ms, record.completion_missing), (Some(12.5), None));
    assert_eq!((record.present_missing, record.presented_frames), (Some("no-presentation"), 0));
}

/// A hold with no dispatch at all records no presentation and no nonpresenting redraw.
#[test]
fn a_no_draw_hold_records_no_presentation_and_no_redraws() {
    let meter =
        PhaseMeter::start("idle", crate::scenarios::PhaseKind::Hold, None, false, None, None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(
        (record.present_missing, record.redraw_requested, record.nonpresenting_redraws),
        (Some("no-presentation"), 0, 0)
    );
    assert_eq!((record.completion_ms, record.completion_missing), (None, None));
}

/// The presentation trace is kept directly, not derived from the bounded slow-dispatch list: with more redraws
/// than SLOW_DISPATCH_LIMIT, the first and last presentations are still the phase's first and last.
#[test]
fn the_trace_outlives_the_slow_dispatch_limit() {
    let mut meter = PhaseMeter::start(
        "stream",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        None,
        None,
    );
    let start = meter.started;
    let redraw_count = crate::record::SLOW_DISPATCH_LIMIT as u64 * 3;
    for index in 0..redraw_count {
        let ended = start + std::time::Duration::from_millis(index * 10 + 5);
        meter.observe_dispatch(Dispatch::Redraw, ended, ended, (index, index + 1), None);
    }
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(record.slow_dispatches.len(), crate::record::SLOW_DISPATCH_LIMIT);
    assert_eq!(record.dispatch_count, redraw_count);
    let last_ms = ((redraw_count - 1) * 10 + 5) as f64;
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(5.0), Some(last_ms)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(1), Some(redraw_count)));
}

/// The atlas-retry failure line is built only from the reading the machine kept when it refused it: the
/// failure path and the logger never reread the scene or the App's foreground cache, so a result applied
/// after the refusal cannot reach the line.
#[test]
fn the_atlas_retry_failure_line_never_rereads_the_app() {
    for name in ["drive_atlas_retry", "log_atlas_retry_evidence"] {
        let body = method(name);
        for reread in ["atlas_retry_scene(", "atlas_retry_cached(", "fg_proc_cache", "main_panes("]
        {
            assert!(!body.contains(reread), "{name} rereads `{reread}`");
        }
    }
    assert!(method("drive_atlas_retry").contains("failure_observation(&retry.machine)"));
}
