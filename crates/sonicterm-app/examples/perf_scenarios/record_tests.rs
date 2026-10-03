//! Pins S2 latency attribution, grid scanning and the result JSON contract.

use serde_json::{json, Value};
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

use super::*;
use crate::counters::FieldValue;
use crate::scenarios::{plan_for, Host, Presentation};
use crate::workload::{fixtures, FixtureBody};

fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

fn snapshot(pane: &Parser, target: &EchoTarget) -> EchoSnapshot {
    snapshot_echo(pane.grid(), target)
}

/// A pane showing the fixed prompt, and where the first typed character will echo.
fn prompted() -> (Parser, EchoTarget) {
    let mut pane = parser(80, 24);
    pane.advance(b"READY 0\r\nperf$ ");
    let origin = prompt_origin(pane.grid(), "perf$ ").expect("prompt at the cursor");
    (pane, echo_target(origin, 80, 0))
}

/// The `latency` object exactly as result.json and progress.json record it.
fn latency_json(samples: &[LatencySample]) -> Value {
    serde_json::to_value(LatencyReport(samples)).expect("a latency report converts to JSON")
}

fn observe(before: EchoSnapshot, after: EchoSnapshot, advanced: bool) -> Attribution {
    attribute_dispatch(&DispatchObservation { before, after, advanced })
}

#[test]
fn echo_parsed_before_the_input_frame_is_credited_to_that_frame() {
    // The frame the commit requested presents an echo that the PTY returned first.
    let (mut pane, target) = prompted();
    let revision = pane.grid().revision();
    assert_eq!(snapshot(&pane, &target), EchoSnapshot::Read { present: false, revision });
    pane.advance(b"a");
    let before = snapshot(&pane, &target);
    assert!(matches!(before, EchoSnapshot::Read { present: true, .. }));
    assert_eq!(observe(before, snapshot(&pane, &target), true), Attribution::Credited);
}

#[test]
fn echo_parsed_after_unlock_is_credited_to_the_next_presenting_frame() {
    // The worker unlocks the parser before its trailing wake; a wake is not proof of a present.
    let (mut pane, target) = prompted();
    let empty = snapshot(&pane, &target);
    assert_eq!(observe(empty, empty, false), Attribution::Pending);
    pane.advance(b"a");
    let seen = snapshot(&pane, &target);
    assert_eq!(observe(seen, seen, false), Attribution::Pending, "nothing presented yet");
    assert_eq!(observe(seen, snapshot(&pane, &target), true), Attribution::Credited);
}

#[test]
fn unrelated_chrome_frames_before_the_echo_stay_pending() {
    // Cursor or tab-bar frames that present before the echo arrives never take its credit.
    let (mut pane, target) = prompted();
    let empty = snapshot(&pane, &target);
    assert_eq!(observe(empty, empty, true), Attribution::Pending);
    assert_eq!(observe(empty, empty, true), Attribution::Pending);
    pane.advance(b"a");
    let seen = snapshot(&pane, &target);
    assert_eq!(observe(seen, seen, true), Attribution::Credited);
}

#[test]
fn concurrent_output_in_another_pane_leaves_the_active_pane_creditable() {
    // Snapshots read only the active pane, so output elsewhere does not move its revision.
    let (mut pane, target) = prompted();
    let mut flooding = parser(80, 24);
    pane.advance(b"a");
    let before = snapshot(&pane, &target);
    flooding.advance(b"y\r\ny\r\ny\r\n");
    assert_eq!(observe(before, snapshot(&pane, &target), true), Attribution::Credited);
}

#[test]
fn revision_change_during_the_first_frame_holding_the_echo_is_unattributed() {
    // Output parsed mid-frame leaves the presented grid unknown, so no frame is credited.
    let (mut pane, target) = prompted();
    pane.advance(b"a");
    let before = snapshot(&pane, &target);
    pane.advance(b"b");
    let changed = Attribution::Unattributed(UnattributedReason::RevisionChanged);
    assert_eq!(observe(before, snapshot(&pane, &target), true), changed);
}

