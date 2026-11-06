//! Pins `wait_for`'s forward scan: a match pushed past the tail between two polls is found, a
//! backlog larger than one scan is finished without new output, logical lines are matched only
//! whole and from their own start, the newest line (a prompt with no newline) can match, and rows
//! dropped before they were scanned are reported rather than silently timing out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use sonicterm_cfg::config::McpControl;
use sonicterm_grid::grid::RowToken;

use super::{scan_from, ScanCursor, ScanCursors};
use crate::app::mcp::read::{read_pane, ReadLimits, ReadMode};
use crate::app::mcp::tools_read::{
    inspect, wait_until, ScanFlags, TailCheck, WaitCondition, WaitOutcome, WaitReason,
};
use crate::app::mcp::{test_entry, CallScope, McpShared, PaneEntry};

/// A call admitted now on `shared`, for a connection that stays open.
pub(super) fn scope(shared: &McpShared) -> CallScope {
    CallScope::admit(shared, &Arc::new(AtomicBool::new(false)))
}

/// Feed `bytes` to the pane and publish a new output generation, as the VT worker does.
pub(super) fn emit(entry: &PaneEntry, bytes: &[u8]) {
    entry.parser.lock().advance(bytes);
    entry.output_generation.fetch_add(1, Ordering::Release);
}

/// A wait on `pattern` for lines after `after`, bounded by `timeout_ms`.
pub(super) fn regex_wait(pattern: &str, after: Option<RowToken>, timeout_ms: u64) -> WaitCondition {
    WaitCondition {
        regex: Some(Regex::new(pattern).unwrap()),
        after,
        quiet: None,
        watch_exit: false,
        timeout: Duration::from_millis(timeout_ms),
    }
}

/// `count` numbered noise lines, CRLF-terminated.
pub(super) fn noise(count: usize) -> Vec<u8> {
    (0..count).flat_map(|index| format!("log line {index}\r\n").into_bytes()).collect()
}

/// The last row holding text: the `end_row` an agent passes back as `after_row`.
pub(super) fn end_row(entry: &PaneEntry) -> RowToken {
    let snapshot = read_pane(&entry.parser, ReadMode::Tail, ReadLimits::clamped(None, None));
    snapshot.lines.last().expect("the pane has text").row
}

/// A wait driven one inspection at a time, as `wait_until`'s loop drives it, so a test decides
/// exactly what output each inspection sees instead of racing a polling thread.
pub(super) struct Steps {
    /// The pattern.
    pub(super) regex: Regex,
    /// The caller's `after_row`.
    pub(super) after: Option<RowToken>,
    /// Where the next forward scan starts, and whether it stopped at its budget, on each screen.
    pub(super) cursor: ScanCursors,
    /// What the wait could not inspect.
    pub(super) flags: ScanFlags,
}

impl Steps {
    /// A wait on `pattern` for lines after `after`.
    pub(super) fn new(pattern: &str, after: Option<RowToken>) -> Self {
        Self {
            regex: Regex::new(pattern).unwrap(),
            after,
            cursor: ScanCursors::default(),
            flags: ScanFlags::default(),
        }
    }

    /// One inspection of `entry`; the outcome if it ended the wait.
    pub(super) fn step(&mut self, entry: &PaneEntry) -> Option<WaitOutcome> {
        let check = inspect(entry, &self.regex, self.after, &mut self.cursor, &mut self.flags);
        match check {
            TailCheck::Done(outcome) => Some(outcome),
            TailCheck::Busy | TailCheck::NoMatch => None,
        }
    }

    /// Inspect until the wait ends or `limit` inspections pass with no match.
    pub(super) fn run(&mut self, entry: &PaneEntry, limit: usize) -> Option<WaitOutcome> {
        (0..limit).find_map(|_| self.step(entry))
    }
}

/// Inspect once, then emit `bytes`, then inspect until done: the first inspection certainly
/// happens before the output, which `wait_until` on a thread cannot promise.
pub(super) fn step_then_emit(
    entry: &PaneEntry,
    steps: &mut Steps,
    bytes: &[u8],
) -> Option<WaitOutcome> {
    assert!(steps.step(entry).is_none(), "the first inspection matched before the output");
    emit(entry, bytes);
    steps.run(entry, 20)
}

