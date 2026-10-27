//! Pins S2 latency attribution, grid scanning and the result JSON contract.

pub(super) use serde_json::{json, Value};
pub(super) use sonicterm_grid::grid::Grid;
pub(super) use sonicterm_vt::vt::Parser;

use super::*;
pub(super) use crate::counters::FieldValue;
pub(super) use crate::scenarios::{plan_for, Host, Workload};
pub(super) use crate::workload::{fixtures, FixtureBody};

pub(super) fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

pub(super) fn snapshot(pane: &Parser, target: &EchoTarget) -> EchoSnapshot {
    snapshot_echo(pane.grid(), target)
}

/// A pane showing the fixed prompt, and where the first typed character will echo.
pub(super) fn prompted() -> (Parser, EchoTarget) {
    let mut pane = parser(80, 24);
    pane.advance(b"READY 0\r\nperf$ ");
    let origin = prompt_origin(pane.grid(), "perf$ ").expect("prompt at the cursor");
    (pane, echo_target(origin, 80, 0))
}

/// The `latency` object exactly as result.json and progress.json record it.
pub(super) fn latency_json(samples: &[LatencySample]) -> Value {
    serde_json::to_value(LatencyReport(samples)).expect("a latency report converts to JSON")
}

/// A credited sample of `latency_ms` from a build without the echo watch, as older fixtures record.
pub(super) fn credited_sample(inject_unix_s: f64, latency_ms: f64) -> LatencySample {
    LatencySample {
        inject_unix_s,
        latency_ms: Some(latency_ms),
        reason: CREDITED,
        split: None,
        split_reason: "unsupported",
        dispatch_timeline: TIMELINE_NOT_TAKEN,
        echo_timeline: None,
    }
}

pub(super) fn observe(before: EchoSnapshot, after: EchoSnapshot, advanced: bool) -> Attribution {
    attribute_dispatch(&DispatchObservation { before, after, advanced })
}

pub(super) fn partial_result(status: Status) -> RunResult {
    RunResult {
        harness_hash: Some("abc123".into()),
        scenario: "S2",
        variant: "default",
        managed: true,
        short: false,
        laps: false,
        alloc_counting: false,
        status,
        invalid_reason: Some("native Occluded(true) outside the cover".into()),
        harness_pid: 4242,
        grid: Some((250, 70)),
        monitor: Some(MonitorInfo {
            name: Some("Built-in Retina Display".into()),
            refresh_rate_millihertz: Some(60_000),
            scale_factor: 2.0,
        }),
        window_path: "production",
        synthetic_occlusion: false,
        trim_hook: TrimHookOutcome::NotReached,
        trim_experiment: None,
        trim_seq_after_hook: None,
        atlas_recovery: None,
        native_focus_events_dropped: 1,
        native_cursor_rest_events_dropped: 0,
        finish_session_settled: true,
        frame_counters: CountersMode::Off,
        presenter: None,
        phases: vec![PhaseRecord {
            name: "startup",
            kind: "sustained",
            endpoint: None,
            completion_ms: None,
            completion_missing: None,
            start_unix_s: 1.0,
            end_unix_s: 2.5,
            cpu_user_s: 0.75,
            cpu_system_s: 0.25,
            presented_frames: 2,
            redraw_requested: 3,
            first_present_ms: Some(0.5),
            last_present_ms: Some(1.25),
            first_present_seq: Some(1),
            last_present_seq: Some(2),
            present_missing: None,
            nonpresenting_redraws: 1,
            dispatch_ms: vec![4.0, 5.5, 0.5],
            dispatch_presenting_ms: Vec::new(),
            dispatch_nonpresenting_ms: Vec::new(),
            go_to_first_output_ms: None,
            first_output_to_present_ms: None,
            slow_dispatches: Vec::new(),
            dispatch_count: 3,
            present_interval_ms: vec![16.5],
            allocations_per_frame: None,
            frame_counters: None,
            updates: None,
            s10_attribution: None,
            guard_correlation: None,
            guard_correlation_transport_ns: None,
        }],
        latency: Some(vec![LatencySample::uncredited(2.0, "no-candidate")]),
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        checkpoints: vec![CheckpointRecord {
            index: 1,
            label: "end",
            unix_s: 3.0,
            footprint_file: None,
            ..CheckpointRecord::default()
        }],
        notes: vec!["typing is a labelled proxy".into()],
    }
}