#[test]
fn echo_first_seen_during_a_presenting_frame_is_unattributed() {
    // That frame may or may not have drawn the echo, so neither it nor a later frame is credited.
    let (mut pane, target) = prompted();
    let before = snapshot(&pane, &target);
    pane.advance(b"a");
    let after = snapshot(&pane, &target);
    let during = Attribution::Unattributed(UnattributedReason::EchoDuringFrame);
    assert_eq!(observe(before, after, true), during);
    assert_eq!(
        observe(before, after, false),
        Attribution::Pending,
        "a dispatch that presents nothing"
    );
}

#[test]
fn busy_lock_at_a_presenting_frame_forfeits_the_sample() {
    // A presenting frame whose grid could not be read may have drawn the echo.
    let read = EchoSnapshot::Read { present: false, revision: 1 };
    let busy = Attribution::Unattributed(UnattributedReason::LockBusy);
    for (before, after) in [(EchoSnapshot::Busy, read), (read, EchoSnapshot::Busy)] {
        assert_eq!(observe(before, after, true), busy);
        assert_eq!(observe(before, after, false), Attribution::Pending);
    }
}

#[test]
fn echo_targets_follow_the_prompt_and_wrap_at_the_grid_width() {
    // Typed character `index` lands at the prompt column plus `index`, wrapping to the next row.
    let (pane, first) = prompted();
    let prompt_row = pane.grid().scrollback_len() as u64 + 1;
    assert_eq!(first, EchoTarget { abs_row: prompt_row, col: 6, character: 'a' });
    let last_column = EchoTarget { abs_row: prompt_row, col: 79, character: typed_character(73) };
    assert_eq!(echo_target((prompt_row, 6), 80, 73), last_column);
    let wrapped = EchoTarget { abs_row: prompt_row + 1, col: 0, character: typed_character(74) };
    assert_eq!(echo_target((prompt_row, 6), 80, 74), wrapped);
    assert_eq!([typed_character(0), typed_character(35), typed_character(36)], ['a', '9', 'a']);
    // A cursor that is not just past the prompt is not at a prompt.
    let mut typed = parser(80, 24);
    typed.advance(b"perf$ x");
    assert_eq!(prompt_origin(typed.grid(), "perf$ "), None);
}

#[test]
fn protocol_lines_are_found_only_at_a_row_start_near_the_cursor() {
    // The probe scans a few rows above the cursor, and a match must start its row.
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    let mut pane = parser(80, 24);
    pane.advance(b"echo 'READY 0'\r\nPERF_DONE 0 0123456789abcdef\r\nperf$ ");
    assert!(line_near_cursor(pane.grid(), sentinel, 3));
    assert!(!line_near_cursor(pane.grid(), "PERF_DONE 1 0123456789abcdef", 3));
    assert!(!line_near_cursor(pane.grid(), "READY 0", 3), "quoted text is not a protocol line");
    for _ in 0..4 {
        pane.advance(b"\r\nfiller");
    }
    assert!(!line_near_cursor(pane.grid(), sentinel, 3), "scrolled out of reach");
}

#[test]
fn latency_summary_reports_attribution_coverage() {
    // Latency acceptance needs coverage, so unattributed samples stay counted and named.
    let samples = [
        LatencySample { inject_unix_s: 1.0, latency_ms: Some(12.5), reason: CREDITED },
        LatencySample {
            inject_unix_s: 1.25,
            latency_ms: None,
            reason: UnattributedReason::LockBusy.as_str(),
        },
        LatencySample { inject_unix_s: 1.5, latency_ms: Some(9.0), reason: CREDITED },
        LatencySample {
            inject_unix_s: 1.75,
            latency_ms: None,
            reason: UnattributedReason::NoCandidate.as_str(),
        },
    ];
    let summary = latency_json(&samples);
    assert_eq!((summary["attributed"].as_u64(), summary["total"].as_u64()), (Some(2), Some(4)));
    assert_eq!(summary["coverage"], 0.5);
    let lock_busy = json!({"inject_unix_s": 1.25, "latency_ms": null, "attributed": false, "reason": "lock-busy"});
    assert_eq!(summary["samples"][1], lock_busy);
    assert_eq!(summary["samples"][0]["attributed"], true);
    assert_eq!(summary["samples"][3]["reason"], "no-candidate");
    // No samples report coverage 0.0, so a reader that needs a number never meets null.
    assert_eq!(latency_json(&[])["coverage"], 0.0);
    let reasons = [
        UnattributedReason::EchoDuringFrame.as_str(),
        UnattributedReason::RevisionChanged.as_str(),
    ];
    assert_eq!(reasons, ["echo-during-frame", "revision-changed"]);
}

