//! Pins S2 latency attribution, grid scanning and the result JSON contract.

use serde_json::{json, Value};
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

use super::*;

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
        finish_session_settled: true,
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
            "grid",
            "harness_hash",
            "harness_pid",
            "invalid_reason",
            "laps",
            "latency",
            "managed",
            "monitor",
            "native_focus_events_dropped",
            "notes",
            "phases",
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
