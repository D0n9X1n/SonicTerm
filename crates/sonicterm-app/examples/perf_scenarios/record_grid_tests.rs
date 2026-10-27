//! Grid sentinel, prompt and workload completeness contracts.

use super::super::record_tests::*;
use super::*;

#[test]
fn echo_parsed_before_the_input_frame_is_credited_to_that_frame() {
    // The frame the commit requested presents an echo that the PTY returned first.
    let (mut pane, target) = prompted();
    let revision = pane.grid().revision();
    assert_eq!(
        snapshot(&pane, &target),
        EchoSnapshot::Read { present: false, revision, identity: prompt_identity(pane.grid()) }
    );
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
    let read = EchoSnapshot::Read { present: false, revision: 1, identity: RowIdentity::default() };
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
        credited_sample(1.0, 12.5),
        LatencySample::uncredited(1.25, UnattributedReason::LockBusy.as_str()),
        credited_sample(1.5, 9.0),
        LatencySample::uncredited(1.75, UnattributedReason::NoCandidate.as_str()),
    ];
    let summary = latency_json(&samples);
    assert_eq!((summary["attributed"].as_u64(), summary["total"].as_u64()), (Some(2), Some(4)));
    assert_eq!(summary["coverage"], 0.5);
    let lock_busy = json!({"inject_unix_s": 1.25, "latency_ms": null, "attributed": false, "reason": "lock-busy", "split": null, "split_reason": "not-credited", "dispatch_timeline": "not-taken", "echo_timeline": null});
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
    // A macOS run records its presenter too: wgpu, never Windows GDI.
    result.presenter = Some(presenter_record_for(Host::Posix, "auto", false, false));
    let expected = json!({"software_render_mode": "auto", "software_rendering": false,
                          "software_render_degraded": false, "windows_gdi": false});
    assert_eq!(result.to_json()["presenter"], expected);
}

#[test]
fn the_presenter_record_names_gdi_only_for_a_degraded_windows_run() {
    // macOS stays on wgpu whether or not it degrades; only Windows' degrade path presents through GDI.
    let record = |host, degraded| {
        let presenter = presenter_record_for(host, "auto", degraded, degraded);
        (presenter.software_render_degraded, presenter.windows_gdi)
    };
    assert_eq!(record(Host::Posix, false), (false, false));
    assert_eq!(record(Host::Posix, true), (true, false));
    assert_eq!(record(Host::Windows, true), (true, true));
    assert_eq!(record(Host::Windows, false), (false, false));
    let presenter = presenter_record_for(Host::Posix, "force", true, false);
    assert_eq!((presenter.software_render_mode, presenter.software_rendering), ("force", true));
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

/// Streaming into the scroll region below the fixture advances the grid and the tail row, but
/// leaves every fixture row's stamp, the history and the screen identity unchanged; rewriting a
/// fixture row with the same text is still a repaint the evidence reports.
#[test]
fn a_streaming_region_leaves_the_fixture_and_screen_unchanged() {
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    let mut pane = rest_fixture_pane(sentinel);
    let rest_start = park_snapshot(pane.grid(), None, crate::scenarios::REST_PATH_ROW);
    let tail_start = park_snapshot(pane.grid(), None, crate::scenarios::REST_TAIL_ROW);
    for second in 0..200 {
        pane.advance(format!("Thu Jan  1 00:00:{second:02} UTC 1970\r\n").as_bytes());
    }
    let rest_end = park_snapshot(pane.grid(), None, crate::scenarios::REST_PATH_ROW);
    let rest = rest_start.evidence(&rest_end, 30, None);
    assert!(rest.streamed_rows_advanced && rest.screen_unchanged && rest.fixture_unchanged);
    assert!(!rest.pointed_row_changed, "the path row is fixed");
    let tail_end = park_snapshot(pane.grid(), None, crate::scenarios::REST_TAIL_ROW);
    assert!(tail_start.evidence(&tail_end, 30, None).pointed_row_changed, "the tail row streams");
    // A same-value repaint of the path row restamps it, so the run is invalid.
    pane.advance(b"\x1b7\x1b[2;1H/tmp/scratch/workload/rest-target.txt\x1b8");
    let repainted = park_snapshot(pane.grid(), None, crate::scenarios::REST_PATH_ROW);
    assert!(!rest_start.evidence(&repainted, 30, None).fixture_unchanged);
}

/// A parked phase starts only on its displayed fixture with the pointer on its own cell: the rest
/// path cell and a streaming tail cell pass, while a changed fixture, a pointer that resolves to no
/// cell, a cell outside the grid and a tail cell outside the region each reject the run.
#[test]
fn park_entry_rejects_a_changed_fixture_and_an_out_of_grid_pointer() {
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    let pane = rest_fixture_pane(sentinel);
    let grid = pane.grid();
    let path_cell = (crate::scenarios::REST_PATH_ROW, 3);
    let tail_cell = (crate::scenarios::REST_TAIL_ROW, 3);
    assert_eq!(park_entry_problem("rest", grid, None, sentinel, Some(path_cell), path_cell), None);
    assert_eq!(park_entry_problem("tail", grid, None, sentinel, Some(tail_cell), tail_cell), None);
    let other_run = "PERF_DONE 0 fedcba9876543210";
    let changed = park_entry_problem("rest", grid, None, other_run, Some(path_cell), path_cell);
    assert!(changed.unwrap().contains("does not display its fixture"));
    let nowhere = park_entry_problem("rest", grid, None, sentinel, None, path_cell);
    assert!(nowhere.unwrap().contains("resolves to None"));
    let outside = (grid.rows + 5, 3);
    let beyond = park_entry_problem("tail", grid, None, sentinel, Some(outside), outside);
    assert!(beyond.unwrap().contains("outside the grid"));
    let fixed_tail = park_entry_problem("tail", grid, None, sentinel, Some(path_cell), path_cell);
    assert!(fixed_tail.unwrap().contains("not its target"));
    let mut repainted = rest_fixture_pane(sentinel);
    repainted.advance(b"\x1b[1;1Hhttps://example.com/other\x1b[K");
    let gone =
        park_entry_problem("rest", repainted.grid(), None, sentinel, Some(path_cell), path_cell);
    assert!(gone.unwrap().contains("does not display its fixture"));
}
