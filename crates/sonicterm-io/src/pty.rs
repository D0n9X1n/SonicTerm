//! Cross-platform PTY spawning.
//!
//! Wraps the [`portable-pty`] crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

/// Outgoing message: bytes to write to the pty master (typed by user).
type Outgoing = Vec<u8>;
/// Incoming message: bytes read from the pty master (program output).
///
/// Uses [`bytes::Bytes`] — a refcounted slice — so the reader thread can
/// hand the buffer off to the VT thread without per-read `Vec::to_vec`
/// allocations. The reader keeps a single [`BytesMut`] ring of 64 KiB and
/// `split_to`s the filled prefix into a `Bytes` each iteration; once the
/// ring drains below capacity it reuses the same allocation.
type Incoming = Bytes;

/// Maximum unread PTY output chunks retained per pane.
///
/// Each chunk is at most the 64 KiB reader-ring size, so this bounds queued
/// output to roughly 4 MiB. Once full, the reader blocks and lets the OS PTY
/// apply backpressure instead of growing process memory without limit.
pub const PTY_OUTPUT_QUEUE_CAPACITY: usize = 64;
/// Maximum pending terminal-input messages retained per pane.
pub const PTY_INPUT_QUEUE_CAPACITY: usize = 4;
/// Largest single terminal-input message accepted by first-party callers.
pub const MAX_PTY_INPUT_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const PTY_IO_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(windows)]
const CONPTY_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
static ACTIVE_PTY_IO_THREADS: AtomicUsize = AtomicUsize::new(0);

/// A terminal-input message that could not be queued without blocking.
#[derive(Debug, thiserror::Error)]
pub enum PtyInputError {
    /// The message exceeds [`MAX_PTY_INPUT_MESSAGE_BYTES`].
    #[error("PTY input message exceeds the per-message byte limit")]
    MessageTooLarge(Vec<u8>),
    /// The bounded writer queue has no available slot.
    #[error("PTY input writer queue is full")]
    QueueFull(Vec<u8>),
    /// The PTY writer has already stopped.
    #[error("PTY input writer is disconnected")]
    WriterDisconnected(Vec<u8>),
}

impl PtyInputError {
    /// Recover the rejected bytes so a caller can retry or report their size.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::MessageTooLarge(bytes)
            | Self::QueueFull(bytes)
            | Self::WriterDisconnected(bytes) => bytes,
        }
    }
}

/// Cloneable, non-blocking probe for a PTY child process's exit state.
#[derive(Clone)]
pub struct PtyChildExitProbe {
    child: Arc<Mutex<ChildState>>,
}

impl PtyChildExitProbe {
    /// Return whether the child has exited without waiting for it.
    pub fn has_exited(&self) -> Result<bool> {
        let mut child = self.child.lock();
        #[cfg(windows)]
        return Ok(child.has_exited()?);
        #[cfg(unix)]
        {
            if child.exited {
                return Ok(true);
            }
            let Some(pid) = child.child.process_id() else {
                return Ok(false);
            };
            if !unix_child_exit_pending(pid)? {
                return Ok(false);
            }
            signal_process_group_for_platform(&mut child)?;
            Ok(true)
        }
    }
}

#[cfg(unix)]
fn unix_child_exit_pending(pid: u32) -> std::io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: info points to writable siginfo storage. WNOWAIT observes the
    // exited leader without reaping it, keeping its pid/pgid reserved until
    // teardown has signalled the process group.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

struct ChildState {
    child: Box<dyn Child + Send + Sync>,
    exited: bool,
    unix_session_id: Option<u32>,
    process_group_signalled: bool,
}

impl ChildState {
    fn new(child: Box<dyn Child + Send + Sync>, unix_session_id: Option<u32>) -> Self {
        Self { child, exited: false, unix_session_id, process_group_signalled: false }
    }

    fn has_exited(&mut self) -> std::io::Result<bool> {
        if self.exited {
            return Ok(true);
        }
        if self.child.try_wait()?.is_some() {
            self.exited = true;
        }
        Ok(self.exited)
    }

    fn process_id(&self) -> Option<u32> {
        (!self.exited).then(|| self.child.process_id()).flatten()
    }
}

fn terminate_child<G, P>(
    child: &mut ChildState,
    signal_group: G,
    mut signal_pid: P,
) -> std::io::Result<()>
where
    G: FnMut(u32) -> std::io::Result<()>,
    P: FnMut(u32),
{
    signal_process_group(child, signal_group)?;
    if child.has_exited()? {
        return Ok(());
    }
    if let Some(pid) = child.process_id() {
        signal_pid(pid);
    }
    child.child.kill()
}

fn signal_process_group<G>(child: &mut ChildState, mut signal_group: G) -> std::io::Result<()>
where
    G: FnMut(u32) -> std::io::Result<()>,
{
    if child.process_group_signalled {
        return Ok(());
    }
    if let Some(unix_session_id) = child.unix_session_id {
        signal_group(unix_session_id)?;
    }
    child.process_group_signalled = true;
    Ok(())
}