/// The reported bug: a matching line followed by 300 more lines, all between two polls, leaves
/// the tail; the forward scan still finds it.
#[test]
fn a_match_pushed_past_the_tail_between_polls_is_found() {
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let mut burst = b"BUILD_FAILED\r\n".to_vec();
    burst.extend(noise(300));
    let mut steps = Steps::new("BUILD_FAILED", None);
    let outcome = step_then_emit(&entry, &mut steps, &burst).expect("matched");
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert_eq!(outcome.matched.unwrap().0, "BUILD_FAILED");
    assert!(!steps.flags.missed);
}

/// A match beyond the first scan's cell budget is found by a later scan with no new output in
/// between: 400 full-width rows (80,000 cells) precede it, more than one scan's 32,768.
#[test]
fn a_backlog_larger_than_one_scan_is_finished_without_new_output() {
    let entry = test_entry(200, 5, 5_000);
    emit(&entry, b"$ run\r\n");
    let mut burst: Vec<u8> = format!("{}\r\n", "x".repeat(199)).repeat(400).into_bytes();
    burst.extend_from_slice(b"THE_MATCH\r\n");
    burst.extend(noise(200));
    let mut steps = Steps::new("THE_MATCH", None);
    assert!(steps.step(&entry).is_none());
    emit(&entry, &burst);
    // The first scan after the output stops at its budget before the match.
    assert!(steps.step(&entry).is_none(), "one scan reached a match past its budget");
    assert!(steps.cursor.any_backlog(), "the scan stopped at its budget");
    // Later inspections continue with no new output and find it.
    let outcome = steps.run(&entry, 10).expect("the backlog was finished");
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert!(!steps.flags.missed && !steps.flags.truncated);
}

/// A logical line that straddles one scan's budget is reread whole by the next scan and matched
/// as one line.
#[test]
fn a_line_straddling_the_scan_budget_is_matched_whole() {
    let entry = test_entry(200, 5, 5_000);
    emit(&entry, b"$ run\r\n");
    // 160 rows of 200 cells fill 32,000 cells; the next line wraps over three rows past 32,768.
    let mut burst: Vec<u8> = format!("{}\r\n", "x".repeat(199)).repeat(160).into_bytes();
    burst.extend(format!("{}SPLIT_ACROSS\r\n", "y".repeat(500)).into_bytes());
    burst.extend(noise(50));
    let mut steps = Steps::new("^y{500}SPLIT_ACROSS$", None);
    let outcome = step_then_emit(&entry, &mut steps, &burst).expect("matched");
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// A pane with no text yet: output written anywhere on the screen is scanned, not only below its
/// bottom row.
#[test]
fn a_wait_on_a_blank_pane_sees_the_first_output() {
    let entry = test_entry(40, 5, 100);
    let mut steps = Steps::new("MATCH", None);
    let outcome = step_then_emit(&entry, &mut steps, b"MATCH\r\n").expect("matched");
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// A prompt on the bottom row with no newline after it can match: the newest line is examined as
/// it stands, or an agent waiting for a prompt would wait forever.
#[test]
fn a_prompt_with_no_newline_after_it_matches() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 2, 100);
    emit(&entry, b"old\r\nREADY");
    let after = RowToken { line: 0, alt: false };
    let outcome =
        wait_until(&shared, &entry, &regex_wait("READY", Some(after), 1_000), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// A continuation row of a line that began at or before `after_row` is part of that line, never a
/// line of its own, so an anchored pattern cannot match its suffix.
#[test]
fn a_wrapped_suffix_of_an_excluded_line_is_not_a_line() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(5, 5, 100);
    emit(&entry, b"ABCDE12345\r\n");
    let after = RowToken { line: 0, alt: false };
    let outcome =
        wait_until(&shared, &entry, &regex_wait("^12345$", Some(after), 300), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Timeout);
}

/// A wrapped line whose first rows are blank keeps those blanks: five spaces then `MATCH` is not
/// the line `MATCH`.
#[test]
fn leading_blank_rows_of_a_wrapped_line_are_kept() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(5, 5, 100);
    emit(&entry, b"x\r\n");
    let after = end_row(&entry);
    emit(&entry, b"     MATCH\r\n$ ");
    let pattern = "^MATCH$";
    let outcome =
        wait_until(&shared, &entry, &regex_wait(pattern, Some(after), 300), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Timeout, "the blank prefix was dropped");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("^ {5}MATCH$", Some(after), 300), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// A logical line longer than a scan can copy is never matched by its prefix: an end anchor or a
/// word boundary that holds only at the cut must not match, and the wait reports `truncated`.
#[test]
fn a_cut_line_is_reported_and_never_matched_by_its_prefix() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(200, 5, 5_000);
    emit(&entry, b"$ run\r\n");
    let after = end_row(&entry);
    let mut line = "a".repeat(64 * 1024 - 1).into_bytes();
    line.extend_from_slice(b"ZNOT_END\r\n$ \r\n");
    emit(&entry, &line);
    for pattern in ["Z$", "Z\\b"] {
        let condition = regex_wait(pattern, Some(after), 400);
        let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
        assert_eq!(outcome.reason, WaitReason::Timeout, "{pattern} matched a cut prefix");
        assert!(outcome.truncated, "{pattern}: the cut line must be reported");
    }
}

