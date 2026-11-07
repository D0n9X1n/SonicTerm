//! Pins `wait_for`'s scan bookkeeping across polls: quiet and every other end of a wait report
//! rows still unscanned, on either screen; a newly switched screen is scanned and a saved one
//! resumed; a row or line too long for one scan is skipped without stalling; and the scan's
//! progress invariants hold over generated output, screen switches and resizes.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use sonicterm_cfg::config::McpControl;

use super::scan_tests::{emit, end_row, noise, scope, step_then_emit, Steps};
use super::ScanCursor;
use crate::app::mcp::tools_read::{
    scan_pending, wait_until, wait_until_observed, WaitCondition, WaitReason, WAIT_RECHECK,
};
use crate::app::mcp::{test_entry, McpShared};

/// Quiet must not fire while the scan still has a backlog. The burst is already written when the
/// wait starts, so output is quiet from the start; with `after_row` before it, the scan needs
/// several polls to reach `DONE` at its end, longer than the 60 ms quiet window.
#[test]
fn quiet_waits_for_the_scan_backlog() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(200, 5, 5_000);
    emit(&entry, b"$ run\r\n");
    let after = end_row(&entry);
    let mut burst: Vec<u8> = format!("{}\r\n", "x".repeat(199)).repeat(600).into_bytes();
    burst.extend_from_slice(b"DONE\r\n");
    emit(&entry, &burst);
    let condition = WaitCondition {
        regex: Some(Regex::new("DONE").unwrap()),
        after: Some(after),
        quiet: Some(Duration::from_millis(60)),
        watch_exit: false,
        timeout: Duration::from_secs(5),
        recheck: WAIT_RECHECK,
    };
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched, "quiet fired before the scan reached DONE");
    assert!(outcome.scan_complete);
}

/// A new wait with no `after_row` looks at the tail first, as `read_pane` does, never the whole
/// history: an old success line pushed far above the tail does not end a wait for the next one.
#[test]
fn a_first_look_reads_the_tail_not_the_whole_history() {
    let entry = test_entry(40, 5, 1_000);
    let mut history = b"READY\r\n".to_vec();
    history.extend(noise(400));
    emit(&entry, &history);
    let mut steps = Steps::new("READY", None);
    assert!(steps.run(&entry, 5).is_none(), "an old READY far above the tail ended a new wait");
    emit(&entry, b"READY\r\n");
    assert_eq!(steps.run(&entry, 5).map(|done| done.reason), Some(WaitReason::Matched));
}

/// A scan from an explicit `after_row` that finds only blank rows below it stays where it
/// started, so a line output later writes on those rows, at the cursor, is still read.
#[test]
fn a_scan_of_blank_rows_after_after_row_stays_put() {
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let after = end_row(&entry);
    let mut steps = Steps::new("BUILD_DONE", Some(after));
    let outcome = step_then_emit(&entry, &mut steps, b"BUILD_DONE\r\n");
    assert_eq!(outcome.map(|done| done.reason), Some(WaitReason::Matched));
}

/// A parser held from before the wait starts makes every first look `Busy`, so the output is never
/// inspected; quiet must not fire on output nobody checked, whatever the quiet window says.
#[test]
fn quiet_waits_for_an_inspection_a_busy_parser_deferred() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let after = end_row(&entry);
    emit(&entry, b"BUILD_DONE\r\n");
    let condition = WaitCondition {
        regex: Some(Regex::new("BUILD_DONE").unwrap()),
        after: Some(after),
        quiet: Some(Duration::from_millis(30)),
        watch_exit: false,
        timeout: Duration::from_secs(3),
        recheck: WAIT_RECHECK,
    };
    // Held before the wait starts, so no look can complete until it is released, long after the
    // 30 ms quiet window; an ungated quiet would end the wait while the holder still sleeps.
    let held = entry.parser.lock();
    let waiter = {
        let (shared, entry) = (Arc::clone(&shared), entry.clone());
        std::thread::spawn(move || wait_until(&shared, &entry, &condition, &scope(&shared)))
    };
    std::thread::sleep(Duration::from_millis(300));
    drop(held);
    let outcome = waiter.join().unwrap();
    assert_eq!(outcome.reason, WaitReason::Matched, "quiet fired on uninspected output");
}

