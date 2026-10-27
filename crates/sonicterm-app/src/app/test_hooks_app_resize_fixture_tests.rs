//! Native resize warning capture and failure contracts. regression contracts.

use super::*;

/// A pane with no PTY is not a resize failure and must not warn.
#[test]
fn a_pane_without_a_pty_is_not_a_resize_failure() {
    let pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);

    let log = capture_resize_warnings(|| pane.resize_pty(7, 100, 30));

    assert_eq!(warning_count(&log), 0, "a pane with no PTY has no native geometry to fail at");
    assert!(!pane.resize_warned.load(Ordering::Relaxed), "an absent PTY must not latch");
}

/// The first failure warns once with pane id, requested geometry, and error.
#[test]
fn the_first_failure_warns_once_with_metadata_and_no_payload() {
    let (pane, calls, _fail) = pane_with_failing_resize();
    // A sentinel the warning must never carry: resize reporting is metadata-only.
    pane.pty
        .as_ref()
        .expect("probe pty")
        .send_input_nonblocking(b"SONICTERM_PAYLOAD_SENTINEL\n".to_vec())
        .expect("queue probe input");

    let log = capture_resize_warnings(|| pane.resize_pty(7, 100, 30));

    assert_eq!(warning_count(&log), 1, "the first failure reports exactly once");
    assert!(log.contains("pane_id=7"), "the warning names the pane:\n{log}");
    assert!(log.contains("cols=100"), "the warning names the requested columns:\n{log}");
    assert!(log.contains("rows=30"), "the warning names the requested rows:\n{log}");
    assert!(log.contains("native resize refused"), "the warning carries the error:\n{log}");
    assert!(
        !log.contains("SONICTERM_PAYLOAD_SENTINEL"),
        "the warning must carry no terminal payload:\n{log}"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the native call was attempted");
}

/// Repeated failures warn once while every request still reaches the native call.
#[test]
fn repeated_failures_warn_once_but_never_stop_retrying() {
    let (pane, calls, _fail) = pane_with_failing_resize();

    let log = capture_resize_warnings(|| {
        pane.resize_pty(7, 100, 30);
        pane.resize_pty(7, 110, 32);
        pane.resize_pty(7, 120, 34);
    });

    assert_eq!(warning_count(&log), 1, "a failing run reports once, not once per request:\n{log}");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        3,
        "suppression is warning-side only; every request must still reach native"
    );
}

/// A success clears the latch, so the next failure warns a second time.
#[test]
fn a_success_clears_the_latch_so_a_later_failure_warns_again() {
    let (pane, calls, fail) = pane_with_failing_resize();

    let log = capture_resize_warnings(|| {
        pane.resize_pty(7, 100, 30);
        fail.store(false, Ordering::Relaxed);
        pane.resize_pty(7, 110, 32);
        fail.store(true, Ordering::Relaxed);
        pane.resize_pty(7, 120, 34);
    });

    assert_eq!(
        warning_count(&log),
        2,
        "the failure after a success is a new run and reports again:\n{log}"
    );
    assert!(log.contains("cols=100"), "the first run's geometry is reported:\n{log}");
    assert!(log.contains("cols=120"), "the second run's geometry is reported:\n{log}");
    assert_eq!(calls.load(Ordering::Relaxed), 3, "each distinct size reached native");
}

/// The grid keeps the requested geometry when the native resize fails.
///
/// Exercised through `resize_all_panes` rather than `resize_pty` directly,
/// because the ordering under test — grid first, native second, no rollback —
/// belongs to the caller.
#[test]
fn a_failed_native_resize_leaves_the_grid_committed() {
    let (pane, calls, _fail) = pane_with_failing_resize();
    let parser = pane.parser.clone();
    let mut panes = HashMap::new();
    panes.insert(7u64, pane);

    let log = capture_resize_warnings(|| resize_all_panes(&panes, 100, 30));

    let (cols, rows) = {
        let guard = parser.lock();
        let grid = guard.grid();
        (grid.cols, grid.rows)
    };
    assert_eq!((cols, rows), (100, 30), "the grid keeps the geometry the user asked for");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the native call was attempted and failed");
    assert_eq!(warning_count(&log), 1, "the failure was reported once:\n{log}");
}
