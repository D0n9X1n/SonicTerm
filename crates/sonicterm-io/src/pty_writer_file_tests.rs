#[cfg(unix)]
use super::*;
#[cfg(unix)]
use crate::pty::pty_test_support::*;

/// Bytes the harness types before teardown, ending in the same newline and
/// Ctrl+D a synthesised EOF would inject, so the drain can tell them apart.
#[cfg(unix)]
const BASELINE_INPUT: &[u8] = b"typed-input\n\x04";

/// Explicit nonzero `VEOF`, so the upstream destructor cannot be inert.
#[cfg(unix)]
const FIXTURE_VEOF: u8 = 4;

/// A live PTY pair with an independently opened child-side descriptor.
///
/// The child side is never closed: doing so would fabricate the "no bytes
/// arrived" result these tests exist to prove.
#[cfg(unix)]
struct RawPtyFixture {
    /// `Option` so a test can drop the master while a duplicate still lives.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    child_side: std::fs::File,
    /// Read from the child side but not yet consumed, so a single `read(2)`
    /// returning typed and injected bytes together still attributes each.
    pending: Vec<u8>,
    _slave: Box<dyn portable_pty::SlavePty + Send>,
}

#[cfg(unix)]
impl RawPtyFixture {
    /// Open a real PTY in raw mode with an explicit nonzero `VEOF`.
    fn open() -> Self {
        use std::os::unix::fs::OpenOptionsExt;

        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("open a real pty pair");
        let tty_name = pair.master.tty_name().expect("master exposes its tty path");
        // O_NOCTTY keeps this pty from becoming our controlling terminal;
        // O_NONBLOCK keeps every read bounded by the caller's deadline.
        let child_side = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&tty_name)
            .expect("open the child side of the pty");

        let fixture =
            Self { master: Some(pair.master), child_side, pending: Vec::new(), _slave: pair.slave };
        fixture.make_raw_with_explicit_veof();
        // Asserted through the MASTER, the descriptor the upstream destructor
        // reads: a zero VEOF would make every later assertion vacuous.
        assert_eq!(
            fixture.master_veof(),
            FIXTURE_VEOF,
            "fixture precondition: the master must report a nonzero VEOF, or a destructor that \
             injects EOF would look identical to one that does not"
        );
        fixture
    }

    fn master(&self) -> &dyn portable_pty::MasterPty {
        &**self.master.as_ref().expect("master still held")
    }

    /// Release the master while leaving the child side open.
    fn drop_master(&mut self) {
        self.master.take().expect("master still held");
    }

    /// Raw mode with `VEOF` kept set: ICANON and ECHO off make the child-side
    /// stream contain writes and nothing else, so observation is exact.
    fn make_raw_with_explicit_veof(&self) {
        let fd = self.master_fd();
        let mut termios: libc::termios = {
            // SAFETY: `termios` is a plain struct of integers, so all-zero is a
            // valid state for `tcgetattr` to overwrite.
            unsafe { std::mem::zeroed() }
        };
        assert_eq!(
            // SAFETY: `fd` is the live master descriptor; `termios` is writable.
            unsafe { libc::tcgetattr(fd, &mut termios) },
            0,
            "read pty termios"
        );
        // SAFETY: `termios` was just filled by a successful `tcgetattr`.
        unsafe { libc::cfmakeraw(&mut termios) };
        termios.c_cc[libc::VEOF] = FIXTURE_VEOF;
        assert_eq!(
            // SAFETY: same live descriptor; `termios` is initialised and
            // outlives the call.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) },
            0,
            "apply raw termios with an explicit VEOF"
        );
    }

    /// `VEOF` as the master descriptor reports it.
    fn master_veof(&self) -> u8 {
        let fd = self.master_fd();
        let mut termios: libc::termios = {
            // SAFETY: `termios` is a plain struct of integers; zero is valid.
            unsafe { std::mem::zeroed() }
        };
        assert_eq!(
            // SAFETY: `fd` is the live master descriptor; `termios` is writable.
            unsafe { libc::tcgetattr(fd, &mut termios) },
            0,
            "read master termios"
        );
        termios.c_cc[libc::VEOF]
    }

    fn master_fd(&self) -> libc::c_int {
        self.master().as_raw_fd().expect("unix master exposes a descriptor")
    }

    /// Accumulate child-side bytes until `want` or `deadline`; bounded so a
    /// missing byte is reported rather than hanging the suite.
    fn fill_pending_until(&mut self, want: usize, deadline: Instant) {
        let mut chunk = [0u8; 256];
        while self.pending.len() < want && Instant::now() < deadline {
            match self.child_side.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("child-side read failed: {error}"),
            }
        }
    }

    /// Consume exactly `want` typed bytes; any surplus stays for the residue.
    fn read_exactly(&mut self, want: usize) -> Vec<u8> {
        self.fill_pending_until(want, Instant::now() + Duration::from_secs(5));
        let taken = want.min(self.pending.len());
        self.pending.drain(..taken).collect()
    }

    /// Everything the child received that no one typed. Called only after the
    /// writer thread is joined, so the settle window is slack, not sync.
    fn drain_residue(&mut self) -> Vec<u8> {
        // usize::MAX: read until the window closes, since "nothing at all" is
        // the expected result and there is no count to stop at.
        self.fill_pending_until(usize::MAX, Instant::now() + Duration::from_millis(250));
        std::mem::take(&mut self.pending)
    }
}