/// With an explicit `after_row`, every eligible retained row is scanned from the first look, even
/// when it all arrived before the wait began.
#[test]
fn an_explicit_after_row_scans_everything_written_before_the_first_look() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let after = end_row(&entry);
    let mut burst = b"BUILD_FAILED\r\n".to_vec();
    burst.extend(noise(300));
    emit(&entry, &burst);
    let condition = regex_wait("BUILD_FAILED", Some(after), 3_000);
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// The same with the parser held during the first look: the busy inspection retries.
#[test]
fn an_explicit_after_row_scan_survives_a_busy_first_look() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let after = end_row(&entry);
    let mut burst = b"BUILD_FAILED\r\n".to_vec();
    burst.extend(noise(300));
    emit(&entry, &burst);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder = {
        let parser = Arc::clone(&entry.parser);
        std::thread::spawn(move || {
            let _held = parser.lock();
            held_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        })
    };
    held_rx.recv().unwrap();
    let condition = regex_wait("BUILD_FAILED", Some(after), 3_000);
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    holder.join().unwrap();
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// An explicit `after_row` from the primary screen stays bound to it: a switch to the alternate
/// screen ends the wait as `stale_row` instead of searching unrelated content.
#[test]
fn an_explicit_after_row_is_stale_once_the_screen_switches() {
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ vim\r\n");
    let after = end_row(&entry);
    let mut steps = Steps::new("MATCH_HERE", Some(after));
    let outcome = step_then_emit(&entry, &mut steps, b"\x1b[?1049hMATCH_HERE\r\n").expect("ended");
    assert!(outcome.stale_row, "a primary-screen after_row must not match alternate text");
}

/// A match that scrolled out of a small scrollback before it was scanned is reported missed.
#[test]
fn rows_dropped_before_they_were_scanned_are_reported_missed() {
    let entry = test_entry(40, 5, 20);
    emit(&entry, b"$ make\r\n");
    let mut burst = b"BUILD_FAILED\r\n".to_vec();
    burst.extend(noise(300));
    let mut steps = Steps::new("BUILD_FAILED", None);
    assert!(step_then_emit(&entry, &mut steps, &burst).is_none(), "the match was dropped");
    assert!(steps.flags.missed, "dropped unscanned rows must be reported");
}

/// The same with scrollback disabled: rows scroll off unretained and are still reported.
#[test]
fn rows_lost_with_scrollback_disabled_are_reported_missed() {
    let entry = test_entry(40, 5, 0);
    emit(&entry, b"$ make\r\n");
    let mut burst = b"BUILD_FAILED\r\n".to_vec();
    burst.extend(noise(50));
    let mut steps = Steps::new("BUILD_FAILED", None);
    assert!(step_then_emit(&entry, &mut steps, &burst).is_none());
    assert!(steps.flags.missed);
}