#[test]
pub(super) fn result_json_carries_every_contract_field_even_for_a_partial_run() {
    // An invalid or timed-out run keeps its samples; `status` marks it for the comparison to skip.
    let value = partial_result(Status::Invalid).to_json();
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "alloc_counting",
            "checkpoint_memory",
            "checkpoints",
            "exit_code",
            "finish_session_settled",
            "frame_counters",
            "grid",
            "harness_hash",
            "harness_pid",
            "hooks",
            "invalid_reason",
            "laps",
            "latency",
            "managed",
            "monitor",
            "native_cursor_rest_events_dropped",
            "native_focus_events_dropped",
            "notes",
            "phases",
            "presenter",
            "s10_attribution_api",
            "scenario",
            "schema_version",
            "scrollback_rows_retained",
            "short",
            "status",
            "synthetic_occlusion",
            "throughput",
            "trim_experiment",
            "trim_seq_after_hook",
            "uncover_ms",
            "variant",
            "window_path",
        ]
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!((value["status"].as_str(), value["exit_code"].as_u64()), (Some("invalid"), Some(3)));
    assert_eq!(value["grid"], json!({"cols": 250, "rows": 70}));
    // The comparison rejects a pair whose displays differ, so the display is recorded.
    let display = json!({"name": "Built-in Retina Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0});
    assert_eq!(value["monitor"], display);
    let phase = &value["phases"][0];
    let mut phase_keys: Vec<_> = phase.as_object().unwrap().keys().cloned().collect();
    phase_keys.sort();
    assert_eq!(
        phase_keys,
        [
            "allocations_per_frame",
            "cpu_system_s",
            "cpu_user_s",
            "dispatch_count",
            "dispatch_ms",
            "dispatch_nonpresenting_ms",
            "dispatch_presenting_ms",
            "end_unix_s",
            "first_present_ms",
            "first_present_seq",
            "kind",
            "last_present_ms",
            "last_present_seq",
            "name",
            "nonpresenting_redraws",
            "present_interval_ms",
            "presented_frames",
            "redraw_requested",
            "slow_dispatches",
            "start_unix_s",
        ]
    );
    assert_eq!(phase["dispatch_ms"], json!([4.0, 5.5, 0.5]));
    assert_eq!(phase["allocations_per_frame"], Value::Null);
    let checkpoint = json!({"index": 1, "label": "end", "unix_s": 3.0, "footprint_file": null});
    assert_eq!(value["checkpoints"][0], checkpoint);
    assert_eq!(value["latency"]["total"], 1);
    assert_eq!(value["notes"], json!(["typing is a labelled proxy"]));
    for (status, name, code) in [
        (Status::Valid, "valid", 0),
        (Status::Invalid, "invalid", 3),
        (Status::Timeout, "timeout", 4),
        (Status::Blocked, "blocked", 5),
    ] {
        let value = partial_result(status).to_json();
        assert_eq!(
            (value["status"].as_str(), value["exit_code"].as_u64()),
            (Some(name), Some(code))
        );
        assert_eq!(status.exit_code(), code as u8);
    }
    let mut early = partial_result(Status::Timeout);
    early.grid = None;
    early.latency = None;
    early.monitor = None;
    early.finish_session_settled = false;
    early.throughput = Some(Throughput { bytes: 10, seconds: 0.5 });
    let value = early.to_json();
    assert_eq!((value["grid"].clone(), value["latency"].clone()), (Value::Null, Value::Null));
    assert_eq!(value["finish_session_settled"], false);
    assert_eq!(value["monitor"], Value::Null, "no monitor reported");
    assert_eq!(value["throughput"], json!({"bytes": 10, "seconds": 0.5}));
}

/// A finished S2 run carrying every measurement field: two phases, a credited and an
/// unattributed sample, throughput, the uncover time, retained history and two checkpoints.
pub(super) fn measured_result() -> RunResult {
    let mut result = partial_result(Status::Valid);
    result.phases.push(PhaseRecord {
        name: "typing",
        kind: "sustained",
        endpoint: None,
        completion_ms: None,
        completion_missing: None,
        start_unix_s: 2.5,
        end_unix_s: 23.0,
        cpu_user_s: 1.5,
        cpu_system_s: 0.5,
        presented_frames: 200,
        redraw_requested: 210,
        first_present_ms: Some(10.0),
        last_present_ms: Some(20_000.0),
        first_present_seq: Some(1),
        last_present_seq: Some(200),
        present_missing: None,
        nonpresenting_redraws: 10,
        dispatch_ms: vec![3.0, 2.5],
        dispatch_presenting_ms: Vec::new(),
        dispatch_nonpresenting_ms: Vec::new(),
        go_to_first_output_ms: None,
        first_output_to_present_ms: None,
        slow_dispatches: vec![
            SlowDispatch { start_unix_s: 3.0, end_unix_s: 3.003, duration_ms: 3.0 },
            SlowDispatch { start_unix_s: 4.0, end_unix_s: 4.0025, duration_ms: 2.5 },
        ],
        dispatch_count: 2,
        present_interval_ms: vec![100.0],
        allocations_per_frame: Some(vec![950, 940]),
        frame_counters: None,
        updates: None,
        s10_attribution: None,
        guard_correlation: None,
        guard_correlation_transport_ns: None,
    });
    result.latency = Some(vec![
        credited_sample(2.0, 12.5),
        LatencySample::uncredited(2.1, UnattributedReason::LockBusy.as_str()),
    ]);
    result.throughput = Some(Throughput { bytes: 5_642_880, seconds: 0.25 });
    result.uncover_ms = Some(7.5);
    result.scrollback_rows_retained = Some(10_000);
    result.checkpoints = vec![
        CheckpointRecord {
            index: 0,
            label: "settled",
            unix_s: 3.0,
            footprint_file: Some("checkpoints/0-settled.json".into()),
            ..CheckpointRecord::default()
        },
        CheckpointRecord {
            index: 1,
            label: "end",
            unix_s: 4.0,
            footprint_file: None,
            ..CheckpointRecord::default()
        },
    ];
    result
}

/// progress.json as `write_progress` streams it for `measured`, parsed.
pub(super) fn progress_of(harness_hash: Option<&str>, measured: Measurements<'_>) -> Value {
    let mut bytes = Vec::new();
    write_progress(&mut bytes, harness_hash, measured).unwrap();
    serde_json::from_slice(&bytes).expect("progress.json is JSON")
}

/// result.json as `probe::run` writes it and a reader parses it back. serde_json without
/// `float_roundtrip` can parse a 17-digit float to its neighbour, so a written document is
/// compared with a written document, never with the in-memory value.
pub(super) fn result_json_as_written(result: &RunResult) -> Value {
    let text = serde_json::to_string_pretty(&result.to_json()).unwrap();
    serde_json::from_str(&text).expect("result.json is JSON")
}

#[test]
pub(super) fn result_json_records_every_measurement_in_its_pinned_shape() {
    // result.json and progress.json share these fields, so each field's shape is pinned here.
    let value = measured_result().to_json();
    let typing = json!({
        "name": "typing",
        "kind": "sustained",
        "start_unix_s": 2.5,
        "end_unix_s": 23.0,
        "cpu_user_s": 1.5,
        "cpu_system_s": 0.5,
        "presented_frames": 200,
        "redraw_requested": 210,
        "first_present_ms": 10.0,
        "last_present_ms": 20_000.0,
        "first_present_seq": 1,
        "last_present_seq": 200,
        "nonpresenting_redraws": 10,
        "dispatch_ms": [3.0, 2.5],
        "dispatch_presenting_ms": [],
        "dispatch_nonpresenting_ms": [],
        "slow_dispatches": [
            {"start_unix_s": 3.0, "end_unix_s": 3.003, "ms": 3.0},
            {"start_unix_s": 4.0, "end_unix_s": 4.0025, "ms": 2.5},
        ],
        "dispatch_count": 2,
        "present_interval_ms": [100.0],
        "allocations_per_frame": [950, 940],
    });
    assert_eq!(value["phases"][1], typing);
    let latency = json!({
        "samples": [
            {"inject_unix_s": 2.0, "latency_ms": 12.5, "attributed": true, "reason": "credited",
             "split": null, "split_reason": "unsupported", "dispatch_timeline": "not-taken", "echo_timeline": null},
            {"inject_unix_s": 2.1, "latency_ms": null, "attributed": false, "reason": "lock-busy",
             "split": null, "split_reason": "not-credited", "dispatch_timeline": "not-taken", "echo_timeline": null},
        ],
        "attributed": 1,
        "total": 2,
        "coverage": 0.5,
        "split_schema": 1,
        "split_count": 0,
        "split_reasons": {"unsupported": 1},
        "split_coverage": 0.0,
        "echo_timeline_coverage": null,
    });
    assert_eq!(value["latency"], latency);
    assert_eq!(value["throughput"], json!({"bytes": 5_642_880, "seconds": 0.25}));
    assert_eq!(value["uncover_ms"], 7.5);
    assert_eq!(value["scrollback_rows_retained"], 10_000);
    let checkpoints = json!([
        {"index": 0, "label": "settled", "unix_s": 3.0, "footprint_file": "checkpoints/0-settled.json"},
        {"index": 1, "label": "end", "unix_s": 4.0, "footprint_file": null},
    ]);
    assert_eq!(value["checkpoints"], checkpoints);
}

#[test]
pub(super) fn progress_json_carries_every_measurement_completed_so_far() {
    // A killed run keeps every completed measurement in progress.json, each exactly as
    // result.json records it; only the outcome, the run facts and the notes wait for result.json.
    let result = measured_result();
    let progress = progress_of(result.harness_hash.as_deref(), result.measurements());
    let mut keys: Vec<_> = progress.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "checkpoints",
            "frame_counters",
            "harness_hash",
            "latency",
            "phases",
            "schema_version",
            "scrollback_rows_retained",
            "status",
            "throughput",
            "uncover_ms",
        ]
    );
    let finished = result_json_as_written(&result);
    for key in [
        "schema_version",
        "harness_hash",
        "frame_counters",
        "phases",
        "latency",
        "throughput",
        "uncover_ms",
        "scrollback_rows_retained",
        "checkpoints",
    ] {
        assert_eq!(progress[key], finished[key], "{key}");
    }
    assert_eq!(progress["status"], "running");
    // A run killed in Startup has measured nothing yet; an unmanaged run has no hash.
    let nothing = Measurements {
        frame_counters: CountersMode::Off,
        phases: &[],
        latency: None,
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        checkpoints: &[],
    };
    let progress = progress_of(None, nothing);
    for key in ["harness_hash", "latency", "throughput", "uncover_ms", "scrollback_rows_retained"] {
        assert_eq!(progress[key], Value::Null, "{key}");
    }
    assert_eq!(
        (progress["phases"].clone(), progress["checkpoints"].clone()),
        (json!([]), json!([]))
    );
}

