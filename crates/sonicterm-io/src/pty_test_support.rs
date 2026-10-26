use super::*;

#[cfg(windows)]
static LIVE_PTY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
pub(super) fn lock_live_pty_test() -> std::sync::MutexGuard<'static, ()> {
    LIVE_PTY_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn diagnostic_input_channel() -> (PtyInputSender, Receiver<Vec<u8>>) {
    let (tx, rx) = pty_input_channel();
    (
        PtyInputSender {
            tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            writer_progress: Arc::new(PtyWriterProgress::new()),
        },
        rx,
    )
}

pub(super) fn observe_until(
    sender: &PtyInputSender,
    ready: impl Fn(&PtyInputDiagnostics) -> bool,
) -> PtyInputDiagnostics {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let diagnostics = sender.diagnostics();
        if ready(&diagnostics) {
            return diagnostics;
        }
        assert!(
            Instant::now() < deadline,
            "writer progress did not reach the expected state: {diagnostics:?}"
        );
        thread::yield_now();
    }
}

/// Exit-order source pins read every production module, not just the handle facade.
pub(super) const PTY_SOURCE: &str = concat!(
    include_str!("pty.rs"),
    "\n",
    include_str!("pty_input.rs"),
    "\n",
    include_str!("pty_output.rs"),
    "\n",
    include_str!("pty_child.rs"),
    "\n",
    include_str!("pty_session.rs"),
    "\n",
    include_str!("pty_native.rs"),
    "\n",
    include_str!("pty_teardown.rs"),
    "\n",
    include_str!("pty_resize.rs"),
    "\n",
    include_str!("pty_writer_file.rs"),
    "\n",
    include_str!("pty_spawn.rs"),
    "\n",
    include_str!("pty_reader.rs"),
    "\n",
    include_str!("pty_writer.rs"),
    "\n",
    include_str!("pty_shell.rs"),
);