/// Clearing history (ED 3) drops rows the scan had not reached, which is reported, and the flag
/// stays set for the rest of the wait.
#[test]
fn clearing_history_before_rows_were_scanned_is_reported_missed() {
    let entry = test_entry(40, 5, 1_000);
    emit(&entry, b"$ make\r\n");
    let mut burst = noise(100);
    burst.extend_from_slice(b"\x1b[3J");
    burst.extend(noise(3));
    let mut steps = Steps::new("NEVER", None);
    assert!(step_then_emit(&entry, &mut steps, &burst).is_none());
    assert!(steps.flags.missed, "rows cleared before the scan reached them are lost");
    emit(&entry, &noise(2));
    assert!(steps.run(&entry, 3).is_none());
    assert!(steps.flags.missed, "the flag stays set");
}

/// Eviction of rows that were already scanned is not loss: small bursts, each inspected before the
/// next, push old scanned rows out of a small scrollback without setting `missed`.
#[test]
fn eviction_of_already_scanned_rows_is_not_reported_missed() {
    let entry = test_entry(40, 5, 20);
    emit(&entry, &noise(30));
    let mut steps = Steps::new("NEVER", None);
    assert!(steps.step(&entry).is_none());
    for _ in 0..12 {
        emit(&entry, &noise(3));
        assert!(steps.step(&entry).is_none());
    }
    assert!(!steps.flags.missed, "only already-scanned rows left history");
}

/// A line written in two chunks, the first inspected before the second arrives, is matched whole:
/// the cursor stays on the open line instead of passing the blank rows below it.
#[test]
fn a_line_written_in_two_chunks_is_matched_whole() {
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"$ run\r\n");
    let mut steps = Steps::new("BUILD_FAILED", None);
    assert!(steps.step(&entry).is_none());
    emit(&entry, b"BUILD_");
    assert!(steps.step(&entry).is_none());
    emit(&entry, b"FAILED\r\n");
    let outcome = steps.run(&entry, 3).expect("the extended line matched");
    assert_eq!(outcome.matched.unwrap().0, "BUILD_FAILED");
    assert!(!steps.flags.missed && !steps.flags.truncated);
}

/// Blank lines are never matched, whether the first look reads the tail (no `after_row`) or the
/// scan reads them (with one), so `^$` gives the same answer either way.
#[test]
fn blank_lines_are_never_matched_on_either_path() {
    let entry = test_entry(40, 6, 100);
    emit(&entry, b"old\r\n\r\nnext\r\n");
    let mut tail_first = Steps::new("^$", None);
    assert!(tail_first.run(&entry, 3).is_none(), "the tail path matched a blank line");
    let after = RowToken { line: 0, alt: false };
    let mut scan_first = Steps::new("^$", Some(after));
    assert!(scan_first.run(&entry, 3).is_none(), "the scan path matched a blank line");
}

/// A line whose inspected prefix is blank but which outgrew the scan is still reported
/// `truncated`: the blank-line rule never hides a cut line.
#[test]
fn a_cut_line_with_a_blank_prefix_is_still_reported() {
    let entry = test_entry(200, 5, 1_000);
    emit(&entry, b"$ run\r\n");
    let after = end_row(&entry);
    let mut line = " ".repeat(40_000).into_bytes();
    line.extend_from_slice(b"x\r\nnext\r\n");
    emit(&entry, &line);
    let mut steps = Steps::new("ABSENT", Some(after));
    assert!(steps.run(&entry, 10).is_none());
    assert!(steps.flags.truncated, "the over-budget blank-prefix line was not reported");
}