/// The pending rule itself, which every end of a wait consults: with a regex, a budget stop on
/// either screen or a generation never inspected (a busy parser, a later batch, or no look yet)
/// leaves rows unscanned; without a regex nothing is scanned, so nothing is pending.
#[test]
fn scan_pending_covers_backlog_and_uninspected_generations() {
    assert!(!scan_pending(true, false, Some(4), 4), "inspected and no backlog: complete");
    assert!(scan_pending(true, true, Some(4), 4), "a screen's backlog is pending");
    assert!(scan_pending(true, false, Some(3), 4), "a later generation is pending");
    assert!(scan_pending(true, false, None, 0), "no look yet is pending");
    assert!(!scan_pending(false, true, None, 4), "a wait without a regex scans nothing");
}
/// A process that exits before its output was inspected ends the wait with the scan reported
/// incomplete, not as proof that the output held no match.
#[test]
fn an_exit_before_any_inspection_reports_the_scan_incomplete() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 1_000);
    let _ = entry.exit_status.set(sonicterm_types::PaneExitStatus::Code(0));
    let condition = WaitCondition {
        regex: Some(Regex::new("NEVER").unwrap()),
        after: None,
        quiet: None,
        watch_exit: true,
        timeout: Duration::from_secs(3),
        recheck: WAIT_RECHECK,
    };
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Exited);
    assert!(!outcome.scan_complete, "the exit claimed a scan that never ran");
}

/// A wait whose timeout passes while the scan still has a backlog says so: `scan_complete` is
/// false, so the timeout is not proof that the unscanned rows held no match. The backlog is
/// written before the wait and bounded by `after_row`, so no thread timing decides the result:
/// 6,500 full rows need about 40 scans, far more than a 250 ms wait polls.
#[test]
fn a_timeout_mid_backlog_reports_the_scan_incomplete() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(200, 5, 50_000);
    emit(&entry, b"$ run\r\n");
    let after = end_row(&entry);
    emit(&entry, &format!("{}\r\n", "x".repeat(199)).repeat(6_500).into_bytes());
    let condition = WaitCondition {
        regex: Some(Regex::new("NEVER").unwrap()),
        after: Some(after),
        quiet: None,
        watch_exit: false,
        timeout: Duration::from_millis(250),
        recheck: WAIT_RECHECK,
    };
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Timeout);
    assert!(!outcome.scan_complete, "the timeout claimed a scan it had not finished");
}

/// A match found while rows are still unscanned says so: a `READY` near the start of a large
/// backlog ends the wait at once, with `scan_complete` false.
#[test]
fn an_early_match_reports_the_rest_unscanned() {
    let entry = test_entry(200, 5, 50_000);
    emit(&entry, b"$ run\r\n");
    let after = end_row(&entry);
    let mut burst = b"READY\r\n".to_vec();
    burst.extend(format!("{}\r\n", "x".repeat(199)).repeat(600).into_bytes());
    emit(&entry, &burst);
    let mut steps = Steps::new("READY", Some(after));
    let outcome = steps.step(&entry).expect("READY is in the first scan");
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert!(!outcome.scan_complete, "the match claimed rows the scan never reached");
}