/// S2's 200 typing samples: every character credited except one whose lock was busy.
pub(super) fn typing_samples() -> Vec<LatencySample> {
    (0..200_u32)
        .map(|index| {
            let inject_unix_s = 10.0 + f64::from(index) * 0.1;
            if index == 7 {
                let reason = UnattributedReason::LockBusy.as_str();
                LatencySample::uncredited(inject_unix_s, reason)
            } else {
                credited_sample(inject_unix_s, 20.0 + f64::from(index % 5))
            }
        })
        .collect()
}

#[test]
pub(super) fn progress_after_typing_holds_every_sample_and_its_coverage() {
    // S2 writes progress.json when typing ends, so a run killed in the idle phase after it
    // keeps all 200 samples and the coverage result.json reports from them.
    let samples = typing_samples();
    let startup = partial_result(Status::Valid).phases;
    let before_typing = Measurements {
        frame_counters: CountersMode::Off,
        phases: &startup,
        latency: Some(&[]),
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        checkpoints: &[],
    };
    let progress = progress_of(None, before_typing);
    assert_eq!(
        (progress["latency"]["total"].clone(), progress["latency"]["coverage"].clone()),
        (json!(0), json!(0.0))
    );
    let mut phases = startup.clone();
    phases.push(PhaseRecord { name: "typing", ..startup[0].clone() });
    let after_typing = Measurements { phases: &phases, latency: Some(&samples), ..before_typing };
    let progress = progress_of(None, after_typing);
    assert_eq!(progress["latency"]["samples"].as_array().map(Vec::len), Some(200));
    assert_eq!(
        (progress["latency"]["attributed"].clone(), progress["latency"]["total"].clone()),
        (json!(199), json!(200))
    );
    assert_eq!(progress["latency"]["coverage"], 199.0 / 200.0);
    // result.json, as written, records the same phases and samples identically.
    let mut finished = partial_result(Status::Valid);
    finished.phases = phases;
    finished.latency = Some(samples);
    let finished = result_json_as_written(&finished);
    assert_eq!(
        (&progress["phases"], &progress["latency"]),
        (&finished["phases"], &finished["latency"])
    );
}