/// Armed, test-only failures for the writer thread's two native calls.
#[cfg(unix)]
#[derive(Default)]
struct InjectedWriterFaults {
    fail_write: std::sync::atomic::AtomicBool,
    fail_flush: std::sync::atomic::AtomicBool,
}

/// Owns the REAL production writer and fails one call on demand, so dropping
/// it still runs the production destructor under test.
#[cfg(unix)]
struct FaultInjectingWriter {
    inner: Box<dyn Write + Send>,
    faults: Arc<InjectedWriterFaults>,
}

#[cfg(unix)]
impl Write for FaultInjectingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.faults.fail_write.load(Ordering::SeqCst) {
            // When: a write fault is armed, fail before the native call so the
            // wrapper, not the pty, decides the error.
            return Err(std::io::Error::other("injected write failure (test-only)"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.faults.fail_flush.load(Ordering::SeqCst) {
            // When: a flush fault is armed, fail after the write landed,
            // matching a native flush error rather than a rejected write.
            return Err(std::io::Error::other("injected flush failure (test-only)"));
        }
        self.inner.flush()
    }
}

/// How the writer thread is made to end.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum TeardownMode {
    /// The cancellation channel fires, as `PtyHandle::drop` does.
    Cancelled,
    /// Every input producer goes away and the channel disconnects.
    Disconnected,
    /// A native write fails.
    WriteFailed,
    /// A native flush fails after its write landed.
    FlushFailed,
}

/// Require the writer thread to have exited on its own, then join it.
///
/// `PtyIoThread::finish` detaches on timeout, which would leave the writer
/// undropped and make an empty residue meaningless. Proving exit first means a
/// stuck thread fails the test instead of passing it.
#[cfg(unix)]
fn require_writer_exited(thread: &mut PtyIoThread) {
    // Disconnected also proves exit: `done_tx` drops as the thread unwinds.
    match thread.done.recv_timeout(Duration::from_secs(5)) {
        Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            panic!(
                "writer thread did not signal exit; its destructor cannot be assumed to have run"
            )
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !thread.handle.as_ref().is_some_and(|handle| handle.is_finished())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        thread.handle.as_ref().is_some_and(|handle| handle.is_finished()),
        "writer thread signalled exit but never finished, so the writer may still be alive"
    );
    // Joins the finished thread, which drops the boxed writer as it is reaped.
    thread.finish("regression writer");
}