#[cfg(unix)]
fn signal_process_group_for_platform(child: &mut ChildState) -> std::io::Result<()> {
    signal_process_group(child, terminate_unix_session)
}

#[cfg(target_os = "macos")]
fn unix_session_pids(session_id: u32) -> std::io::Result<Vec<u32>> {
    use libproc::processes::{pids_by_type, ProcFilter};

    Ok(pids_by_type(ProcFilter::All)?
        .into_iter()
        .filter(|pid| {
            *pid != 0 && unsafe { libc::getsid(*pid as libc::pid_t) } == session_id as libc::pid_t
        })
        .collect())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_session_pids(session_id: u32) -> std::io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if (unsafe { libc::getsid(pid as libc::pid_t) }) == session_id as libc::pid_t {
            pids.push(pid);
        }
    }
    Ok(pids)
}

#[cfg(unix)]
fn terminate_unix_session(session_id: u32) -> std::io::Result<()> {
    // Signal the shell's original process group even if process-table access
    // is restricted.
    unsafe {
        libc::kill(-(session_id as libc::pid_t), libc::SIGKILL);
    }
    for _ in 0..8 {
        let members = unix_session_pids(session_id)?
            .into_iter()
            .filter(|pid| *pid != session_id && *pid != std::process::id())
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Ok(());
        }
        for pid in members {
            // Recheck membership immediately before signalling.
            if unsafe { libc::getsid(pid as libc::pid_t) } != session_id as libc::pid_t {
                continue;
            }
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "PTY session still has live descendants after termination attempts",
    ))
}

fn terminate_child_for_platform(child: &mut ChildState) -> std::io::Result<()> {
    terminate_child(
        child,
        |unix_session_id| {
            #[cfg(unix)]
            return terminate_unix_session(unix_session_id);
            #[cfg(not(unix))]
            {
                let _ = unix_session_id;
                Ok(())
            }
        },
        |pid| {
            #[cfg(unix)]
            {
                // SAFETY: ChildState::has_exited just returned false while
                // holding the child mutex, so the direct pid is not reaped.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            let _ = pid;
        },
    )
}

fn pty_output_channel() -> (Sender<Incoming>, Receiver<Incoming>) {
    crossbeam_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY)
}

fn pty_input_channel() -> (Sender<Outgoing>, Receiver<Outgoing>) {
    crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY)
}

fn try_queue_pty_input(tx: &Sender<Outgoing>, bytes: Vec<u8>) -> Result<(), PtyInputError> {
    if !pty_input_message_allowed(bytes.len()) {
        return Err(PtyInputError::MessageTooLarge(bytes));
    }
    tx.try_send(bytes).map_err(|error| match error {
        crossbeam_channel::TrySendError::Full(bytes) => PtyInputError::QueueFull(bytes),
        crossbeam_channel::TrySendError::Disconnected(bytes) => {
            PtyInputError::WriterDisconnected(bytes)
        }
    })
}

#[must_use]
pub fn pty_input_message_allowed(bytes: usize) -> bool {
    bytes <= MAX_PTY_INPUT_MESSAGE_BYTES
}

/// Maximum bytes that can wait in one pane's PTY input channel.
#[must_use]
pub fn max_pty_queued_input_bytes() -> usize {
    PTY_INPUT_QUEUE_CAPACITY.saturating_mul(MAX_PTY_INPUT_MESSAGE_BYTES)
}

/// Bytes waiting in this pane's PTY output channel.
///
/// The reader hands out `Bytes` views into a reused 64 KiB ring rather than
/// allocating per chunk, so a full queue holds far less than
/// `capacity x chunk size` would suggest — measured at 512 KiB against the
/// 4 MiB that arithmetic predicts. Summing the queued views reports what is
/// actually held rather than what the slot count implies.
///
/// Reads the channel without consuming it, so a caller may sample this on any
/// thread without disturbing the pump.
#[must_use]
pub fn queued_output_bytes(handle: &PtyHandle) -> usize {
    // `Receiver::len` observes without consuming. Draining to measure would
    // make the diagnostic eat the data it is reporting on.
    handle.out_rx.len().saturating_mul(PTY_READ_CHUNK_BYTES)
}

/// Typical bytes in one queued PTY output chunk.
///
/// The reader fills a 64 KiB ring in reads bounded by `READ_HEADROOM`, so a
/// queued view is at most this size. Used to report queue occupancy without
/// consuming the channel.
pub const PTY_READ_CHUNK_BYTES: usize = 8 * 1024;

#[cfg(all(test, windows))]
#[must_use]
fn active_pty_io_threads() -> usize {
    ACTIVE_PTY_IO_THREADS.load(Ordering::Acquire)
}

struct ActivePtyIoThread;

