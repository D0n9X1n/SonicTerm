//! Pins `run_command`'s `exit_code_source`: what a completed run's code rests on is the D's named
//! source only when the run's markers were authenticated and carried a code, and `null` whenever
//! no final record was read, never confused with `unknown`.

use std::time::{Duration, Instant};

use sonicterm_grid::grid::{ExitSource, PinnedRun};

use super::*;
use crate::app::mcp::test_entry;

/// An authenticated, completed run whose D carried `exit` and `source`.
fn completed(exit: Option<i32>, source: Option<ExitSource>) -> PinnedRun {
    PinnedRun {
        seq: 0,
        alt: false,
        output_seen: true,
        output_start: None,
        output_at: Some(Instant::now()),
        end: None,
        end_col: 0,
        exit: Some(exit),
        evicted: false,
        prompt_after_end: true,
        output_authenticated: true,
        end_authenticated: true,
        exit_source: source,
    }
}

/// One case per value: each named source, `reported` for none, `unknown` for no code (even with
/// a source), and `unverified` over everything once either marker is unauthenticated.
#[test]
fn each_source_maps_with_its_precedence() {
    let cases = [
        (completed(Some(4), Some(ExitSource::Native)), "native"),
        (completed(Some(1), Some(ExitSource::PowerShell)), "powershell"),
        (completed(Some(0), Some(ExitSource::Status)), "status"),
        (completed(Some(0), None), "reported"),
        (completed(None, Some(ExitSource::Native)), "unknown"),
        (completed(None, None), "unknown"),
        (
            PinnedRun { end_authenticated: false, ..completed(Some(4), Some(ExitSource::Native)) },
            "unverified",
        ),
        (PinnedRun { output_authenticated: false, ..completed(None, None) }, "unverified"),
    ];
    for (run, expected) in cases {
        assert_eq!(exit_code_source(&run), expected, "{run:?}");
    }
}

/// A finish whose parser stays busy past its wait reads no final record, so the source is
/// `None` (`null`), not `unknown`; a run that did not start has none either.
#[test]
fn an_unread_or_unstarted_finish_has_no_source() {
    let entry = test_entry(30, 6, 100);
    entry.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let seq = entry.parser.lock().grid().newest_prompt().unwrap().seq;
    assert!(entry.parser.lock().grid_mut().pin_run(seq));
    let started = RunStarted { entry, seq };
    started
        .entry
        .parser
        .lock()
        .advance(b"x\r\n\x1b]133;C\x07\x1b]133;D;2;sonicterm_src=native\x07");
    let held = started.entry.parser.lock();
    let busy = finish(&started, RunState::Completed, Duration::from_millis(20));
    drop(held);
    assert_eq!(busy.exit_code_source, None, "a busy finish read nothing");
    let not_started = finish(&started, RunState::NotStarted, Duration::from_millis(250));
    assert_eq!(not_started.exit_code_source, None);
}

/// A completed finish that reads its record reports the tokenless D as `unverified`.
#[test]
fn a_read_finish_reports_the_source() {
    let entry = test_entry(30, 6, 100);
    entry.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let seq = entry.parser.lock().grid().newest_prompt().unwrap().seq;
    assert!(entry.parser.lock().grid_mut().pin_run(seq));
    let started = RunStarted { entry, seq };
    started
        .entry
        .parser
        .lock()
        .advance(b"x\r\n\x1b]133;C\x07\x1b]133;D;2;sonicterm_src=native\x07");
    let outcome = finish(&started, RunState::Completed, Duration::from_millis(250));
    assert_eq!(outcome.exit_code_source, Some("unverified"));
}