/// Drive a real production writer through `mode` and return what the child
/// received after teardown; every typed byte is consumed first, so a non-empty
/// result is a byte no one typed.
#[cfg(unix)]
fn writer_teardown_residue(mode: TeardownMode) -> Vec<u8> {
    let mut fixture = RawPtyFixture::open();
    let production_writer = pty_writer(fixture.master()).expect("production writer factory");
    let faults = Arc::new(InjectedWriterFaults::default());
    let writer: Box<dyn Write + Send> =
        Box::new(FaultInjectingWriter { inner: production_writer, faults: faults.clone() });

    let (sender, rx) = diagnostic_input_channel();
    let (_replies, reply_reader) = reply_spool();
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut thread = spawn_writer_thread(
        writer,
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_reader,
    );

    // Baseline: prove the path works and leave the child-side queue empty.
    sender.send(BASELINE_INPUT.to_vec()).expect("queue baseline input");
    assert_eq!(
        fixture.read_exactly(BASELINE_INPUT.len()),
        BASELINE_INPUT,
        "the child must receive exactly the typed baseline before teardown is examined"
    );
    // Arriving bytes prove the write landed, not that the flush returned. A
    // fault armed before the writer is idle could fail the baseline's own
    // flush, so wait for a completed message first.
    observe_until(&sender, |diagnostics| {
        diagnostics.completed_messages == 1 && diagnostics.writer_phase == PtyWriterPhase::Idle
    });

    match mode {
        TeardownMode::Cancelled => {
            cancel_tx.send(()).expect("signal writer cancellation");
        }
        TeardownMode::Disconnected => {
            drop(sender);
        }
        TeardownMode::WriteFailed => {
            faults.fail_write.store(true, Ordering::SeqCst);
            sender.send(vec![b'y']).expect("queue the message whose write is failed");
        }
        TeardownMode::FlushFailed => {
            faults.fail_flush.store(true, Ordering::SeqCst);
            sender.send(vec![b'y']).expect("queue the message whose flush is failed");
            // This write genuinely succeeds, so its byte is typed input.
            assert_eq!(fixture.read_exactly(1), b"y", "the flush-failed write still lands");
        }
    }

    require_writer_exited(&mut thread);
    fixture.drain_residue()
}

/// Cancelled teardown, the path every pane close takes, must type nothing.
#[cfg(unix)]
#[test]
fn a_cancelled_writer_teardown_sends_no_bytes_to_the_child() {
    let residue = writer_teardown_residue(TeardownMode::Cancelled);
    assert!(
        residue.is_empty(),
        "closing a pane injected {residue:?} into the child after the last typed byte"
    );
}

/// A disconnected input channel must type nothing into the child.
#[cfg(unix)]
#[test]
fn a_disconnected_writer_teardown_sends_no_bytes_to_the_child() {
    let residue = writer_teardown_residue(TeardownMode::Disconnected);
    assert!(
        residue.is_empty(),
        "input-channel disconnect injected {residue:?} into the child after the last typed byte"
    );
}

/// A failed native write must not become typed input during teardown. The
/// failure comes from a wrapper around the real writer and the child side is
/// never closed, so empty means "nothing sent", not "nothing readable".
#[cfg(unix)]
#[test]
fn a_write_failure_teardown_sends_no_bytes_to_the_child() {
    let residue = writer_teardown_residue(TeardownMode::WriteFailed);
    assert!(residue.is_empty(), "a failed write's teardown injected {residue:?} into the child");
}

/// A failed native flush must not become typed input during teardown.
#[cfg(unix)]
#[test]
fn a_flush_failure_teardown_sends_no_bytes_to_the_child() {
    let residue = writer_teardown_residue(TeardownMode::FlushFailed);
    assert!(residue.is_empty(), "a failed flush's teardown injected {residue:?} into the child");
}