impl ActivePtyIoThread {
    fn enter() -> Self {
        ACTIVE_PTY_IO_THREADS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ActivePtyIoThread {
    fn drop(&mut self) {
        ACTIVE_PTY_IO_THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PtyIoThread {
    handle: Option<thread::JoinHandle<()>>,
    done: Receiver<()>,
}

impl PtyIoThread {
    #[cfg(windows)]
    fn cancel_synchronous_io(&self) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::IO::CancelSynchronousIo};

        let Some(handle) = self.handle.as_ref() else { return };
        let thread_handle = HANDLE(handle.as_raw_handle());
        // SAFETY: JoinHandle owns a live Windows thread handle. Cancellation
        // only interrupts that thread's pending synchronous I/O.
        unsafe {
            if let Err(error) = CancelSynchronousIo(thread_handle) {
                tracing::debug!(%error, "PTY I/O thread had no cancellable synchronous operation");
            }
        }
    }

    #[cfg(not(windows))]
    fn cancel_synchronous_io(&self) {}

    fn finish(&mut self, name: &'static str) {
        let finished = match self.done.recv_timeout(PTY_IO_SHUTDOWN_TIMEOUT) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => true,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => false,
        };
        if finished {
            if let Some(handle) = self.handle.take() {
                if handle.join().is_err() {
                    tracing::warn!("{name} panicked during shutdown");
                }
            }
        } else {
            tracing::warn!("{name} did not exit within the PTY shutdown timeout");
            self.handle.take();
        }
    }
}

/// Handle to a running pty process.
///
/// On drop, the child process is explicitly killed, pending native I/O is
/// cancelled, and the PTY reader/writer threads are given a bounded interval
/// to exit.
pub struct PtyHandle {
    /// Channel of byte chunks read from the child's stdout/stderr.
    pub out_rx: Receiver<Incoming>,
    /// Channel for bytes / control messages to send to the child.
    pub in_tx: Sender<Outgoing>,
    /// Closure that resizes the pty to `(cols, rows)`.
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    reader_cancel: Sender<()>,
    writer_cancel: Sender<()>,
    reader_thread: PtyIoThread,
    writer_thread: PtyIoThread,
    child: Arc<Mutex<ChildState>>,
    #[cfg(windows)]
    conpty_drain_reader: Option<Box<dyn Read + Send>>,
    /// Resolved shell program path (the command we actually spawned).
    shell_program_path: String,
}

/// Options controlling how `spawn_default_shell` constructs the shell
/// command line. Default is interactive behavior (preserve user profile,
/// banner, prompt). E2E gates / examples that need deterministic output
/// pass `clean_e2e: true` to suppress profile/logo and emit shell-family-
/// specific clean-startup args.
///
/// Added — pre-PR examples sent POSIX `printf` to PowerShell,
/// producing zero output. PLAN v5 split the fix into:
///   1. (this) — add opts + WindowsApps stub filter + shell-path accessor
///   2. (next PR) — ShellDialect trait + golden fixtures + actual e2e fix
#[derive(Clone, Debug)]
pub struct ShellSpawnOpts {
    /// Suppress shell startup banner/profile and emit clean-mode args
    /// (PowerShell `-NoLogo -NoProfile`, bash `--norc --noprofile`,
    /// zsh `-f`). For e2e gates only — production app keeps default.
    pub clean_e2e: bool,
    /// `TERM_PROGRAM` value injected into the child PTY environment.
    /// Defaults to `SonicTerm` to preserve existing terminal identity.
    pub term_program: String,
    /// Explicit shell program override from `[terminal] shell`. When
    /// `Some(non-empty)`, this is spawned verbatim instead of the
    /// platform default. `None` / empty → auto-detect.
    pub shell: Option<String>,
}

impl ShellSpawnOpts {
    /// Production default `TERM_PROGRAM` value.
    pub const DEFAULT_TERM_PROGRAM: &'static str = "SonicTerm";
}

impl Default for ShellSpawnOpts {
    fn default() -> Self {
        Self { clean_e2e: false, term_program: Self::DEFAULT_TERM_PROGRAM.to_string(), shell: None }
    }
}

impl PtyHandle {
    /// Explicitly terminate the child shell. Idempotent — second call is a
    /// no-op because the underlying handle will report it's already gone.
    /// Called automatically on Drop, but exposed for callers that want
    /// deterministic shutdown earlier.
    pub fn kill(&self) {
        let mut child = self.child.lock();
        let _ = terminate_child_for_platform(&mut child);
    }

    /// Process id of the underlying shell, if the platform reports it. Used
    /// by the tab-title renderer to probe the foreground process running in
    /// this pane's pty (e.g. "zsh" vs "nvim" vs "ssh"). Returns `None` if
    /// the OS layer doesn't expose a pid (rare) or if the child has already
    /// exited.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().process_id()
    }

    /// Resolved shell program path (the command we actually spawned).
    pub fn shell_program_path(&self) -> &str {
        &self.shell_program_path
    }

