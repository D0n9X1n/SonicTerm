//! Native resize warning capture and failure contracts.

use super::*;

/// Accumulates subscriber output so a test can assert on emitted warnings.
#[derive(Clone, Default)]
pub(super) struct ResizeWarningLog(Arc<Mutex<Vec<u8>>>);

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
pub(super) fn capture_resize_warnings(action: impl FnOnce()) -> String {
    let log = ResizeWarningLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let output = log.0.lock().clone();
    String::from_utf8(output).unwrap()
}

/// How many `pty resize failed` warnings `log` contains.
pub(super) fn warning_count(log: &str) -> usize {
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
pub(super) fn pane_with_failing_resize() -> (PaneState, Arc<AtomicUsize>, Arc<AtomicBool>) {
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

#[cfg(test)]
#[path = "test_hooks_app_resize_fixture_tests.rs"]
mod test_hooks_app_resize_fixture_tests;
