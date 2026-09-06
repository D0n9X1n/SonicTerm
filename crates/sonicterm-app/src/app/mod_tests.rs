//! PTY resize reporting at the app seam.
//!
//! Every test here takes `MEDIA_COUNTER_LOCK` as its first statement, before
//! building any pane. Each `PaneState` creates an inline-media charge, and the
//! per-pane budget is a process-wide ceiling divided by the live charge count,
//! so a pane alive here shrinks the budget a sibling test is measuring and that
//! sibling fails reporting a defect that is not there. Declaring the guard
//! first makes it drop last, after the pane and the charge it owns.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Accumulates subscriber output so a test can assert on emitted warnings.
#[derive(Clone, Default)]
struct ResizeWarningLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for ResizeWarningLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `action` under a local `warn` subscriber and return what it logged.
fn capture_resize_warnings(action: impl FnOnce()) -> String {
    let log = ResizeWarningLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, action);
    let output = log.0.lock().clone();
    String::from_utf8(output).unwrap()
}

/// How many `pty resize failed` warnings `log` contains.
fn warning_count(log: &str) -> usize {
    log.matches("pty resize failed").count()
}

/// A pane whose PTY resize fails on demand, counting native attempts.
///
/// The spawned handle's own resize closure is the sole owner of the PTY master
/// — teardown closes the master by dropping that closure — so the probe moves
/// it into the replacement rather than assigning over it. Dropping it here
/// would release the master before `PtyHandle::drop`, which on Windows races
/// `ClosePseudoConsole` against the drain. The success path delegates to it, so
/// the original stays live and the native resize still happens.
fn pane_with_failing_resize() -> (PaneState, Arc<AtomicUsize>, Arc<AtomicBool>) {
    #[cfg(unix)]
    let (cmd, args) = ("/bin/cat", Vec::<String>::new());
    #[cfg(windows)]
    let (cmd, args) = ("cmd.exe", Vec::<String>::new());
    let mut pty =
        sonicterm_io::pty::PtyHandle::spawn_with_args(cmd, &args, 80, 24).expect("spawn probe pty");

    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(true));
    let seen = calls.clone();
    let should_fail = fail.clone();
    let original = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
    pty.resize = Box::new(move |cols, rows| {
        seen.fetch_add(1, Ordering::Relaxed);
        if should_fail.load(Ordering::Relaxed) {
            // When: `should_fail` is set, report a native refusal without touching the pty.
            return Err(anyhow::anyhow!("native resize refused"));
        }
        original(cols, rows)
    });

    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    (PaneState::new(parser, Some(pty)), calls, fail)
}

/// A pane with no PTY is not a resize failure and must not warn.
#[test]
fn a_pane_without_a_pty_is_not_a_resize_failure() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);

    let log = capture_resize_warnings(|| pane.resize_pty(7, 100, 30));

    assert_eq!(warning_count(&log), 0, "a pane with no PTY has no native geometry to fail at");
    assert!(!pane.resize_warned.load(Ordering::Relaxed), "an absent PTY must not latch");
}

/// The first failure warns once with pane id, requested geometry, and error.
#[test]
fn the_first_failure_warns_once_with_metadata_and_no_payload() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
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
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
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
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
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
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
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