/// Removing the synthetic EOF must not cost the user the EOF they ask for:
/// an explicit Ctrl+D still ends a canonical shell.
#[cfg(unix)]
#[test]
fn an_explicit_ctrl_d_still_ends_a_canonical_shell() {
    // clean_e2e drops inherited ENV/BASH_ENV, so a user's shell hooks cannot
    // decide when — or whether — the prompt is reached.
    let pty = PtyHandle::spawn_with_args_and_opts(
        "/bin/sh",
        &[],
        80,
        24,
        ShellSpawnOpts { clean_e2e: true, ..Default::default() },
    )
    .expect("spawn a canonical shell");
    let probe = pty.child_exit_probe();

    // Readiness is proven by a marker the shell had to execute to produce.
    // It is assembled by printf from a separate argument, so the echoed
    // command line cannot contain it and only real output can match.
    pty.send_input_nonblocking(b"printf '__READY_%s__\\n' 'EOF'\n".to_vec())
        .expect("ask the shell to announce readiness");
    let marker = b"__READY_EOF__";
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while Instant::now() < deadline && !output.windows(marker.len()).any(|window| window == marker)
    {
        match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("shell output closed before it reported readiness")
            }
        }
    }
    assert!(
        output.windows(marker.len()).any(|window| window == marker),
        "shell never reported readiness; output={}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        !probe.has_exited().expect("probe the shell"),
        "the shell exited before Ctrl+D was sent, so its exit proves nothing about EOF"
    );

    pty.send_input_nonblocking(vec![0x04]).expect("send an explicit Ctrl+D");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !probe.has_exited().expect("probe the shell") && Instant::now() < deadline {
        while pty.out_rx.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        probe.has_exited().expect("final shell probe"),
        "an explicit Ctrl+D no longer ends a canonical shell"
    );
}

/// Dropping the handle must still reap both the shell leader and a background
/// descendant of its session, within a bounded drop. Repeated because a single
/// teardown can win a race it would usually lose.
#[cfg(unix)]
#[test]
fn dropping_the_handle_still_cleans_up_the_leader_and_a_same_session_descendant() {
    for round in 1..=3 {
        // `$$` is the shell leader, `$!` the background child; the trap keeps
        // the child from dying to a hangup so teardown has to kill it.
        let args =
            vec!["-c".to_string(), "trap '' HUP; sleep 30 & echo $$ $!; sleep 30".to_string()];
        let pty = PtyHandle::spawn_with_args("/bin/sh", &args, 80, 24).expect("spawn shell");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while !output.contains(&b'\n') && Instant::now() < deadline {
            match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) => output.extend_from_slice(&chunk),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    panic!("round {round}: PTY output closed before the pids were reported")
                }
            }
        }
        let reported = String::from_utf8_lossy(&output);
        let mut pids =
            reported.split_whitespace().filter_map(|field| field.parse::<libc::pid_t>().ok());
        let leader = pids.next().unwrap_or_else(|| panic!("round {round}: no leader pid"));
        let background = pids.next().unwrap_or_else(|| panic!("round {round}: no background pid"));

        let started = Instant::now();
        drop(pty);
        let dropped_in = started.elapsed();
        assert!(
            dropped_in < Duration::from_secs(10),
            "round {round}: dropping the handle took {dropped_in:?}, so teardown is not bounded"
        );

        for (label, pid) in [("leader", leader), ("background", background)] {
            let deadline = Instant::now() + Duration::from_secs(5);
            while unix_process_is_active(pid as u32).expect("probe process")
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !unix_process_is_active(pid as u32).expect("final probe"),
                "round {round}: dropping the handle left {label} pid {pid} running"
            );
        }
    }
}