#[test]
pub(super) fn phases_carry_frame_counters_only_when_the_run_counts() {
    // The top level names the mode; only an "on" run gives every phase a frame_counters
    // object, and "off" and "unsupported" runs give none.
    for mode in [CountersMode::Unsupported, CountersMode::Off] {
        let mut result = measured_result();
        result.frame_counters = mode;
        let document = result_json_as_written(&result);
        assert_eq!(document["frame_counters"], json!(mode.as_str()));
        let phases = document["phases"].as_array().unwrap();
        assert!(phases.iter().all(|phase| phase.get("frame_counters").is_none()), "{mode:?}");
    }
    let mut result = measured_result();
    result.frame_counters = CountersMode::On;
    for phase in &mut result.phases {
        phase.frame_counters = Some(CounterTotals::zero());
    }
    let document = result_json_as_written(&result);
    assert_eq!(document["frame_counters"], json!("on"));
    for phase in document["phases"].as_array().unwrap() {
        let counters = &phase["frame_counters"];
        for section in ["window", "app", "vt", "renderer"] {
            assert!(counters[section].is_object(), "{section}: {counters}");
        }
        assert_eq!(counters["window"]["handler_ms"]["counts"].as_array().map(Vec::len), Some(10));
        assert_eq!(counters["vt"]["parse_us"]["counts"].as_array().map(Vec::len), Some(7));
    }
}