/// A wait with no `after_row` whose first look is done, on the primary screen, before `burst`
/// arrives in one batch; returned with the wait's outcome and the pane, quiet after 60 ms.
fn wait_through(burst: &[u8], timeout_ms: u64) -> crate::app::mcp::tools_read::WaitOutcome {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(200, 5, 50_000);
    emit(&entry, b"$ run\r\n");
    let condition = WaitCondition {
        regex: Some(Regex::new("READY").unwrap()),
        after: None,
        quiet: Some(Duration::from_millis(60)),
        watch_exit: false,
        timeout: Duration::from_millis(timeout_ms),
        recheck: WAIT_RECHECK,
    };
    // The wait's own first look reads the tail; only then, before its next poll, is the burst
    // written, so the wait's real state sees the primary screen first and the burst after it.
    let (looked_tx, looked_rx) = std::sync::mpsc::channel();
    let (written_tx, written_rx) = std::sync::mpsc::channel::<()>();
    let waiter = {
        let (shared, entry) = (Arc::clone(&shared), entry.clone());
        std::thread::spawn(move || {
            wait_until_observed(&shared, &entry, &condition, &scope(&shared), || {
                looked_tx.send(()).unwrap();
                written_rx.recv().unwrap();
            })
        })
    };
    looked_rx.recv().unwrap();
    emit(&entry, burst);
    written_tx.send(()).unwrap();
    waiter.join().unwrap()
}

/// Output on the primary screen just before a switch, in the same batch, is not forgotten: the
/// saved primary screen stays pending, so the wait is never quiet or complete while READY is
/// unscanned there, and never ends as `stale_row` without an `after_row`.
#[test]
fn output_written_just_before_a_switch_keeps_the_primary_pending() {
    let outcome = wait_through(b"READY\r\n\x1b[?1049hfullscreen\r\n", 400);
    assert!(!outcome.stale_row);
    assert_ne!(outcome.reason, WaitReason::Quiet, "quiet over the saved primary's READY");
    assert!(!outcome.scan_complete, "the saved primary's READY was never scanned");

    // A backlog several scans long on the primary, then the switch: the same holds.
    let mut burst = format!("{}\r\n", "x".repeat(199)).repeat(600).into_bytes();
    burst.extend_from_slice(b"READY\r\n\x1b[?1049h");
    let outcome = wait_through(&burst, 400);
    assert!(!outcome.stale_row);
    assert_ne!(outcome.reason, WaitReason::Quiet, "quiet over the primary's backlog");
    assert!(!outcome.scan_complete);
}

/// The saved primary screen's pending rows are scanned once it is active again: READY written
/// before the switch is found after the program leaves the alternate screen.
#[test]
fn the_saved_primary_is_scanned_on_return() {
    let entry = test_entry(200, 5, 50_000);
    emit(&entry, b"$ run\r\n");
    let mut steps = Steps::new("READY", None);
    assert!(steps.step(&entry).is_none(), "the first look reads the tail");
    emit(&entry, b"READY\r\n\x1b[?1049hfullscreen\r\n");
    assert!(steps.step(&entry).is_none(), "the alternate screen has no READY");
    assert!(steps.cursor.backlog(false), "the saved primary screen is pending");
    emit(&entry, b"\x1b[?1049l");
    let outcome = steps.run(&entry, 5).expect("READY on the primary screen");
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// Leaving the alternate screen discards it, so rows its scan had not reached are reported lost
/// and never waited for: the wait settles as quiet instead of rescanning until its timeout, and
/// a later alternate screen is read from its own start.
#[test]
fn a_discarded_alternate_backlog_is_reported_not_awaited() {
    let entry = test_entry(200, 200, 50_000);
    emit(&entry, b"$ run\r\n");
    let mut steps = Steps::new("READY", None);
    assert!(steps.step(&entry).is_none(), "the first look reads the tail");
    let mut burst = b"\x1b[?1049h".to_vec();
    burst.extend(format!("{}\r\n", "x".repeat(199)).repeat(190).into_bytes());
    emit(&entry, &burst);
    assert!(steps.step(&entry).is_none(), "one scan of the alternate screen");
    assert!(steps.cursor.backlog(true), "the alternate screen's scan stopped at its budget");
    emit(&entry, b"\x1b[?1049l");
    assert!(steps.step(&entry).is_none(), "the primary screen has nothing new");
    assert!(!steps.cursor.backlog(true), "the discarded screen's backlog is not awaited");
    assert!(steps.flags.missed, "its unscanned rows are reported lost");
    assert_eq!(steps.cursor.get(true), None, "a later alternate screen starts afresh");
    // The primary screen's own rows (200 rows of 200 cells, more than one scan) drain, and then
    // nothing is pending, so a quiet wait would settle instead of running to its timeout.
    assert!(steps.run(&entry, 5).is_none());
    assert!(!steps.cursor.any_backlog(), "pending forever: {:?}", steps.cursor);
}
/// With nothing left to scan, a quiet wait still ends `quiet` and reports the scan complete.
#[test]
fn a_quiet_wait_with_no_backlog_is_complete() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"$ idle\r\n");
    let condition = WaitCondition {
        regex: Some(Regex::new("NEVER").unwrap()),
        after: None,
        quiet: Some(Duration::from_millis(80)),
        watch_exit: false,
        timeout: Duration::from_secs(5),
        recheck: WAIT_RECHECK,
    };
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Quiet);
    assert!(outcome.scan_complete);
}