/// The duplicated master descriptor must be close-on-exec (so a child cannot
/// inherit a writable handle to its own terminal), outlive the master it came
/// from, and type nothing when closed.
#[cfg(unix)]
#[test]
fn the_duplicated_master_descriptor_is_cloexec_independent_and_silent() {
    use std::os::fd::AsRawFd;

    let mut fixture = RawPtyFixture::open();
    let master_fd = fixture.master_fd();

    let mut first = unix_master_writer_file(fixture.master()).expect("duplicate the master");
    let first_fd = first.as_raw_fd();
    assert_ne!(
        first_fd, master_fd,
        "the writer must hold its own descriptor, or closing it would close the master"
    );

    let flags = {
        // SAFETY: `first_fd` is owned by the live `first` for this call, and
        // F_GETFD only reads that descriptor's flags.
        unsafe { libc::fcntl(first_fd, libc::F_GETFD) }
    };
    assert_ne!(flags, -1, "read the duplicate's descriptor flags");
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "the duplicate must be close-on-exec so a spawned child cannot inherit a writable handle \
         to its own terminal"
    );

    first.write_all(b"x").expect("write through the first duplicate");
    first.flush().expect("flush the first duplicate");
    assert_eq!(fixture.read_exactly(1), b"x", "the duplicate must deliver a typed byte");

    let mut second =
        unix_master_writer_file(fixture.master()).expect("duplicate the master a second time");
    drop(first);
    let residue = fixture.drain_residue();
    assert!(residue.is_empty(), "closing a duplicated master descriptor typed {residue:?}");

    // Outliving the master is what makes the duplicate's lifetime independent:
    // it shares the open file description, so the pty stays writable.
    fixture.drop_master();
    second.write_all(b"z").expect("write after the master was dropped");
    second.flush().expect("flush after the master was dropped");
    assert_eq!(
        fixture.read_exactly(1),
        b"z",
        "a duplicate must keep delivering after the master it came from is gone"
    );

    drop(second);
    let residue = fixture.drain_residue();
    assert!(residue.is_empty(), "closing the last duplicate typed {residue:?}");
}

/// A master that exposes no descriptor, recording any `take_writer` fallback.
#[cfg(unix)]
struct DescriptorlessMaster {
    take_writer_calls: Arc<AtomicUsize>,
}

#[cfg(unix)]
impl portable_pty::MasterPty for DescriptorlessMaster {
    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize::default())
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        // Recorded rather than panicking, so the test reports "the fallback was
        // taken" instead of an unwind that hides which contract broke.
        self.take_writer_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(std::io::sink()))
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        None
    }

    fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        None
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// A Unix master with no descriptor must fail, never fall back to
/// `take_writer` — that fallback is the byte-injecting writer the fix removed,
/// so a silent one would reintroduce the defect with every test still green.
#[cfg(unix)]
#[test]
fn a_master_without_a_descriptor_errors_instead_of_falling_back() {
    let take_writer_calls = Arc::new(AtomicUsize::new(0));
    let master = DescriptorlessMaster { take_writer_calls: take_writer_calls.clone() };

    // Matched rather than `expect_err`: the Ok payload is a boxed writer, which
    // is not `Debug`, and the Ok arm is itself the failure being reported.
    let error = match pty_writer(&master) {
        Ok(_) => panic!(
            "pty_writer returned a writer for a descriptor-less master, so it fell back to \
             take_writer, whose destructor injects a newline and VEOF into the child"
        ),
        Err(error) => error,
    };

    let io_error = error
        .downcast_ref::<std::io::Error>()
        .unwrap_or_else(|| panic!("expected an io::Error the caller can classify, got: {error:?}"));
    assert_eq!(
        io_error.kind(),
        std::io::ErrorKind::Unsupported,
        "a missing descriptor must be reported as Unsupported, not as a generic failure"
    );
    assert_eq!(
        take_writer_calls.load(Ordering::SeqCst),
        0,
        "pty_writer fell back to take_writer, whose destructor injects a newline and VEOF into \
         the child"
    );
}