/// A pane parser of a run's 250 x 70 grid that keeps at most `scrollback` history rows.
pub(super) fn run_pane(scrollback: usize) -> Parser {
    let mut grid = Grid::new(250, 70);
    grid.set_scrollback_limit(scrollback);
    Parser::new(grid)
}

/// A pane `cols` wide with a 100-row history, fed READY, `lines`, the sentinel and a prompt as
/// ConPTY delivers them; returns the pane with its READY and sentinel rows.
pub(super) fn flood_pane(cols: u16, lines: &[String]) -> (Parser, u64, u64) {
    let mut grid = Grid::new(cols, 70);
    grid.set_scrollback_limit(100);
    let mut pane = Parser::new(grid);
    pane.advance(b"READY 0\r\n");
    let ready = line_row_near_cursor(pane.grid(), "READY 0", 3).expect("READY near the cursor");
    for line in lines {
        pane.advance(format!("{line}\r\n").as_bytes());
    }
    let sentinel_text = "PERF_DONE 0 0123456789abcdef";
    pane.advance(format!("{sentinel_text}\r\nperf$ ").as_bytes());
    let sentinel = line_row_near_cursor(pane.grid(), sentinel_text, protocol_rows(Host::Windows))
        .expect("sentinel near the cursor");
    (pane, ready, sentinel)
}

/// A dispatch of `duration_ms` milliseconds that starts at second `start_s`.
pub(super) fn dispatch_of(start_s: f64, duration_ms: f64) -> SlowDispatch {
    SlowDispatch { start_unix_s: start_s, end_unix_s: start_s + duration_ms / 1000.0, duration_ms }
}