/// One scan step directly: a cursor below the retained range reports `missed`; a cursor on the
/// other screen asks for a restart; a cursor at the oldest row misses nothing.
#[test]
fn a_scan_step_reports_loss_and_screen_changes() {
    let entry = test_entry(40, 5, 10);
    emit(&entry, &noise(60));
    let guard = entry.parser.lock();
    let grid = guard.grid();
    let oldest = grid.row_token(0);
    let below = RowToken { line: oldest.line.saturating_sub(5), alt: oldest.alt };
    assert!(scan_from(grid, ScanCursor::at(below)).missed);
    let other_screen = RowToken { line: oldest.line, alt: !oldest.alt };
    assert_eq!(scan_from(grid, ScanCursor::at(other_screen)).next, None);
    assert!(!scan_from(grid, ScanCursor::at(oldest)).missed);
}

/// Clearing history that held only rows already examined is not loss, even when the wait's resume
/// point sits on that history to reread an open line: only unexamined rows count.
#[test]
fn clearing_only_examined_history_is_not_reported_missed() {
    let entry = test_entry(40, 2, 100);
    emit(&entry, b"done\r\n\r\n");
    let mut steps = Steps::new("ABSENT", None);
    assert!(steps.run(&entry, 3).is_none());
    emit(&entry, b"\x1b[3J");
    assert!(steps.run(&entry, 3).is_none());
    assert!(!steps.flags.missed, "only examined rows were cleared");
}

/// The same for row-limit eviction: rows already examined that scroll out of a small scrollback
/// are not loss, while the line still being written stays matchable.
#[test]
fn evicting_only_examined_rows_keeps_the_open_line_matchable() {
    let entry = test_entry(40, 3, 2);
    emit(&entry, b"one\r\ntwo\r\nthree\r\nfour\r\nREADY_");
    let mut steps = Steps::new("READY_NOW", None);
    assert!(steps.run(&entry, 3).is_none());
    emit(&entry, b"NOW");
    let outcome = steps.run(&entry, 3).expect("the extended open line matched");
    assert_eq!(outcome.matched.unwrap().0, "READY_NOW");
    assert!(!steps.flags.missed);
}

/// Output written onto rows that were blank when inspected, then scrolled out before the next
/// inspection, is reported missed: a blank row below the cursor is not settled, so it never
/// counts as examined. Both a small scrollback and none at all.
#[test]
fn output_written_onto_inspected_blank_rows_then_lost_is_reported_missed() {
    for scrollback in [0, 2] {
        let entry = test_entry(40, 5, scrollback);
        let mut steps = Steps::new("MATCH", None);
        assert!(steps.step(&entry).is_none(), "the blank pane matched");
        emit(&entry, &[b"MATCH\r\n".to_vec(), b"noise\r\n".repeat(7)].concat());
        assert!(steps.run(&entry, 3).is_none(), "scrollback {scrollback}: MATCH was retained");
        assert!(steps.flags.missed, "scrollback {scrollback}: the lost MATCH was not reported");
    }
}

/// A scan that stops at its budget keeps the coverage established before it. The cursor sits 250
/// blank rows below the only text, so the wait's coverage already reaches it, while a rescan
/// from the text stops at its budget around row 163; scrolling 171 rows away then drops only
/// rows already covered, which is not loss.
#[test]
fn a_budget_stop_keeps_coverage_already_established() {
    let entry = test_entry(200, 300, 0);
    emit(&entry, &[b"x".to_vec(), b"\r\n".repeat(250)].concat());
    let mut steps = Steps::new("ABSENT", None);
    assert!(steps.step(&entry).is_none(), "the tail look");
    let settled = steps.cursor.primary.expect("a cursor").frontier;
    assert!(steps.step(&entry).is_none(), "a rescan that stops at its budget");
    assert!(steps.cursor.any_backlog(), "the rescan stopped at its budget");
    assert!(
        steps.cursor.primary.expect("a cursor").frontier >= settled,
        "a budget stop lowered coverage"
    );
    // 49 linefeeds reach the bottom row; 171 more scroll rows 0-170 away, all already covered.
    emit(&entry, &b"\n".repeat(220));
    assert!(steps.step(&entry).is_none());
    assert!(!steps.flags.missed, "only covered rows left history");
}