fn partial_result(status: Status) -> RunResult {
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
        native_focus_events_dropped: 1,
        native_cursor_rest_events_dropped: 0,
        finish_session_settled: true,
        frame_counters: CountersMode::Off,
        presenter: None,
        phases: vec![PhaseRecord {
            name: "startup",
            start_unix_s: 1.0,
            end_unix_s: 2.5,
            cpu_user_s: 0.75,
            cpu_system_s: 0.25,
            presented_frames: 2,
            redraw_requested: 3,
            dispatch_ms: vec![4.0, 5.5, 0.5],
            present_interval_ms: vec![16.5],
            allocations_per_frame: None,
            frame_counters: None,
        }],
        latency: Some(vec![LatencySample {
            inject_unix_s: 2.0,
            latency_ms: None,
            reason: "no-candidate",
        }]),
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        checkpoints: vec![CheckpointRecord {
            index: 1,
            label: "end",
            unix_s: 3.0,
            footprint_file: None,
        }],
        notes: vec!["typing is a labelled proxy".into()],
    }
}

#[test]
fn result_json_carries_every_contract_field_even_for_a_partial_run() {
    // An invalid or timed-out run keeps its samples; `status` marks it for the comparison to skip.
    let value = partial_result(Status::Invalid).to_json();
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "alloc_counting",
            "checkpoints",
            "exit_code",
            "finish_session_settled",
            "frame_counters",
            "grid",
            "harness_hash",
            "harness_pid",
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
            "scenario",
            "schema_version",
            "scrollback_rows_retained",
            "short",
            "status",
            "synthetic_occlusion",
            "throughput",
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
            "dispatch_ms",
            "end_unix_s",
            "name",
            "present_interval_ms",
            "presented_frames",
            "redraw_requested",
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
fn measured_result() -> RunResult {
    let mut result = partial_result(Status::Valid);
    result.phases.push(PhaseRecord {
        name: "typing",
        start_unix_s: 2.5,
        end_unix_s: 23.0,
        cpu_user_s: 1.5,
        cpu_system_s: 0.5,
        presented_frames: 200,
        redraw_requested: 210,
        dispatch_ms: vec![3.0, 2.5],
        present_interval_ms: vec![100.0],
        allocations_per_frame: Some(vec![950, 940]),
        frame_counters: None,
    });
    result.latency = Some(vec![
        LatencySample { inject_unix_s: 2.0, latency_ms: Some(12.5), reason: CREDITED },
        LatencySample {
            inject_unix_s: 2.1,
            latency_ms: None,
            reason: UnattributedReason::LockBusy.as_str(),
        },
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
        },
        CheckpointRecord { index: 1, label: "end", unix_s: 4.0, footprint_file: None },
    ];
    result
}

/// progress.json as `write_progress` streams it for `measured`, parsed.
fn progress_of(harness_hash: Option<&str>, measured: Measurements<'_>) -> Value {
    let mut bytes = Vec::new();
    write_progress(&mut bytes, harness_hash, measured).unwrap();
    serde_json::from_slice(&bytes).expect("progress.json is JSON")
}

/// result.json as `probe::run` writes it and a reader parses it back. serde_json without
/// `float_roundtrip` can parse a 17-digit float to its neighbour, so a written document is
/// compared with a written document, never with the in-memory value.
fn result_json_as_written(result: &RunResult) -> Value {
    let text = serde_json::to_string_pretty(&result.to_json()).unwrap();
    serde_json::from_str(&text).expect("result.json is JSON")
}

#[test]
fn result_json_records_every_measurement_in_its_pinned_shape() {
    // result.json and progress.json share these fields, so each field's shape is pinned here.
    let value = measured_result().to_json();
    let typing = json!({
        "name": "typing",
        "start_unix_s": 2.5,
        "end_unix_s": 23.0,
        "cpu_user_s": 1.5,
        "cpu_system_s": 0.5,
        "presented_frames": 200,
        "redraw_requested": 210,
        "dispatch_ms": [3.0, 2.5],
        "present_interval_ms": [100.0],
        "allocations_per_frame": [950, 940],
    });
    assert_eq!(value["phases"][1], typing);
    let latency = json!({
        "samples": [
            {"inject_unix_s": 2.0, "latency_ms": 12.5, "attributed": true, "reason": "credited"},
            {"inject_unix_s": 2.1, "latency_ms": null, "attributed": false, "reason": "lock-busy"},
        ],
        "attributed": 1,
        "total": 2,
        "coverage": 0.5,
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
fn progress_json_carries_every_measurement_completed_so_far() {
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
fn typing_samples() -> Vec<LatencySample> {
    (0..200_u32)
        .map(|index| {
            let inject_unix_s = 10.0 + f64::from(index) * 0.1;
            if index == 7 {
                let reason = UnattributedReason::LockBusy.as_str();
                LatencySample { inject_unix_s, latency_ms: None, reason }
            } else {
                let latency_ms = Some(20.0 + f64::from(index % 5));
                LatencySample { inject_unix_s, latency_ms, reason: CREDITED }
            }
        })
        .collect()
}

#[test]
fn progress_after_typing_holds_every_sample_and_its_coverage() {
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
fn phases_carry_frame_counters_only_when_the_run_counts() {
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

#[test]
fn filetime_seconds_counts_hundred_nanosecond_ticks() {
    // GetProcessTimes reports CPU time as 100 ns ticks split into two 32-bit words.
    assert_eq!(filetime_seconds(10_000_000, 0), 1.0);
    assert_eq!(filetime_seconds(0, 0), 0.0);
    // One tick in the high word is 2^32 ticks: 429.4967296 s.
    assert_eq!(filetime_seconds(0, 1), 429.496_729_6);
    assert_eq!(filetime_seconds(5_000_000, 1), 429.996_729_6);
}

#[test]
fn result_json_carries_the_presenter_block() {
    // The comparison pairs runs by presenter, so the record says how the run presented, or null.
    let mut result = partial_result(Status::Valid);
    let value = result.to_json();
    assert!(value.as_object().unwrap().contains_key("presenter"));
    assert_eq!(value["presenter"], Value::Null);
    result.presenter = Some(PresenterRecord {
        software_render_mode: "force",
        software_rendering: false,
        software_render_degraded: true,
        windows_gdi: true,
    });
    let expected = json!({"software_render_mode": "force", "software_rendering": false,
                          "software_render_degraded": true, "windows_gdi": true});
    assert_eq!(result.to_json()["presenter"], expected);
}

#[test]
fn a_windows_presenter_that_misses_its_variant_blocks_the_run() {
    // A gdi run that did not present through GDI, or a degraded wgpu run, cannot measure its variant.
    let presented = |degraded: bool, gdi: bool| PresenterRecord {
        software_render_mode: "auto",
        software_rendering: false,
        software_render_degraded: degraded,
        windows_gdi: gdi,
    };
    let gdi_reason =
        presenter_blocked(Presentation::ForceGdi, &presented(false, false), Host::Windows).unwrap();
    assert!(gdi_reason.contains("gdi") && gdi_reason.contains("windows_gdi"), "{gdi_reason}");
    assert!(
        presenter_blocked(Presentation::ForceGdi, &presented(true, true), Host::Windows).is_none()
    );
    let wgpu_reason =
        presenter_blocked(Presentation::ForceWgpu, &presented(true, false), Host::Windows).unwrap();
    assert!(wgpu_reason.contains("wgpu") && wgpu_reason.contains("software_render_degraded"));
    assert!(presenter_blocked(Presentation::ForceWgpu, &presented(false, false), Host::Windows)
        .is_none());
    assert!(presenter_blocked(Presentation::Configured, &presented(true, false), Host::Windows)
        .is_none());
    assert!(
        presenter_blocked(Presentation::ForceGdi, &presented(false, false), Host::Posix).is_none()
    );
}

/// A pane parser of a run's 250 x 70 grid that keeps at most `scrollback` history rows.
fn run_pane(scrollback: usize) -> Parser {
    let mut grid = Grid::new(250, 70);
    grid.set_scrollback_limit(scrollback);
    Parser::new(grid)
}

#[test]
fn rows_between_ready_and_sentinel_count_only_the_workload() {
    // S3's delivered lines lie strictly between the READY row and the sentinel's row, whose
    // lifetime-absolute numbers survive history eviction; ConPTY ends each line with CR LF.
    let mut pane = run_pane(100);
    pane.advance(b"READY 0\r\n");
    let ready = line_row_near_cursor(pane.grid(), "READY 0", 3).expect("READY near the cursor");
    let lines: Vec<String> =
        (0..500).map(|index| format!("line {index:04} of the workload")).collect();
    for line in &lines {
        pane.advance(format!("{line}\r\n").as_bytes());
    }
    let sentinel_text = "PERF_DONE 0 0123456789abcdef";
    pane.advance(format!("{sentinel_text}\r\nperf$ ").as_bytes());
    let sentinel =
        line_row_near_cursor(pane.grid(), sentinel_text, 3).expect("sentinel near the cursor");
    // READY has left the retained history, yet the count between the two rows holds.
    assert!(pane.grid().scrollback_evicted() > ready, "READY row {ready} was not evicted");
    assert_eq!(row_count_mismatch(ready, sentinel, 500), None);
    let reason = row_count_mismatch(ready, sentinel, 501).unwrap();
    assert!(reason.contains("500") && reason.contains("501"), "{reason}");
    // The retained rows above the sentinel are the end of the workload, in order.
    let tail: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(bulk_tail_mismatch(pane.grid(), sentinel, &tail), None);
    let mut altered = tail.clone();
    *altered.last_mut().unwrap() = "line 9999 that never arrived";
    let reason = bulk_tail_mismatch(pane.grid(), sentinel, &altered).unwrap();
    assert!(reason.contains("line 9999"), "{reason}");
}

#[test]
fn row_count_is_the_same_whenever_the_scan_runs() {
    // The sentinel's lifetime row does not move as later output evicts history, so a late scan
    // counts the same rows; a dropped or an added line is a mismatch naming both counts.
    let mut pane = run_pane(100);
    pane.advance(b"READY 0\r\n");
    let ready = line_row_near_cursor(pane.grid(), "READY 0", 3).unwrap();
    for index in 0..300 {
        pane.advance(format!("y {index}\r\n").as_bytes());
    }
    let sentinel_text = "PERF_DONE 0 0123456789abcdef";
    pane.advance(format!("{sentinel_text}\r\n").as_bytes());
    let early = line_row_near_cursor(pane.grid(), sentinel_text, 3).unwrap();
    let evicted_early = pane.grid().scrollback_evicted();
    pane.advance(b"perf$ \r\nperf$ ");
    let late = line_row_near_cursor(pane.grid(), sentinel_text, 3).unwrap();
    assert!(pane.grid().scrollback_evicted() > evicted_early, "the later output evicted no row");
    assert_eq!(early, late);
    assert_eq!(row_count_mismatch(ready, late, 300), None);
    for planned in [299_u64, 301] {
        let reason = row_count_mismatch(ready, late, planned).unwrap();
        assert!(reason.contains(&planned.to_string()) && reason.contains("300"), "{reason}");
    }
}

#[test]
fn progress_records_the_same_frame_counters_as_the_result() {
    // A run killed between phases keeps each finished phase's counters in progress.json,
    // exactly as result.json would write them.
    let mut result = measured_result();
    result.frame_counters = CountersMode::On;
    let mut counted = CounterTotals::zero();
    counted.values[0] = Some(FieldValue::Count(3));
    for phase in &mut result.phases {
        phase.frame_counters = Some(counted.clone());
    }
    let progress = progress_of(None, result.measurements());
    let finished = result_json_as_written(&result);
    assert_eq!(progress["frame_counters"], json!("on"));
    assert_eq!(progress["frame_counters"], finished["frame_counters"]);
    assert_eq!(progress["phases"], finished["phases"]);
    assert_eq!(progress["phases"][0]["frame_counters"]["window"]["attempts"], json!(3));
}

#[test]
fn missing_wide_tokens_names_each_absent_token() {
    // S9 is blocked when the grid lacks a token its fixture printed, and the reason names each one.
    let tokens = ["😀", "漢字", "👨‍👩‍👧‍👦", "🇯🇵"];
    assert_eq!(missing_wide_tokens("alpha 😀 漢字 👨‍👩‍👧‍👦 🇯🇵", &tokens), Vec::<&str>::new());
    assert_eq!(missing_wide_tokens("alpha 😀 🇯🇵", &tokens), ["漢字", "👨‍👩‍👧‍👦"]);
}

#[test]
fn the_emoji_fixture_reads_back_whole_from_the_grid() {
    // S9's Windows check reads the retained rows back as text, so every token the fixture prints,
    // joined and flag sequences included, must survive the grid and the copy path.
    let text_plan = plan_for("S9", "default", false, Host::Posix).unwrap();
    let files = fixtures(&text_plan);
    let [emoji] = files.as_slice() else { panic!("S9 writes one fixture") };
    let FixtureBody::Bytes(bytes) = &emoji.body else { panic!("the fixture is bytes") };
    let text = std::str::from_utf8(bytes).unwrap();
    let mut pane = run_pane(1_000);
    pane.advance(text.replace('\n', "\r\n").as_bytes());
    let tokens = wide_tokens(text);
    assert!(tokens.len() >= 10, "{tokens:?}");
    assert_eq!(missing_wide_tokens(&retained_text(pane.grid()), &tokens), Vec::<&str>::new());
}

#[test]
fn a_sentinel_above_cmds_banner_is_found_on_windows() {
    // On Windows the idle shell is cmd.exe, whose banner and a blank line come between the sentinel and
    // its first prompt, so the sentinel sits four rows above the cursor: inside Windows's scan, not POSIX's.
    let mut pane = run_pane(100);
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    pane.advance(format!("{sentinel}\r\n").as_bytes());
    pane.advance(b"Microsoft Windows [Version 10.0.26300.1]\r\n");
    pane.advance(b"(c) Microsoft Corporation. All rights reserved.\r\n\r\nperf$ ");
    let windows = line_row_near_cursor(pane.grid(), sentinel, protocol_rows(Host::Windows));
    assert!(windows.is_some(), "the Windows scan missed the sentinel above the banner");
    assert_eq!(line_row_near_cursor(pane.grid(), sentinel, protocol_rows(Host::Posix)), None);
}

/// A pane `cols` wide with a 100-row history, fed READY, `lines`, the sentinel and a prompt as
/// ConPTY delivers them; returns the pane with its READY and sentinel rows.
fn flood_pane(cols: u16, lines: &[String]) -> (Parser, u64, u64) {
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

#[test]
fn a_flood_wider_than_the_grid_counts_its_wrapped_rows() {
    // A window can open narrower than S3's 127-column bulk lines, which then wrap onto two rows; the
    // count is in rows at the pane's width, and the tail compares whole lines across the wraps.
    let mut lines: Vec<String> = vec!["y".to_owned(); 50];
    for index in 0..100 {
        // A space falls on column 100, the last cell of the first row, so a join that trims it fails.
        let mut line = format!("line {index:04} ") + &"abcd ".repeat(24);
        line.truncate(127);
        lines.push(line);
    }
    // Exactly as wide as the grid: the wrap is deferred, so the line takes one row.
    lines.push(format!("{:-<100}", "edge"));
    let widths = || lines.iter().map(|line| line.chars().count());
    assert_eq!(planned_rows(widths(), 100), 50 + 2 * 100 + 1);
    assert_eq!(planned_rows(widths(), 250), 151);
    let (pane, ready, sentinel) = flood_pane(100, &lines);
    assert_eq!(row_count_mismatch(ready, sentinel, planned_rows(widths(), 100)), None);
    let tail: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(bulk_tail_mismatch(pane.grid(), sentinel, &tail), None);
    // A dropped line is still a mismatch, in the row count and in the retained tail.
    let mut dropped = lines.clone();
    dropped.remove(lines.len() - 5);
    let (pane, ready, sentinel) = flood_pane(100, &dropped);
    let reason = row_count_mismatch(ready, sentinel, planned_rows(widths(), 100)).unwrap();
    assert!(reason.contains("249") && reason.contains("251"), "{reason}");
    assert!(bulk_tail_mismatch(pane.grid(), sentinel, &tail).is_some());
}

#[test]
fn a_failed_foreground_lock_is_noted() {
    // A failed LockSetForegroundWindow is not fatal: the run goes on, and its notes name the call
    // and the error, so a reader knows the window may have taken the foreground as it opened.
    let note = foreground_lock_note("Access is denied. (0x80070005)");
    let expected = "LockSetForegroundWindow failed: Access is denied. (0x80070005)";
    assert!(note.starts_with(expected), "{note}");
    assert!(note.contains("take the foreground"), "{note}");
}