    /// Build a cloneable probe for consumers that must observe natural exit
    /// even when a platform PTY reader remains blocked until master teardown.
    pub fn child_exit_probe(&self) -> PtyChildExitProbe {
        PtyChildExitProbe { child: self.child.clone() }
    }

    /// Queue terminal input without blocking the event-loop thread.
    ///
    /// On failure, the error retains the rejected bytes so the caller can
    /// retry or notify the user instead of silently losing terminal input.
    pub fn send_input_nonblocking(&self, bytes: Vec<u8>) -> Result<(), PtyInputError> {
        try_queue_pty_input(&self.in_tx, bytes)
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        let resize = std::mem::replace(&mut self.resize, Box::new(|_, _| {}));
        let mut teardown =
            PtyHandleTeardown { handle: self, resize: Some(resize), termination_failed: false };
        run_pty_teardown(&mut teardown);
    }
}

trait PtyTeardownOps {
    fn signal_cancel(&mut self);
    fn cancel_io(&mut self);
    fn terminate_child(&mut self);
    fn finish_io(&mut self);
    fn close_master(&mut self);
    fn reap_child(&mut self);
}

fn run_pty_teardown(teardown: &mut impl PtyTeardownOps) {
    teardown.signal_cancel();
    teardown.cancel_io();
    teardown.terminate_child();
    #[cfg(windows)]
    {
        teardown.finish_io();
        teardown.close_master();
    }
    #[cfg(not(windows))]
    {
        teardown.close_master();
        teardown.finish_io();
    }
    teardown.reap_child();
}

struct PtyHandleTeardown<'a> {
    handle: &'a mut PtyHandle,
    resize: Option<Box<dyn Fn(u16, u16) + Send + Sync>>,
    termination_failed: bool,
}

impl PtyTeardownOps for PtyHandleTeardown<'_> {
    fn signal_cancel(&mut self) {
        let _ = self.handle.reader_cancel.try_send(());
        let _ = self.handle.writer_cancel.try_send(());
    }

    fn cancel_io(&mut self) {
        self.handle.reader_thread.cancel_synchronous_io();
        self.handle.writer_thread.cancel_synchronous_io();
    }

    fn terminate_child(&mut self) {
        let mut child = self.handle.child.lock();
        let result = terminate_child_for_platform(&mut child);
        if let Err(error) = result {
            tracing::warn!(%error, "failed to terminate PTY child");
            self.termination_failed = true;
        } else {
            self.termination_failed = false;
        }
    }

    fn finish_io(&mut self) {
        self.handle.reader_thread.finish("PTY reader thread");
        self.handle.writer_thread.finish("PTY writer thread");
    }

    fn close_master(&mut self) {
        #[cfg(windows)]
        if let (Some(reader), Some(resize)) =
            (self.handle.conpty_drain_reader.take(), self.resize.take())
        {
            let completed =
                close_master_with_drain(reader, move || drop(resize), CONPTY_CLOSE_TIMEOUT);
            if !completed {
                tracing::warn!("ConPTY master close did not finish within the shutdown timeout");
            }
            return;
        }
        drop(self.resize.take());
    }

    fn reap_child(&mut self) {
        if self.termination_failed {
            let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
            loop {
                match terminate_child_for_platform(&mut self.handle.child.lock()) {
                    Ok(()) => {
                        self.termination_failed = false;
                        break;
                    }
                    Err(error) if Instant::now() >= deadline => {
                        tracing::warn!(
                            %error,
                            "PTY session cleanup failed through the shutdown deadline; leader left unreaped"
                        );
                        return;
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        let mut child = self.handle.child.lock();
        if child.exited {
            return;
        }
        let pid_for_log = child.process_id();
        let deadline = std::time::Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
        loop {
            match child.has_exited() {
                Ok(true) => break,
                Ok(false) if std::time::Instant::now() >= deadline => {
                    tracing::warn!(
                        pid = pid_for_log,
                        "PTY child did not exit within the shutdown timeout"
                    );
                    break;
                }
                Ok(false) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    tracing::warn!(%error, "failed to reap PTY child");
                    break;
                }
            }
        }
    }
}

#[cfg(windows)]
fn close_master_with_drain(
    mut reader: Box<dyn Read + Send>,
    close_master: impl FnOnce() + Send + 'static,
    timeout: Duration,
) -> bool {
    let (drain_done_tx, drain_done_rx) = crossbeam_channel::bounded(1);
    let drain_thread =
        match thread::Builder::new().name("sonic-conpty-drain".into()).spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = drain_done_tx.send(());
        }) {
            Ok(thread) => thread,
            Err(error) => {
                tracing::warn!(%error, "failed to spawn ConPTY drain thread");
                std::mem::forget(close_master);
                return false;
            }
        };
    let (close_done_tx, close_done_rx) = crossbeam_channel::bounded(1);
    let close_thread =
        match thread::Builder::new().name("sonic-conpty-close".into()).spawn(move || {
            close_master();
            let _ = close_done_tx.send(());
        }) {
            Ok(thread) => thread,
            Err(error) => {
                tracing::warn!(%error, "failed to spawn ConPTY close thread");
                std::mem::forget(drain_thread);
                return false;
            }
        };

    let close_finished = matches!(
        close_done_rx.recv_timeout(timeout),
        Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected)
    );
    if close_finished {
        let _ = close_thread.join();
        let drain_finished = matches!(
            drain_done_rx.recv_timeout(timeout),
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        );
        if drain_finished {
            let _ = drain_thread.join();
        } else {
            drop(drain_thread);
        }
    } else {
        drop(close_thread);
        drop(drain_thread);
    }
    close_finished
}