/// Output written on a newly switched screen before the wait looks again is still scanned: the
/// first look at a screen reads it from its oldest retained row, and the switch is reported.
#[test]
fn output_on_a_newly_switched_screen_is_found() {
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"$ vim\r\n");
    let mut steps = Steps::new("READY", None);
    assert!(steps.step(&entry).is_none());
    emit(&entry, b"\x1b[?1049hline1\r\nREADY\r\nline3\r\n");
    let outcome = steps.run(&entry, 4).expect("READY on the alternate screen was found");
    assert_eq!(outcome.matched.unwrap().0, "READY");
    assert!(steps.flags.screen_changed, "the switch was not reported");
}

/// Leaving the alternate screen resumes the primary screen's own cursor: a match written after the
/// wait began is found, and primary text from before it is not rescanned.
#[test]
fn returning_to_the_primary_screen_resumes_its_cursor() {
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"OLD_MATCH\r\n$ vim\r\n");
    let mut steps = Steps::new("MATCH", None);
    // The first look reads the tail, which already holds OLD_MATCH: that is the existing contract.
    let _ = steps.step(&entry);
    let mut steps = Steps::new("NEW_MATCH|OLD_MATCH_AGAIN", None);
    assert!(steps.step(&entry).is_none());
    emit(&entry, b"\x1b[?1049hfullscreen\r\n");
    assert!(steps.step(&entry).is_none());
    emit(&entry, b"\x1b[?1049lNEW_MATCH\r\n");
    let outcome = steps.run(&entry, 4).expect("the new primary output was found");
    assert_eq!(outcome.matched.unwrap().0, "NEW_MATCH");
}

/// A row wider than a whole scan's budget, with nothing pending, is reported cut and skipped: the
/// scan advances past it instead of returning the same cursor forever.
#[test]
fn a_row_wider_than_the_whole_budget_is_cut_and_skipped() {
    let entry = test_entry(200, 5, 100);
    emit(&entry, b"$ run\r\n");
    emit(&entry, &[b"x".repeat(150), b"\r\nnext\r\n".to_vec()].concat());
    let guard = entry.parser.lock();
    let grid = guard.grid();
    let wide = (0..grid.scrollback_len() as u64 + u64::from(grid.rows))
        .find(|&abs| {
            grid.row_at_abs(abs)
                .is_some_and(|row| row.iter().filter(|cell| cell.ch == 'x').count() > 100)
        })
        .expect("the wide row");
    let start = ScanCursor::at(grid.row_token(wide));
    // A budget of 100 cells is smaller than one 200-cell row.
    let step = super::scan_within(grid, start, 100);
    let next = step.next.expect("a cursor");
    assert!(next.resume.line > start.resume.line, "the scan did not advance past the wide row");
    assert!(step.lines.first().is_some_and(|line| line.cut), "the wide row was not reported cut");
}