/// Instants for one fully split sample: injected, parsed, published, decided, ended, in that order,
/// with odd nanosecond gaps so a rounding error would show.
pub(super) fn split_instants() -> [Instant; 5] {
    let injected = Instant::now();
    let parsed = injected + Duration::from_nanos(3_000_001);
    let published = parsed + Duration::from_nanos(1_234_567);
    let decided = published + Duration::from_nanos(41_003);
    let ended = published + Duration::from_nanos(9_876_543);
    [injected, parsed, published, decided, ended]
}

/// A taken record that splits: shown, unchanged, one appearance, a publication to the measurement
/// window and a sent decision.
pub(super) fn split_facts([_, parsed, published, decided, _]: [Instant; 5]) -> TraceFacts {
    TraceFacts {
        shown: true,
        identity_changed: false,
        pre_present: false,
        lost: false,
        appearance: Some(AppearanceFacts { generation: 7, parsed_at: parsed, sync_open: false }),
        publication: Some(PublicationFacts {
            published_at: published,
            window: PublicationWindow::Main,
            coalesced: false,
        }),
        delivery: Some(DeliveryFacts { outcome: Delivery::Sent, decided_at: decided }),
    }
}

/// An App whose gate is on, with one seeded pane carrying real counter handles.
#[cfg(feature = "perf-echo-trace")]
pub(super) fn counting_app() -> (sonicterm_app::app::App, u64) {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::new("warn"));
    let mut app = sonicterm_logging::test_capture::with_default(subscriber, || {
        sonicterm_app::app::App::new(
            sonicterm_cfg::theme::Theme::default(),
            sonicterm_cfg::config::Config::default(),
            sonicterm_cfg::keymap::Keymap::default(),
        )
    });
    app.force_frame_counters_on().expect("no window or pane yet");
    let pane = app.__test_seed_counting_tab("s2");
    (app, pane)
}

/// The split reason of one sample through the real App protocol: arm `pane` for `a` at row 0,
/// column 0, run `between`, then take and split with the dispatch ending at `ended`.
#[cfg(feature = "perf-echo-trace")]
pub(super) fn app_reason(
    app: &mut sonicterm_app::app::App,
    pane: u64,
    between: impl FnOnce(&mut sonicterm_app::app::App, Option<EchoToken>),
    ended: impl FnOnce(Instant) -> Instant,
) -> &'static str {
    let main = sonicterm_app::app::synthetic_main_window_id();
    let identity = app
        .main_panes()
        .and_then(|panes| panes.get(&pane))
        .map_or(RowIdentity::default(), |state| prompt_identity(state.parser.lock().grid()));
    let target = EchoTarget { abs_row: 0, col: 0, character: 'a' };
    let injected = Instant::now();
    let arm = echo_api::arm(app, pane, &target, identity);
    let token = match arm {
        ArmState::Armed(token) => Some(token),
        _ => None,
    };
    between(app, token);
    let outcome = echo_outcome(arm, |token| echo_api::take(app, pane, token, Some(main)).0);
    LatencySample::credited(1.0, injected, ended(Instant::now()), &outcome, false).split_reason
}

/// The completeness reading serializes as the comparison reads it: a certified reading carries its two
/// distinct-character counts and atlas dimensions and no reason; an unavailable one carries only its
/// reason; both carry the renderer's scale.
#[test]
pub(super) fn a_completeness_reading_serializes_as_the_comparison_reads_it() {
    let certified =
        serde_json::to_value(CompletenessRecord::certified(2, 1, (512, 1024), 1.0)).unwrap();
    assert_eq!(
        certified,
        json!({"state": "certified", "missing_terminal": 2, "missing_chrome": 1,
               "atlas_width": 512, "atlas_height": 1024, "scale": 1.0})
    );
    let unavailable =
        serde_json::to_value(CompletenessRecord::unavailable("scene changed", 2.0)).unwrap();
    assert_eq!(
        unavailable,
        json!({"state": "unavailable", "reason": "scene changed", "scale": 2.0})
    );
}

/// Only S9's and S12's `end` checkpoints take a completeness reading; every other checkpoint and
/// scenario records none, so no other row can be mistaken for a perf-end census.
#[test]
pub(super) fn only_s9_and_s12_end_take_a_completeness_reading() {
    assert!(takes_completeness("S9", "end"));
    assert!(takes_completeness("S12", "end"));
    for (scenario, label) in [("S9", "start"), ("S12", "covered"), ("S11", "end"), ("S1", "end")] {
        assert!(!takes_completeness(scenario, label), "{scenario} {label}");
    }
}