impl PtyHandle {
    /// Spawn the user's default shell.
    ///
    /// `opts.clean_e2e=true` suppresses shell startup banner/profile and
    /// emits clean-mode args (PowerShell `-NoLogo -NoProfile`, bash
    /// `--norc --noprofile`, zsh `-f`). E2E gates pass `true`; the
    /// production app passes `ShellSpawnOpts::default()` to preserve
    /// interactive behavior.
    pub fn spawn_default_shell(cols: u16, rows: u16, opts: ShellSpawnOpts) -> Result<Self> {
        let shell = resolve_spawn_shell(opts.shell.as_deref());
        let args = shell_startup_args(&shell, opts.clone());
        Self::spawn_with_args_and_opts(&shell, &args, cols, rows, opts)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args(cmd, &[], cols, rows)
    }

    /// Internal: spawn `cmd` with `args`. The public `spawn` + `spawn_default_shell`
    /// converge here so opts-derived args (e.g. `-NoLogo -NoProfile` for
    /// PowerShell clean_e2e) reach `CommandBuilder` consistently.
    ///
    /// Also `pub` (doc-hidden) so integration tests can spawn shells with
    /// args (e.g. `bash -c "trap '' HUP; exec cat"` for the LM-007
    /// regression test) without re-implementing the whole pipeline.
    #[doc(hidden)]
    pub fn spawn_with_args(cmd: &str, args: &[String], cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args_and_opts(cmd, args, cols, rows, ShellSpawnOpts::default())
    }

    /// Internal: spawn `cmd` with `args` and explicit environment options.
    #[doc(hidden)]
    pub fn spawn_with_args_and_opts(
        cmd: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        opts: ShellSpawnOpts,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(cmd);
        for a in args {
            builder.arg(a);
        }
        if let Ok(home) = std::env::var("HOME") {
            builder.cwd(home);
        }
        apply_child_pty_env(&mut builder, &opts.term_program);

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let master = pair.master;
        #[cfg(unix)]
        let unix_session_id = child.process_id();
        #[cfg(not(unix))]
        let unix_session_id = None;
        let reader = master.try_clone_reader()?;
        #[cfg(windows)]
        let conpty_drain_reader = Some(master.try_clone_reader()?);
        let writer = master.take_writer()?;
        let master = Arc::new(Mutex::new(master));

        let (out_tx, out_rx) = pty_output_channel();
        let (in_tx, in_rx) = pty_input_channel();
        let (reader_cancel, reader_cancel_rx) = crossbeam_channel::bounded(1);
        let (writer_cancel, writer_cancel_rx) = crossbeam_channel::bounded(1);

        // Reader thread: pty -> out_rx.
        let reader_thread = spawn_reader_thread(reader, out_tx, reader_cancel_rx);
        // Writer thread: in_rx -> pty.
        let writer_thread = spawn_writer_thread(writer, in_rx, writer_cancel_rx);

        let resize_master = master.clone();
        // Dedup no-op resizes. Callers (e.g. tab switch via
        // `resize_visible_panes`) invoke this on every activation even
        // when geometry is unchanged; on Windows each call is a ConPTY
        // `ResizePseudoConsole` that forces a console reflow + shell
        // repaint (SIGWINCH on Unix), which shows up as tab-switch lag.
        // Pack (cols, rows) into one u32 and skip when it matches the
        // last applied size. Seeded to u32::MAX so the first real resize
        // always applies.
        let last_resize = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
        let resize = Box::new(move |cols: u16, rows: u16| {
            let packed = (u32::from(cols) << 16) | u32::from(rows);
            if last_resize.swap(packed, std::sync::atomic::Ordering::Relaxed) == packed {
                return;
            }
            // The public callback currently returns `()`, so this error cannot
            // reach the caller. Keep the limitation explicit until the resize
            // seam can report failures without changing every app call site.
            let _ = resize_master.lock().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        });

        Ok(Self {
            out_rx,
            in_tx,
            resize,
            reader_cancel,
            writer_cancel,
            reader_thread,
            writer_thread,
            child: Arc::new(Mutex::new(ChildState::new(child, unix_session_id))),
            #[cfg(windows)]
            conpty_drain_reader,
            shell_program_path: cmd.to_string(),
        })
    }
}