/// Rows each wider than the budget still stop the scan when the budget runs out, so one scan's
/// work under the parser lock stays bounded, and the next scan continues past them.
#[test]
fn rows_wider_than_the_budget_still_stop_at_it() {
    let entry = test_entry(200, 5, 2_000);
    emit(&entry, &format!("{}\r\n", "x".repeat(199)).repeat(1_000).into_bytes());
    let guard = entry.parser.lock();
    let grid = guard.grid();
    let start = ScanCursor::at(grid.row_token(0));
    let step = super::scan_within(grid, start, 1);
    assert!(step.more, "a budget of one cell must stop the scan");
    assert!(step.lines.len() <= 1, "one cell bought {} cut rows", step.lines.len());
    let next = step.next.expect("a cursor");
    assert!(next.resume.line > start.resume.line, "the stop still advanced");
}

/// A logical line past the 64 KiB byte cap (24,000 three-byte characters) but inside the cell
/// budget is cut, and the scan resumes past its last row, never on it, so it is not reread on
/// every poll; a `READY` written after it is still found, whether the cut line was closed by a
/// newline or is still open.
#[test]
fn a_byte_cut_line_is_never_a_resume_point() {
    for closed in [true, false] {
        let entry = test_entry(200, 5, 1_000);
        emit(&entry, b"$ run\r\n");
        let after = end_row(&entry);
        let mut long = "\u{20ac}".repeat(24_000).into_bytes();
        if closed {
            // When: `closed`, a newline ends the cut line before the scan sees it.
            long.extend_from_slice(b"\r\n");
        }
        emit(&entry, &long);
        let mut steps = Steps::new("READY", Some(after));
        assert!(steps.step(&entry).is_none(), "the cut line is not matched by its prefix");
        assert!(steps.flags.truncated, "the cut line is reported");
        let resume = steps.cursor.get(false).expect("a cursor").resume;
        let guard = entry.parser.lock();
        let grid = guard.grid();
        // The cut line's last row: the newest row holding any of its characters.
        let cut_last = (0..grid.scrollback_len() as u64 + u64::from(grid.rows))
            .filter(|&abs| {
                grid.row_at_abs(abs).is_some_and(|row| row.iter().any(|cell| cell.ch == '\u{20ac}'))
            })
            .map(|abs| grid.row_token(abs).line)
            .max()
            .expect("the cut line is retained");
        drop(guard);
        assert!(resume.line > cut_last, "closed: {closed}: the scan resumes on the cut line");
        emit(&entry, b"\r\nREADY\r\n");
        let outcome = steps.run(&entry, 5).expect("READY after the cut line");
        assert_eq!(outcome.reason, WaitReason::Matched, "closed: {closed}");
    }
}