/// A build without `perf_completeness_api` never calls the renderer's checkpoint and reports the
/// reading unavailable, never certified, so an older base reads `unavailable`.
#[test]
pub(super) fn a_build_without_the_completeness_api_reads_unavailable() {
    assert_eq!(
        completeness_api_disabled(1.0),
        CompletenessRecord::unavailable(COMPLETENESS_API_DISABLED, 1.0)
    );
    assert_eq!(COMPLETENESS_API_DISABLED, "api-disabled");
}

/// With the cfg on, each renderer checkpoint maps to its record: counts and dimensions carried over,
/// and each unavailable reason kept by the renderer's own name.
#[cfg(perf_completeness_api)]
#[test]
pub(super) fn a_renderer_checkpoint_maps_to_its_record() {
    use sonicterm_gpu::completeness::{
        CompletenessCheckpoint, CompletenessCounts, CompletenessUnavailable,
    };
    let counts =
        CompletenessCounts { missing_terminal: 3, missing_chrome: 0, atlas_dims: (512, 512) };
    assert_eq!(
        completeness_from_checkpoint(CompletenessCheckpoint::Certified(counts), 1.0),
        CompletenessRecord::certified(3, 0, (512, 512), 1.0)
    );
    for reason in [
        CompletenessUnavailable::NoCertificate,
        CompletenessUnavailable::SceneChanged,
        CompletenessUnavailable::AtlasChanged,
    ] {
        assert_eq!(
            completeness_from_checkpoint(CompletenessCheckpoint::Unavailable(reason), 2.0),
            CompletenessRecord::unavailable(reason.reason(), 2.0)
        );
    }
}

/// S6/rest prints its sentinel on its fourth row, then sets its scroll region and moves the cursor to
/// the bottom row; all of it, and the first streamed line, can reach the parser in one batch before
/// discovery runs. The cursor-relative scan then misses the sentinel, so the print phase would time
/// out; the rest workload's discovery searches the screen and finds the sentinel's own row.
#[test]
pub(super) fn the_rest_sentinel_is_found_when_the_cursor_moved_away_in_the_same_batch() {
    let mut pane = run_pane(100);
    pane.advance(b"READY 0\r\n");
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    let batch = format!(
        "\x1b[H\x1b[2Jhttps://example.com/sonicterm/rest\r\n/tmp/scratch/workload/rest-target.txt\r\n\r\n\
         {sentinel}\r\n\x1b[{}r\x1b[999;1HThu Jan  1 00:00:00 UTC 1970\r\n",
        crate::scenarios::REST_REGION_TOP
    );
    pane.advance(batch.as_bytes());
    let grid = pane.grid();
    assert_eq!(grid.cursor.row, grid.rows - 1, "the cursor sits at the region's bottom");
    for host in [Host::Posix, Host::Windows] {
        assert_eq!(line_row_near_cursor(grid, sentinel, protocol_rows(host)), None);
        let found =
            sentinel_row(grid, sentinel, Workload::RestStream, host).expect("the sentinel row");
        assert_eq!(found, grid.scrollback_evicted() + grid.scrollback_len() as u64 + 3);
    }
    // Other workloads keep the cursor-relative scan.
    assert_eq!(sentinel_row(grid, sentinel, Workload::IdleShell, Host::Posix), None);
}

/// Role 0's pane after the S6/rest fixture and its scroll region setup, as the workload prints them.
pub(super) fn rest_fixture_pane(sentinel: &str) -> Parser {
    let mut pane = run_pane(100);
    let fixture = format!(
        "\x1b[H\x1b[2Jhttps://example.com/sonicterm/rest\r\n/tmp/scratch/workload/rest-target.txt\r\n\r\n\
         {sentinel}\r\n\x1b[{}r\x1b[999;1H",
        crate::scenarios::REST_REGION_TOP
    );
    pane.advance(fixture.as_bytes());
    pane
}