fn send_pty_output(tx: &Sender<Incoming>, cancel: &Receiver<()>, chunk: Incoming) -> bool {
    crossbeam_channel::select! {
        send(tx, chunk) -> result => result.is_ok(),
        recv(cancel) -> _ => false,
    }
}

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<Incoming>,
    cancel: Receiver<()>,
) -> PtyIoThread {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let handle = thread::Builder::new()
        .name("sonic-pty-reader".into())
        .spawn(move || {
            let _active = ActivePtyIoThread::enter();
            // 64 KiB ring. We `split` the filled prefix into a `Bytes`
            // (refcounted view into the same allocation) on each read and
            // send it downstream. Once consumers drop their `Bytes`, the
            // next `reserve` call reclaims the original allocation in-place
            // — no per-read heap alloc, no `to_vec`. Replaces the previous
            // `[u8; 8192]` stack buffer + `buf[..n].to_vec()` pattern that
            // allocated once per read (and the reader can fire thousands of
            // reads per second under `cat largefile`).
            const RING_CAP: usize = 64 * 1024;
            // Keep at least one full PTY chunk (typical kernel pipe buffer
            // is 4–16 KiB) of headroom before each read to avoid forcing a
            // realloc mid-read.
            const READ_HEADROOM: usize = 8 * 1024;
            let mut buf = BytesMut::with_capacity(RING_CAP);
            loop {
                if buf.capacity() - buf.len() < READ_HEADROOM {
                    // If downstream has dropped its `Bytes` views, this
                    // reclaims the original buffer; otherwise it allocates
                    // a fresh one and drops our half of the previous ring.
                    buf.reserve(RING_CAP);
                }
                // Zero-initialise the spare region before handing it to
                // `Read::read`. `Read` requires an initialised destination
                // slice (passing `MaybeUninit` bytes via a `&mut [u8]` cast
                // is UB even though most impls never read from it). The
                // memset cost on a 64 KiB region is dominated by the syscall
                // itself; the underlying allocation is still reused across
                // reads, preserving the zero-alloc steady state.
                let initial_len = buf.len();
                let read_cap = buf.capacity() - initial_len;
                buf.resize(initial_len + read_cap, 0);
                match reader.read(&mut buf[initial_len..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.truncate(initial_len + n);
                        let chunk = buf.split().freeze();
                        if !send_pty_output(&tx, &cancel, chunk) {
                            break;
                        }
                    }
                    Err(e) => {
                        if cancel.try_recv().is_err() {
                            tracing::warn!("pty read error: {e}");
                        }
                        break;
                    }
                }
            }
            let _ = done_tx.send(());
        })
        // PANIC: thread::Builder::spawn only fails on OS-level resource
        // exhaustion (out of memory / out of process handles). At terminal
        // startup we cannot meaningfully recover — propagating a Result up
        // through `spawn_pane` would land on the same `expect`. Documented.
        .expect("spawn pty reader");
    PtyIoThread { handle: Some(handle), done }
}

fn spawn_writer_thread(
    mut writer: Box<dyn Write + Send>,
    rx: Receiver<Outgoing>,
    cancel: Receiver<()>,
) -> PtyIoThread {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let handle = thread::Builder::new()
        .name("sonic-pty-writer".into())
        .spawn(move || {
            let _active = ActivePtyIoThread::enter();
            loop {
                let bytes = crossbeam_channel::select! {
                    recv(cancel) -> _ => break,
                    recv(rx) -> result => match result {
                        Ok(bytes) => bytes,
                        Err(_) => break,
                    },
                };
                if let Err(e) = writer.write_all(&bytes) {
                    if cancel.try_recv().is_err() {
                        tracing::warn!("pty write error: {e}");
                    }
                    break;
                }
                let _ = writer.flush();
            }
            let _ = done_tx.send(());
        })
        // PANIC: see `spawn_reader_thread` rationale above — OS-level
        // thread-spawn failure at PTY init is unrecoverable.
        .expect("spawn pty writer");
    PtyIoThread { handle: Some(handle), done }
}

fn default_shell() -> String {
    default_shell_program()
}

/// Resolve the shell to spawn: an explicit, non-empty `[terminal] shell`
/// override wins; otherwise fall back to the platform auto-detect.
/// Pure so it can be unit-tested without the live filesystem/PATH.
fn resolve_spawn_shell(override_shell: Option<&str>) -> String {
    match override_shell {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => default_shell(),
    }
}

const DEFAULT_LANG_UTF8_LOCALE: &str = "en_US.UTF-8";
const DEFAULT_LC_CTYPE_UTF8_LOCALE: &str = "UTF-8";

/// Return startup arguments for the selected shell.
///
/// Production macOS shells are login shells so `/etc/zprofile` can run
/// `path_helper`, matching Terminal.app/iTerm2/WezTerm PATH behavior. Clean
/// E2E mode intentionally bypasses profiles for deterministic fixtures.
#[doc(hidden)]
pub fn shell_startup_args(shell_path: &str, opts: ShellSpawnOpts) -> Vec<String> {
    if opts.clean_e2e {
        clean_e2e_args(shell_path)
    } else {
        interactive_shell_args(shell_path)
    }
}