/// The progress invariant, over seeded random event sequences with resizes, multibyte lines past
/// the byte cap, and budgets from one cell to a full production scan: a scan never moves its
/// resume point backward (across a resize, which reflows rows, the cursor restarts as `inspect`
/// would), a scan that stopped at its budget always moves it forward, it never resumes on a cut
/// line, and a full scan that found no text stays where it started, where output goes next.
#[test]
fn every_scan_advances_or_reaches_the_newest_row() {
    // A small linear congruential generator, so the sequence is fixed and needs no dependency.
    let mut seed: u64 = 0x5eed_1792;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for round in 0..40 {
        let entry = test_entry(60, 6, 400);
        let mut cursor = {
            let guard = entry.parser.lock();
            ScanCursor::at(guard.grid().row_token(0))
        };
        for _ in 0..30 {
            let mut resized = false;
            let event: Vec<u8> = match next(10) {
                0 => format!("line {}\r\n", next(1_000)).into_bytes(),
                1 => "w".repeat(next(200) as usize + 1).into_bytes(),
                2 => b"\x1b[3J".to_vec(),
                3 => b"\x1b[?1049h".to_vec(),
                4 => b"\x1b[?1049l".to_vec(),
                5 => b"\x1b[2J\x1b[H".to_vec(),
                6 => b"\r\n".repeat(next(20) as usize),
                7 => "\u{20ac}".repeat(22_000 + next(4_000) as usize).into_bytes(),
                8 => {
                    let (cols, rows) = (next(150) as u16 + 20, next(30) as u16 + 4);
                    entry.parser.lock().resize(cols, rows);
                    resized = true;
                    Vec::new()
                }
                _ => b"\x1b[1;1Hrewritten".to_vec(),
            };
            emit(&entry, &event);
            let guard = entry.parser.lock();
            let grid = guard.grid();
            if resized || cursor.resume.alt != grid.is_alt() {
                // A resize reflows rows and a switch changes screens; both restart the cursor.
                cursor = ScanCursor::at(grid.row_token(0));
            }
            let budget = match next(3) {
                0 => 1,
                1 => next(400) as usize + 1,
                _ => super::super::read::MAX_CELLS_PER_READ,
            };
            let from = cursor.resume.line.max(grid.row_token(0).line);
            let step = super::scan_within(grid, cursor, budget);
            let next_cursor = step.next.expect("same screen, so a cursor");
            // Never backward; a scan stopped at its budget must move strictly forward, or the next
            // poll would rescan the same rows forever.
            assert!(next_cursor.resume.line >= from, "round {round}: the scan moved backward");
            if step.more {
                assert!(
                    next_cursor.resume.line > from,
                    "round {round}: a budget stop did not advance from {from} (budget {budget})"
                );
            }
            // A reread point is a line that can still match, never a cut one.
            let reread = step.lines.iter().find(|line| line.first_row == next_cursor.resume);
            assert!(!reread.is_some_and(|line| line.cut), "round {round}: resumed on a cut line");
            let textless = step.lines.iter().all(|line| line.text.is_empty() && !line.cut);
            if !step.more && textless {
                assert_eq!(next_cursor.resume.line, from, "round {round}: a blank scan moved on");
            }
            cursor = next_cursor;
        }
    }
}

/// The whole wait, through `inspect`, over generated history, screen switches and resizes made
/// before a marker is written: a `READY` written after a large backlog is matched unless the wait
/// reports the loss (`missed_output`) or a cut line (`truncated`); it is never silently skipped.
#[test]
fn a_marker_after_generated_history_is_found_or_reported() {
    let mut seed: u64 = 0x1791;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for round in 0..16 {
        let entry = test_entry(80, 8, 3_000);
        let mut steps = Steps::new("^READY$", None);
        for _ in 0..12 {
            let event: Vec<u8> = match next(6) {
                0 => noise(next(50) as usize + 1),
                1 => b"\x1b[?1049h".to_vec(),
                2 => b"\x1b[?1049l".to_vec(),
                3 => {
                    entry.parser.lock().resize(next(100) as u16 + 20, next(20) as u16 + 4);
                    Vec::new()
                }
                4 => "\u{20ac}".repeat(next(30_000) as usize).into_bytes(),
                _ => b"\r\n".to_vec(),
            };
            emit(&entry, &event);
            assert!(steps.step(&entry).is_none(), "round {round}: matched before the marker");
        }
        // The marker goes on the primary screen after a backlog several scans long.
        let mut tail = b"\x1b[?1049l\r\n".to_vec();
        tail.extend(format!("{}\r\n", "y".repeat(79)).repeat(next(1_500) as usize).into_bytes());
        tail.extend_from_slice(b"READY\r\n");
        emit(&entry, &tail);
        let found = steps.run(&entry, 60).map(|done| done.reason) == Some(WaitReason::Matched);
        assert!(
            found || steps.flags.missed || steps.flags.truncated,
            "round {round}: READY was skipped with nothing reported"
        );
    }
}