#[cfg(target_os = "macos")]
fn apply_terminal_locale_env(builder: &mut CommandBuilder) {
    let lc_all = builder.get_env("LC_ALL").and_then(|v| v.to_str());
    let lc_ctype = builder.get_env("LC_CTYPE").and_then(|v| v.to_str());
    let lang = builder.get_env("LANG").and_then(|v| v.to_str());

    if should_apply_utf8_locale_fallback(lc_all, lc_ctype, lang) {
        if is_empty_env(lang) {
            builder.env("LANG", DEFAULT_LANG_UTF8_LOCALE);
        }
        builder.env("LC_CTYPE", DEFAULT_LC_CTYPE_UTF8_LOCALE);
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_terminal_locale_env(_builder: &mut CommandBuilder) {}

#[doc(hidden)]
pub fn should_apply_utf8_locale_fallback(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> bool {
    if !is_empty_env(lc_all) {
        return false;
    }
    !is_utf8_locale(lc_ctype) && !is_utf8_locale(lang)
}

#[doc(hidden)]
pub const fn default_lang_utf8_locale() -> &'static str {
    DEFAULT_LANG_UTF8_LOCALE
}

#[doc(hidden)]
pub const fn default_lc_ctype_utf8_locale() -> &'static str {
    DEFAULT_LC_CTYPE_UTF8_LOCALE
}

fn is_empty_env(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or_default().is_empty()
}

fn is_utf8_locale(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    normalized.contains("utf-8") || normalized.contains("utf8")
}

#[cfg(target_os = "macos")]
fn interactive_shell_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "zsh" | "zsh.exe" | "tcsh" | "csh" => vec!["-l".to_string()],
        "bash" | "bash.exe" | "fish" | "fish.exe" => vec!["--login".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
fn interactive_shell_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell" => vec![
            "-NoLogo".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            "[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $OutputEncoding=[System.Text.UTF8Encoding]::new($false); chcp 65001 > $null".to_string(),
        ],
        _ => Vec::new(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn interactive_shell_args(_shell_path: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(target_os = "windows")]
fn default_shell_program() -> String {
    resolve_windows_default_shell_with(
        || path_lookup("pwsh.exe"),
        registered_pwsh,
        windowsapps_store_pwsh,
        || path_lookup("powershell.exe"),
    )
}

#[cfg(target_os = "windows")]
fn resolve_windows_default_shell_with<PathPwsh, RegisteredPwsh, StorePwsh, LegacyPwsh>(
    path_pwsh: PathPwsh,
    registered_pwsh: RegisteredPwsh,
    store_pwsh: StorePwsh,
    legacy_pwsh: LegacyPwsh,
) -> String
where
    PathPwsh: FnOnce() -> Option<String>,
    RegisteredPwsh: FnOnce() -> Option<String>,
    StorePwsh: FnOnce() -> Option<String>,
    LegacyPwsh: FnOnce() -> Option<String>,
{
    path_pwsh()
        .or_else(registered_pwsh)
        .or_else(store_pwsh)
        .or_else(legacy_pwsh)
        .unwrap_or_else(|| "cmd.exe".to_string())
}

#[cfg(target_os = "windows")]
fn registered_pwsh() -> Option<String> {
    use std::{ffi::c_void, os::windows::ffi::OsStringExt};

    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
        },
    };

    const MAX_PATH_BYTES: u32 = 64 * 1024 + 2;
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\pwsh.exe"),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS || bytes < 2 || bytes > MAX_PATH_BYTES {
        return None;
    }

    let mut value = vec![0u16; (bytes as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\pwsh.exe"),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }

    let units = (bytes as usize / 2).min(value.len());
    let end = value[..units].iter().position(|&unit| unit == 0).unwrap_or(units);
    let path = PathBuf::from(std::ffi::OsString::from_wide(&value[..end]));
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// Probe the Microsoft Store package directory for a real, executable
/// `pwsh.exe` (PowerShell 7). Returns the highest-versioned one, or `None`.
///
/// `BUILTIN\Users` has read+execute on `C:\Program Files\WindowsApps`, so a
/// normal (non-elevated) process can enumerate `Microsoft.PowerShell_*`.
/// Honors the `SONICTERM_ALLOW_WINDOWSAPPS_SHELL` escape hatch only to the
/// extent that this real package path is always allowed (it works under
/// ConPTY, unlike the per-user alias stub).
#[cfg(target_os = "windows")]
fn windowsapps_store_pwsh() -> Option<String> {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let windowsapps = program_files.join("WindowsApps");
    let entries = std::fs::read_dir(&windowsapps).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name();
        let lname = name.to_string_lossy().to_ascii_lowercase();
        // Match the PowerShell package family; skip the `_neutral_~_`
        // resource package (no real exe). The arch'd package
        // (`Microsoft.PowerShell_<ver>_x64__<pub>`) carries `pwsh.exe`.
        if !lname.starts_with("microsoft.powershell_") {
            continue;
        }
        let pwsh = dir.join("pwsh.exe");
        if pwsh.is_file() {
            candidates.push(pwsh);
        }
    }
    pick_highest_pwsh(&candidates).map(|p| p.to_string_lossy().to_string())
}

/// Pure selector: pick the highest-versioned `pwsh.exe` from candidate Store
/// package paths. Versions are embedded in the parent dir name
/// (`Microsoft.PowerShell_7.6.2.0_x64__...`); compare them numerically so
/// `7.10` sorts above `7.9`. Falls back to lexical path order when no version
/// parses. Returns `None` for an empty list.
#[cfg(target_os = "windows")]
fn pick_highest_pwsh(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .max_by(|a, b| {
            let va = store_pkg_version(a);
            let vb = store_pkg_version(b);
            va.cmp(&vb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
        })
        .cloned()
}

/// Extract the `[major, minor, patch, build]` version from a Store package
/// `pwsh.exe` path's parent dir name (`Microsoft.PowerShell_<ver>_<arch>__...`).
/// Returns all-zero when it can't be parsed, so unparseable entries sort below
/// real ones.
#[cfg(target_os = "windows")]
fn store_pkg_version(pwsh_path: &Path) -> [u64; 4] {
    let dir = pwsh_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // `Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe`
    let after = dir.split('_').nth(1).unwrap_or("");
    let mut out = [0u64; 4];
    for (i, part) in after.split('.').take(4).enumerate() {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn default_shell_program() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

#[cfg(target_os = "windows")]
fn path_lookup(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_string_lossy().to_string());
    }
    let path = std::env::var_os("PATH")?;
    let allow_windowsapps =
        std::env::var("SONICTERM_ALLOW_WINDOWSAPPS_SHELL").map(|v| v == "1").unwrap_or(false);
    std::env::split_paths(&path)
        .map(|dir: PathBuf| dir.join(name))
        .find(|candidate| {
            if !candidate.is_file() {
                return false;
            }
            // skip Microsoft Store WindowsApps stubs for `pwsh.exe` /
            // `powershell.exe`. The App Execution Alias produces zero output
            // under ConPTY when spawned bare, so the e2e gates silently fail.
            // Escape hatch: SONICTERM_ALLOW_WINDOWSAPPS_SHELL=1 to opt back in.
            let lname = name.to_ascii_lowercase();
            let is_powershell = lname.ends_with("pwsh.exe") || lname.ends_with("powershell.exe");
            if is_powershell && !allow_windowsapps {
                let lpath = candidate.to_string_lossy().to_lowercase();
                // Skip only per-user App Execution Alias stubs. The real
                // Microsoft Store PowerShell package also lives under a
                // WindowsApps directory (usually `C:\Program Files\WindowsApps\
                // Microsoft.PowerShell_*\pwsh.exe`) and works correctly under
                // ConPTY; skipping every `\WindowsApps\` path made SonicTerm
                // fall back to Windows PowerShell 5.1, whose PSReadLine redraw
                // path emits literal `?` bytes for CJK edits.
                if is_windowsapps_alias_stub_path(&lpath) {
                    return false;
                }
            }
            true
        })
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(target_os = "windows")]
fn is_windowsapps_alias_stub_path(lowercase_path: &str) -> bool {
    lowercase_path.contains("\\appdata\\local\\microsoft\\windowsapps\\")
}

/// Returns clean-startup args appropriate for the resolved shell. For
/// PowerShell (`pwsh.exe` / `powershell.exe`), emits `-NoLogo -NoProfile`.
/// For bash, emits `--norc --noprofile`. For zsh, emits `-f` (skips
/// `.zshrc` but NOT `.zshenv` — `.zshenv` is for required env setup,
/// and replacing `-f` with `--no-rcs` would be a behavior change rather
/// than a fix). Unknown shells get no args.
///
/// Used only when `ShellSpawnOpts::clean_e2e = true`.
pub(crate) fn clean_e2e_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell" => {
            vec!["-NoLogo".to_string(), "-NoProfile".to_string()]
        }
        "bash" | "bash.exe" => {
            vec!["--norc".to_string(), "--noprofile".to_string()]
        }
        "zsh" | "zsh.exe" => {
            vec!["-f".to_string()]
        }
        _ => Vec::new(),
    }
}

#[doc(hidden)]
pub fn apply_child_pty_env(builder: &mut CommandBuilder, term_program: &str) {
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    // Identify the terminal to programs that branch on TERM_PROGRAM
    // (e.g. Copilot CLI, shells, prompt frameworks). Mirrors iTerm2 /
    // WezTerm, which set TERM_PROGRAM + TERM_PROGRAM_VERSION.
    builder.env("TERM_PROGRAM", term_program);
    builder.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    apply_terminal_locale_env(builder);
}

fn shell_file_name(shell_path: &str) -> String {
    Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "pty_tests.rs"]
mod pty_tests;
