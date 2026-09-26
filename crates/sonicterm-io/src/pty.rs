//! Cross-platform PTY spawning.
//!
//! Wraps the `portable-pty` crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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

pub use crate::reply_spool::PtyReplySender;
use crate::reply_spool::{reply_spool, ReplyReader};

/// Outgoing message: bytes to write to the pty master (typed by user).
type Outgoing = Vec<u8>;
/// Incoming message: bytes read from the pty master (program output).
///
/// Wraps a [`bytes::Bytes`] — a refcounted slice — so the reader thread can
/// hand the buffer off to the VT thread without per-read `Vec::to_vec`
/// allocations. The reader keeps a single [`BytesMut`] ring of 64 KiB and
/// `split`s the filled prefix into a `Bytes` each iteration; once the
/// ring drains below capacity it reuses the same allocation.
///
/// The wrapper exists to carry that ring's charge: many views share one
/// allocation, and only the type holding them can tell when the last one goes.
type Incoming = PtyOutputChunk;

/// Maximum unread PTY output chunks retained per pane.
///
/// A slot holds a view into the reader's 64 KiB ring, not a buffer of its own,
/// and one ring backs many views. The bound that matters is therefore the
/// number of distinct rings the queued views pin: at most one per slot, so
/// 4 MiB worst case, and one ring — 64 KiB — for every real shell workload
/// measured. Once full, the reader blocks and lets the OS PTY apply
/// backpressure instead of growing process memory without limit.
pub const PTY_OUTPUT_QUEUE_CAPACITY: usize = 64;
/// Maximum pending terminal-input messages retained per pane.
pub const PTY_INPUT_QUEUE_CAPACITY: usize = 4;
/// Largest single terminal-input message accepted by first-party callers.
pub const MAX_PTY_INPUT_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const PTY_IO_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(windows)]
const CONPTY_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
static ACTIVE_PTY_IO_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Native handles reserved together for one PTY, including Windows cancellation duplicates.
#[cfg(windows)]
pub const PTY_NATIVE_HANDLE_DEMAND: usize = 8;
/// Native descriptors reserved together for one Unix PTY.
#[cfg(not(windows))]
pub const PTY_NATIVE_HANDLE_DEMAND: usize = 3;
/// Whole helper grant: outer teardown, drain, close, and two cancellation workers.
#[cfg(windows)]
pub const PTY_NATIVE_HELPER_DEMAND: usize = 5;
/// Unix teardown needs only its outer helper.
#[cfg(not(windows))]
pub const PTY_NATIVE_HELPER_DEMAND: usize = 1;
/// Teardown wait budget: shared cancel wait, child lock, two IO joins, close/drain waits, and reap wait.
#[cfg(windows)]
pub const PTY_TEARDOWN_TAIL_BOUND: Duration = PTY_IO_SHUTDOWN_TIMEOUT
    .saturating_mul(5)
    .saturating_add(CONPTY_CLOSE_TIMEOUT.saturating_mul(2));
/// Teardown wait budget: child lock, bounded session retries, master lock, two IO joins, and reap wait.
#[cfg(not(windows))]
pub const PTY_TEARDOWN_TAIL_BOUND: Duration =
    PTY_IO_SHUTDOWN_TIMEOUT.saturating_mul(5).saturating_add(Duration::from_millis(40));

const READER_PHASE: usize = 0;
const WRITER_PHASE: usize = 1;
const TERMINATE_PHASE: usize = 2;
const MASTER_PHASE: usize = 3;
const REAP_PHASE: usize = 4;
#[cfg(windows)]
const DRAIN_PHASE: usize = 5;
#[cfg(windows)]
const READER_CANCEL_PHASE: usize = 6;
#[cfg(windows)]
const WRITER_CANCEL_PHASE: usize = 7;

/// Concurrent observations of a bounded input queue and its single native writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyInputDiagnostics {
    /// Messages observed waiting, excluding the writer's current message.
    pub queued_messages: usize,
    /// Observed queued payload bytes, sampled separately from message count.
    pub queued_bytes: usize,
    /// Configured message-slot limit.
    pub queue_capacity: usize,
    /// Native writer phase observed independently of the queue.
    pub writer_phase: PtyWriterPhase,
    /// Payload bytes held by the writer outside the channel.
    pub in_flight_bytes: usize,
    /// Native writes whose write_all and flush both succeeded since startup.
    pub completed_messages: u64,
    /// Elapsed milliseconds for the observed write or flush, if active.
    pub in_flight_millis: Option<u64>,
}

/// Native input-writer activity, not a verdict on child-process health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PtyWriterPhase {
    /// Waiting to receive input or cancellation.
    Idle = 0,
    /// Inside the native write operation.
    Writing = 1,
    /// Inside the native flush operation.
    Flushing = 2,
    /// Writer has exited and cannot accept more work.
    Stopped = 3,
}

#[derive(Debug)]
struct PtyWriterProgress {
    closing: std::sync::atomic::AtomicBool,
    epoch: Instant,
    operation: AtomicU64,
    in_flight_bytes: AtomicUsize,
    completed_messages: AtomicU64,
}

impl PtyWriterProgress {
    fn new() -> Self {
        Self {
            closing: std::sync::atomic::AtomicBool::new(false),
            epoch: Instant::now(),
            operation: AtomicU64::new(0),
            in_flight_bytes: AtomicUsize::new(0),
            completed_messages: AtomicU64::new(0),
        }
    }

    // Ordering: `operation`, `in_flight_bytes`, and `completed_messages` use `Relaxed` for independent observations.
    fn observe(&self, queued_messages: usize, queued_bytes: usize) -> PtyInputDiagnostics {
        let operation = self.operation.load(Ordering::Relaxed);
        let phase = operation & 3;
        let writer_phase = match phase {
            1 => PtyWriterPhase::Writing,
            2 => PtyWriterPhase::Flushing,
            3 => PtyWriterPhase::Stopped,
            _ => PtyWriterPhase::Idle,
        };
        let in_flight_millis = if phase == 1 || phase == 2 {
            Some(self.elapsed_millis().saturating_sub(operation >> 2))
        } else {
            // When: `phase` is idle or stopped, no active operation has a meaningful elapsed time.
            None
        };
        PtyInputDiagnostics {
            queued_messages,
            queued_bytes,
            queue_capacity: PTY_INPUT_QUEUE_CAPACITY,
            writer_phase,
            in_flight_bytes: self.in_flight_bytes.load(Ordering::Relaxed),
            completed_messages: self.completed_messages.load(Ordering::Relaxed),
            in_flight_millis,
        }
    }

    fn elapsed_millis(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u128::from(u64::MAX >> 2)) as u64
    }

    // Ordering: `operation` uses `Relaxed` to pack phase and its timestamp without publishing payload data.
    fn begin(&self, phase: PtyWriterPhase) {
        self.operation.store((self.elapsed_millis() << 2) | phase as u64, Ordering::Relaxed);
    }
}

/// Owned, cloneable sender for a child process's input channel.
///
/// Wraps the bounded channel so a caller holding one cannot reach the raw
/// `Sender`. All paths enforce the same size cap and refuse rather than block.
/// Parser replies use the separate [`PtyReplySender`] spool.
#[derive(Clone, Debug)]
pub struct PtyInputSender {
    tx: Sender<Outgoing>,
    queued_bytes: Arc<AtomicUsize>,
    writer_progress: Arc<PtyWriterProgress>,
}

impl PtyInputSender {
    /// Queue input, refusing rather than blocking.
    ///
    /// # Errors
    ///
    /// Returns [`PtyInputError`] when the message exceeds the cap, the queue
    /// is full, or the writer has gone. The error retains the bytes so the
    /// caller can retry or report rather than losing them silently.
    pub fn send(&self, bytes: Vec<u8>) -> Result<(), PtyInputError> {
        try_queue_pty_input(&self.tx, &self.queued_bytes, bytes)
    }

    /// Whether the owning pane has begun intentional PTY teardown rather than a native writer failure.
    pub fn is_closing(&self) -> bool {
        self.writer_progress.closing.load(Ordering::SeqCst)
    }

    /// Sample queue occupancy and writer activity without blocking or inspecting input bytes.
    // Ordering: `queued_bytes` uses `Relaxed` for an observation independent of channel length and writer progress.
    #[must_use]
    pub fn diagnostics(&self) -> PtyInputDiagnostics {
        self.writer_progress.observe(self.tx.len(), self.queued_bytes.load(Ordering::Relaxed))
    }
}

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

/// Live totals for one pane's queued PTY output.
///
/// Two quantities, because they answer different questions and differ by three
/// orders of magnitude on a keystroke-echo queue: `ring_bytes` is the memory
/// the process cannot reclaim, `payload_bytes` is the data waiting to be
/// parsed. Both are maintained by the chunks themselves, so a sampler reads
/// them without touching the channel.
#[derive(Debug, Default)]
struct QueuedOutputMeter {
    ring_bytes: AtomicUsize,
    payload_bytes: AtomicUsize,
}

/// One ring allocation, charged while any chunk still views into it.
///
/// The reader splits many views out of a single 64 KiB ring, so charging per
/// chunk would multiply one allocation by the number of views taken from it.
/// This is held by `Arc` from every chunk sharing the ring: the first view
/// charges, and the last one to drop releases. `Drop` is what makes the figure
/// exact without anyone polling — the charge cannot outlive the memory.
#[derive(Debug)]
struct RingCharge {
    bytes: usize,
    meter: Arc<QueuedOutputMeter>,
}

impl RingCharge {
    // Ordering: `ring_bytes` uses `AcqRel` for atomic charge accounting; it does not guard ring memory.
    fn new(bytes: usize, meter: &Arc<QueuedOutputMeter>) -> Arc<Self> {
        meter.ring_bytes.fetch_add(bytes, Ordering::AcqRel);
        Arc::new(Self { bytes, meter: meter.clone() })
    }
}

// Lifecycle: dropping `RingCharge` releases its `ring_bytes` allocation charge.
impl Drop for RingCharge {
    // Ordering: `ring_bytes` uses `AcqRel` for atomic charge accounting; it does not synchronize ring release.
    fn drop(&mut self) {
        self.meter.ring_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// A chunk of child output, and the ring memory it keeps alive.
///
/// Derefs to `[u8]`, so callers read it exactly like the `Bytes` it wraps. The
/// reason it is not a bare `Bytes` is the charge: a view into a shared ring
/// holds down the whole allocation, and nothing outside this type can observe
/// when the last view of a ring goes away.
#[derive(Debug)]
pub struct PtyOutputChunk {
    bytes: Bytes,
    /// Kept for its `Drop`. The ring stays charged while this clone lives.
    _ring: Arc<RingCharge>,
    meter: Arc<QueuedOutputMeter>,
}

impl PtyOutputChunk {
    // Ordering: `payload_bytes` uses `AcqRel` for atomic payload accounting; it does not guard chunk data.
    fn new(bytes: Bytes, ring: Arc<RingCharge>, meter: &Arc<QueuedOutputMeter>) -> Self {
        meter.payload_bytes.fetch_add(bytes.len(), Ordering::AcqRel);
        Self { bytes, _ring: ring, meter: meter.clone() }
    }

    /// The output bytes, as a refcounted slice.
    ///
    /// Cloning the returned `Bytes` outlives this chunk's charge, so a caller
    /// that retains one holds ring memory the queue figure no longer reports.
    /// Parse from it and drop it; do not park it in a cache.
    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Build a chunk charged against `meter` as if split from a `ring_bytes`
    /// ring, for tests that drive the queue without a child process.
    #[cfg(test)]
    fn for_test(bytes: &'static [u8], ring_bytes: usize, meter: &Arc<QueuedOutputMeter>) -> Self {
        Self::new(Bytes::from_static(bytes), RingCharge::new(ring_bytes, meter), meter)
    }
}

// Lifecycle: dropping `PtyOutputChunk` releases its `payload_bytes` queue charge.
impl Drop for PtyOutputChunk {
    // Ordering: `payload_bytes` uses `AcqRel` for atomic payload accounting; it does not synchronize chunk release.
    fn drop(&mut self) {
        self.meter.payload_bytes.fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

impl std::ops::Deref for PtyOutputChunk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl AsRef<[u8]> for PtyOutputChunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
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
    progress: Arc<PtyWriterProgress>,
}

impl PtyChildExitProbe {
    /// Return whether the child has exited without waiting for it.
    ///
    /// On Unix, observing a pending child exit also signals the child's process
    /// group before returning `true`, so background descendants cannot survive
    /// after the shell leader exits.
    pub fn has_exited(&self) -> Result<bool> {
        let Some(mut child) = self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
            // When: child is owned by retirement, a probe must not wait beyond its bounded observation interval.
            return Ok(false);
        };
        if child.closing || self.progress.closing.load(Ordering::SeqCst) {
            // When: closing owns native cleanup, probes only observe the retained status and never reap the leader.
            return Ok(child.exited);
        }
        #[cfg(windows)]
        return Ok(child.has_exited()?);
        #[cfg(unix)]
        {
            if child.exited {
                // When: `child.exited` records an earlier observation, so no new OS probe is needed.
                return Ok(true);
            }
            let Some(pid) = child.process_id() else {
                // When: `process_id` is unavailable, Unix cannot inspect an unreaped leader yet.
                return Ok(false);
            };
            let observed = unix_child_exit_pending(pid)?;
            if !observed.pending {
                // When: `pending` is false while the leader still runs, so teardown must not start.
                return Ok(false);
            }
            // Recorded before signalling the group: teardown reaps the child,
            // after which the status is unrecoverable.
            child.exit_was_clean = observed.was_clean;
            signal_process_group_for_platform(&mut child)?;
            Ok(true)
        }
    }

    /// Whether the child's own exit was clean, once it has exited.
    ///
    /// `None` means unknown rather than bad: the child is still running, or
    /// it was reaped by a path that never saw a status. A caller deciding
    /// whether to tear down a pane must not read `None` as a crash —
    /// discarding a user's scrollback on our own uncertainty is a worse
    /// failure than leaving the pane open.
    ///
    /// Only meaningful after [`Self::has_exited`] returns `true`; that call
    /// is what records the status.
    pub fn exit_was_clean(&self) -> Option<bool> {
        self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT).and_then(|child| child.exit_was_clean)
    }
}

#[cfg(unix)]
/// Whether the child has exited, and if so whether it exited cleanly.
///
/// `waitid` already fills `siginfo_t` with the reason and status, so the
/// cleanliness comes free with the liveness check — reading only `si_pid`
/// would throw away the answer the kernel just handed us.
#[cfg(unix)]
struct UnixExitObservation {
    pending: bool,
    was_clean: Option<bool>,
}

#[cfg(unix)]
fn unix_child_exit_pending(pid: u32) -> std::io::Result<UnixExitObservation> {
    let mut info: libc::siginfo_t =
        // SAFETY: zeroed `siginfo_t` is valid writable storage for `waitid` to initialize.
        unsafe { std::mem::zeroed() };
    let result =
        // SAFETY: `info` is writable; `pid` identifies our child, and `WNOWAIT` preserves it for group signalling.
        unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        // When: `result` is -1, so preserve the kernel error instead of interpreting `info`.
        return Err(std::io::Error::last_os_error());
    }
    let pending =
        // SAFETY: `info` was zeroed first, so `si_pid` is valid whether or not `waitid` wrote a status.
        unsafe { info.si_pid() }
            != 0;
    if !pending {
        // When: `pending` is false, `WNOHANG` found no child exit status to record.
        return Ok(UnixExitObservation { pending: false, was_clean: None });
    }
    // `si_code` distinguishes a normal exit from a signal death; `si_status`
    // carries the exit code only in the former case. A child killed by a
    // signal is not a clean exit, and neither is a nonzero code.
    let (code, status) = (
        info.si_code,
        // SAFETY: pending exit status from `waitid` initialized the `si_status` union field.
        unsafe { info.si_status() },
    );
    let was_clean = match code {
        libc::CLD_EXITED => Some(status == 0),
        // Killed, dumped, trapped, stopped: not a clean exit, and we know it.
        _ => Some(false),
    };
    Ok(UnixExitObservation { pending: true, was_clean })
}

struct ChildState {
    child: Option<NativeValue<Box<dyn Child + Send + Sync>>>,
    closing: bool,
    exited: bool,
    /// Whether the child's own exit was clean, once it has exited.
    ///
    /// `None` while it is still running, and also when the exit was observed
    /// by a path that never saw a status — a kill we issued, or a platform
    /// probe that only reports liveness. A caller deciding whether to close a
    /// pane must treat `None` as "unknown", not as "crashed": tearing down a
    /// window because we could not read a status would lose the user's
    /// scrollback on our own uncertainty.
    exit_was_clean: Option<bool>,
    unix_session_id: Option<u32>,
    process_group_signalled: bool,
}

impl ChildState {
    fn new(child: Box<dyn Child + Send + Sync>, unix_session_id: Option<u32>) -> Self {
        Self {
            child: Some(NativeValue::new(child).0),
            closing: false,
            exited: false,
            exit_was_clean: None,
            unix_session_id,
            process_group_signalled: false,
        }
    }

    fn has_exited(&mut self) -> std::io::Result<bool> {
        if self.exited {
            // When: `self.exited` already consumed the status, so another `try_wait` cannot add information.
            return Ok(true);
        }
        let Some(child) = self.child.as_mut() else {
            // When: child custody was closed after reaping, retain the recorded exit observation.
            return Ok(self.exited);
        };
        if let Some(status) = child.value.as_mut().expect("owned child").try_wait()? {
            self.exited = true;
            // Recorded here because this is the only place the status is
            // available: `try_wait` reaps the child, so a later call returns
            // `None` and the status is gone for good.
            self.exit_was_clean = Some(status.success());
        }
        Ok(self.exited)
    }

    fn process_id(&self) -> Option<u32> {
        (!self.exited).then(|| self.child.as_ref()?.value.as_ref()?.process_id()).flatten()
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
        // When: `child` has exited after group signalling, so direct-pid termination is unnecessary.
        return Ok(());
    }
    if let Some(pid) = child.process_id() {
        signal_pid(pid);
    }
    child
        .child
        .as_mut()
        .expect("unreaped child custody")
        .value
        .as_mut()
        .expect("owned child")
        .kill()
}

fn signal_process_group<G>(child: &mut ChildState, mut signal_group: G) -> std::io::Result<()>
where
    G: FnMut(u32) -> std::io::Result<()>,
{
    if child.process_group_signalled {
        // When: `process_group_signalled` prevents duplicate group kills across teardown retries.
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

    let mut members = Vec::new();
    for pid in pids_by_type(ProcFilter::All)? {
        if pid == 0 {
            // When: `pid` is 0, the kernel task is no session member, and `getsid(0)` would read our own session.
            continue;
        }
        if (
            // SAFETY: nonzero `pid` came from the process table; `getsid` only reads its session id.
            unsafe { libc::getsid(pid as libc::pid_t) }
        ) != session_id as libc::pid_t
        {
            // When: `getsid` does not match `session_id`, including `ESRCH` for a zombie or exiting process, `pid` is not listed.
            continue;
        }
        if !unix_process_is_active(pid)? {
            // When: `unix_process_is_active(pid)` is false, this zombie or reaped member needs no signal.
            continue;
        }
        members.push(pid);
    }
    Ok(members)
}

/// One read of a PTY session member's macOS process-table entry.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum MacosProcessState {
    /// The pid has no process-table entry (`ESRCH`): the member was reaped.
    Gone,
    /// The kernel returned the member's entry.
    Read {
        /// The parent pid when read; `1` once `launchd` has adopted an orphan.
        ppid: u32,
        /// The process group id when read; a member still in the session's group has the session's id.
        pgid: u32,
        /// The kernel `p_stat` value, such as `SRUN` or `SZOMB`.
        status: u32,
        /// Whether the kernel flags the member as working its way through exit.
        in_exit: bool,
        /// The kernel's command name, at most 16 bytes.
        command: String,
    },
    /// The entry could not be read; `errno` is 0 for an incomplete record.
    Unreadable { errno: i32 },
}

/// `PROC_FLAG_INEXIT` from `<sys/proc_info.h>`, which `libc` does not export.
#[cfg(target_os = "macos")]
const MACOS_PROC_FLAG_INEXIT: u32 = 0x4;

/// Reads a session member's state, command, and in-exit flag.
///
/// `PROC_PIDT_SHORTBSDINFO` has no same-user check, so a member that changed
/// credentials stays readable, and argument 1 also finds zombies, for which
/// `getsid` reports `ESRCH`.
#[cfg(target_os = "macos")]
fn macos_process_state(pid: u32) -> MacosProcessState {
    let mut info: libc::proc_bsdshortinfo =
        // SAFETY: `proc_bsdshortinfo` holds only integers and byte arrays, so all-zero bytes are a valid value.
        unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    let written =
        // SAFETY: `info` is writable storage of exactly `size` bytes, and the call writes at most `size` bytes.
        unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDT_SHORTBSDINFO,
                1,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
    if written == size {
        // When: `written == size`, the kernel filled the whole record, so every field is valid.
        let command = info
            .pbsi_comm
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .take_while(|byte| *byte != 0)
            .collect::<Vec<u8>>();
        return MacosProcessState::Read {
            ppid: info.pbsi_ppid,
            pgid: info.pbsi_pgid,
            status: info.pbsi_status,
            in_exit: info.pbsi_flags & MACOS_PROC_FLAG_INEXIT != 0,
            command: String::from_utf8_lossy(&command).into_owned(),
        };
    }
    if written > 0 {
        // When: `written` is positive but short, the record is incomplete and no errno describes it.
        return MacosProcessState::Unreadable { errno: 0 };
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => MacosProcessState::Gone,
        errno => MacosProcessState::Unreadable { errno: errno.unwrap_or(0) },
    }
}

/// Whether a macOS session member still needs a signal and a report.
///
/// Only a zombie or a reaped pid is inactive, as Linux drops `Z`, `X`, and `x`.
/// An unreadable member stays active, so cleanup never reports success it could
/// not observe. A member flagged in exit also stays, since it can still hold the
/// PTY, although the scan's earlier `getsid` match can already drop it.
#[cfg(target_os = "macos")]
fn macos_state_is_active(state: &MacosProcessState) -> bool {
    !matches!(state, MacosProcessState::Gone | MacosProcessState::Read { status: libc::SZOMB, .. })
}

/// Names a surviving session member for the termination error.
///
/// Its parent, process group, kernel state, in-exit flag, and command are one
/// fresh read when the error is built, not proof of ancestry or identity, and
/// `kill=` is the outcome of the latest bounded pass that listed it.
#[cfg(target_os = "macos")]
fn describe_session_member(pid: u32, last_signal: KillOutcome) -> String {
    format_session_survivor(pid, &macos_process_state(pid), last_signal)
}

/// Formats one survivor from its process-table state and latest signal outcome.
///
/// `kill=` describes the latest pass that listed the pid: `ok` means the kernel
/// accepted SIGKILL, an errno that it refused, and `skipped-recheck` that the
/// pre-signal recheck skipped it; `unlisted` means no pass listed it before the
/// final scan. Missing history is never errno 0.
#[cfg(target_os = "macos")]
fn format_session_survivor(
    pid: u32,
    state: &MacosProcessState,
    last_signal: KillOutcome,
) -> String {
    let kill = kill_text(last_signal);
    match state {
        MacosProcessState::Read { ppid, pgid, status, in_exit, command } => format!(
            "pid={pid} ppid={ppid} pgid={pgid} state={} in_exit={in_exit} comm={command:?} kill={kill}",
            macos_status_name(*status)
        ),
        MacosProcessState::Gone => format!("pid={pid} state=gone kill={kill}"),
        MacosProcessState::Unreadable { errno: 0 } => {
            format!("pid={pid} state=unreadable(short record) kill={kill}")
        }
        MacosProcessState::Unreadable { errno } => {
            format!("pid={pid} state=unreadable({}) kill={kill}", errno_name(*errno))
        }
    }
}

/// Names a SIGKILL outcome for the survivor report.
#[cfg(target_os = "macos")]
fn kill_text(signal: KillOutcome) -> String {
    match signal {
        KillOutcome::Sent => "ok".to_owned(),
        KillOutcome::Refused(Some(errno)) => errno_name(errno),
        KillOutcome::Refused(None) => "refused".to_owned(),
        KillOutcome::SkippedRecheck => "skipped-recheck".to_owned(),
        KillOutcome::Unlisted => "unlisted".to_owned(),
    }
}

/// Reports the session's group SIGKILL result once, after the survivor list.
#[cfg(target_os = "macos")]
fn describe_group_kill(group_kill: KillOutcome) -> String {
    format!("; group_kill={}", kill_text(group_kill))
}

/// Names an errno for the survivor report, falling back to its number.
#[cfg(target_os = "macos")]
fn errno_name(errno: i32) -> String {
    match errno {
        libc::EPERM => "EPERM".to_owned(),
        libc::ESRCH => "ESRCH".to_owned(),
        libc::EINVAL => "EINVAL".to_owned(),
        other => format!("errno {other}"),
    }
}

/// Names a macOS `p_stat` value.
#[cfg(target_os = "macos")]
fn macos_status_name(status: u32) -> String {
    match status {
        libc::SIDL => "SIDL".to_owned(),
        libc::SRUN => "SRUN".to_owned(),
        libc::SSLEEP => "SSLEEP".to_owned(),
        libc::SSTOP => "SSTOP".to_owned(),
        libc::SZOMB => "SZOMB".to_owned(),
        other => format!("p_stat {other}"),
    }
}

/// Names a surviving session member by pid; only macOS reports its ids, state, command, and kill outcome here.
#[cfg(all(any(unix, test), not(target_os = "macos")))]
fn describe_session_member(pid: u32, _last_signal: KillOutcome) -> String {
    pid.to_string()
}

/// Adds nothing on other platforms, which keep the pid-only survivor list.
#[cfg(all(any(unix, test), not(target_os = "macos")))]
fn describe_group_kill(_group_kill: KillOutcome) -> String {
    String::new()
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn linux_proc_stat_is_active(stat: &str) -> Option<bool> {
    let (_, suffix) = stat.rsplit_once(") ")?;
    let state = suffix.as_bytes().first().copied()?;
    Some(!matches!(state, b'Z' | b'X' | b'x'))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_process_is_active(pid: u32) -> std::io::Result<bool> {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => Ok(linux_proc_stat_is_active(&stat).unwrap_or(true)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether `pid` is still a live macOS session member; only zombies and reaped pids are not.
#[cfg(target_os = "macos")]
fn unix_process_is_active(pid: u32) -> std::io::Result<bool> {
    Ok(macos_state_is_active(&macos_process_state(pid)))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_session_pids(session_id: u32) -> std::io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            // When: a `/proc` entry is not a numeric `pid`, it cannot identify a session member.
            continue;
        };
        if (
            // SAFETY: `pid` was parsed from a `/proc` process entry; `getsid` only reads its session id.
            unsafe { libc::getsid(pid as libc::pid_t) }
        ) == session_id as libc::pid_t
        {
            // When: `libc::getsid(pid as libc::pid_t) == session_id as libc::pid_t`, inspect this session member's state.
            if !unix_process_is_active(pid)? {
                // When: `unix_process_is_active(pid)` is false, this terminated session member needs no signal.
                continue;
            }
            pids.push(pid);
        }
    }
    Ok(pids)
}

#[cfg(unix)]
fn terminate_unix_session(session_id: u32) -> std::io::Result<()> {
    // Signal the shell's original process group even if process-table access
    // is restricted.
    let group_sent =
        // SAFETY: negative `session_id` targets the child-created process group; `kill` receives no pointers.
        unsafe { libc::kill(-(session_id as libc::pid_t), libc::SIGKILL) };
    let group_kill = kill_outcome(group_sent);
    terminate_session_members(
        group_kill,
        || {
            Ok(unix_session_pids(session_id)?
                .into_iter()
                .filter(|pid| *pid != session_id && *pid != std::process::id())
                .collect())
        },
        |pid| {
            // Recheck membership immediately before signalling.
            if
            // SAFETY: pid was just enumerated; getsid reads its session without changing process state.
            unsafe { libc::getsid(pid as libc::pid_t) } != session_id as libc::pid_t {
                // When: getsid no longer matches session_id, pid reuse makes signalling this process unsafe.
                return KillOutcome::SkippedRecheck;
            }
            let sent =
                // SAFETY: membership was rechecked immediately above; kill receives the member pid by value.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            kill_outcome(sent)
        },
        || std::thread::sleep(Duration::from_millis(5)),
    )
}

/// The outcome of a SIGKILL, kept for the survivor report.
///
/// For a member it is the latest bounded pass that listed it, which overwrites
/// earlier passes: `Sent` means the kernel accepted SIGKILL, `Refused` that it
/// rejected it, `SkippedRecheck` that the pre-signal recheck skipped the pid, and
/// `Unlisted` that no pass listed it before the final scan. The group kill uses
/// only `Sent` and `Refused`.
#[cfg(any(unix, test))]
// Only macOS reads the outcome into the survivor report; other platforms keep the pid list.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KillOutcome {
    /// The kernel accepted SIGKILL.
    Sent,
    /// `kill` failed with the errno read right after the call; `None` if the OS reported none.
    Refused(Option<i32>),
    /// The pre-signal membership recheck no longer matched, so the pid was not signalled.
    SkippedRecheck,
    /// The member was first listed by the final scan, after every signal pass.
    Unlisted,
}

/// Converts a `kill` return value into its outcome.
///
/// Call it right after `kill`, before another call can overwrite errno.
#[cfg(unix)]
fn kill_outcome(result: libc::c_int) -> KillOutcome {
    if result == 0 {
        // When: `result` is 0, the kernel accepted SIGKILL.
        return KillOutcome::Sent;
    }
    KillOutcome::Refused(std::io::Error::last_os_error().raw_os_error())
}

#[cfg(any(unix, test))]
fn terminate_session_members(
    group_kill: KillOutcome,
    mut members: impl FnMut() -> std::io::Result<Vec<u32>>,
    mut signal: impl FnMut(u32) -> KillOutcome,
    mut pause: impl FnMut(),
) -> std::io::Result<()> {
    // Each member's latest outcome, so a survivor shows whether its last signal was accepted.
    let mut last_signal = std::collections::HashMap::new();
    for _ in 0..8 {
        let remaining = members()?;
        if remaining.is_empty() {
            // When: remaining is empty, every descendant is gone and session cleanup is complete.
            return Ok(());
        }
        for pid in remaining {
            last_signal.insert(pid, signal(pid));
        }
        pause();
    }
    let remaining = members()?;
    if remaining.is_empty() {
        // When: remaining is empty after the last pause, the final signal completed within the existing attempt budget.
        return Ok(());
    }
    // State is re-read after the final scan, so on macOS a survivor that exits in between reads `state=gone`.
    // A pid no pass listed before the final scan reads `kill=unlisted`.
    let survivors = remaining
        .iter()
        .map(|pid| {
            let last = last_signal.get(pid).copied().unwrap_or(KillOutcome::Unlisted);
            describe_session_member(*pid, last)
        })
        .collect::<Vec<_>>();
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "PTY session still has live descendants after termination attempts: [{}]{}",
            survivors.join(", "),
            describe_group_kill(group_kill)
        ),
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

/// Stop the retained child without calling try_wait, so cancellation cannot release the leader identity.
fn terminate_child_without_reap(child: &mut ChildState) -> std::io::Result<()> {
    #[cfg(unix)]
    signal_process_group_for_platform(child)?;
    if child.exited {
        // When: exited already records a reaped leader, no native termination is needed.
        return Ok(());
    }
    let Some(native) = child.child.as_mut().and_then(|child| child.value.as_mut()) else {
        // When: child custody is absent after native release, no process handle remains to terminate.
        return Ok(());
    };
    #[cfg(unix)]
    {
        if let Some(pid) = native.process_id() {
            // When: process_id exposes the retained leader, signal it without a status-consuming wait.
            let result =
                // SAFETY: native retains our unreaped child identity; kill never reaps or releases it.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                // When: result failed for a reason other than ESRCH, preserve the termination error for retry.
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::HANDLE,
            System::Threading::{GetExitCodeProcess, TerminateProcess},
        };
        let raw = native.as_raw_handle().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "PTY child has no retained process handle",
            )
        })?;
        let handle = HANDLE(raw);
        let mut status = 0u32;
        let observed =
            // SAFETY: native retains this exact process handle under child custody; no PID lookup substitutes another process.
            unsafe { GetExitCodeProcess(handle, &mut status) };
        if observed.is_err() {
            // When: observed fails, report the native query error instead of guessing whether the child exited.
            return Err(std::io::Error::last_os_error());
        }
        if status != 259 {
            // When: status is not STILL_ACTIVE, natural exit already completed without reaping the retained child.
            return Ok(());
        }
        let terminated =
            // SAFETY: handle is the same retained child queried above; termination does not reap or close its identity.
            unsafe { TerminateProcess(handle, 1) };
        if terminated.is_err() {
            // When: terminated fails, only a confirmed concurrent natural exit can turn that failure into success.
            let error = std::io::Error::last_os_error();
            let observed =
                // SAFETY: native still retains handle while checking whether exit raced the failed termination.
                unsafe { GetExitCodeProcess(handle, &mut status) };
            if observed.is_ok() && status != 259 {
                // When: observed sees a terminal status on the same handle, termination has nothing left to stop.
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    native.kill()
}

fn pty_output_channel() -> (Sender<Incoming>, Receiver<Incoming>) {
    crossbeam_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY)
}

fn pty_input_channel() -> (Sender<Outgoing>, Receiver<Outgoing>) {
    crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY)
}

// Ordering: `queued_bytes` uses `Relaxed`; channel ownership supplies ordering, while the atomic tracks byte accounting only.
fn try_queue_pty_input(
    tx: &Sender<Outgoing>,
    queued_bytes: &AtomicUsize,
    bytes: Vec<u8>,
) -> Result<(), PtyInputError> {
    if !pty_input_message_allowed(bytes.len()) {
        // When: `bytes` exceeds the cap, reject it before charging or entering the bounded queue.
        return Err(PtyInputError::MessageTooLarge(bytes));
    }
    // Counted before the send, because once the message is in the channel the
    // writer thread may drain it before this line would otherwise run — which
    // would decrement a counter that had never been incremented.
    let len = bytes.len();
    queued_bytes.fetch_add(len, Ordering::Relaxed);
    tx.try_send(bytes).map_err(|error| {
        // The message never entered the queue, so it is not queued memory.
        queued_bytes.fetch_sub(len, Ordering::Relaxed);
        match error {
            crossbeam_channel::TrySendError::Full(bytes) => PtyInputError::QueueFull(bytes),
            crossbeam_channel::TrySendError::Disconnected(bytes) => {
                PtyInputError::WriterDisconnected(bytes)
            }
        }
    })
}

/// Reports whether one input message fits the enforced per-message byte cap.
#[must_use]
pub fn pty_input_message_allowed(bytes: usize) -> bool {
    bytes <= MAX_PTY_INPUT_MESSAGE_BYTES
}

/// Worst-case bytes that can wait in one pane's PTY input channel.
///
/// The product of the slot count and the per-message cap. This is the bound,
/// not the occupancy: [`PtyHandle::queued_input_bytes`] reports what is
/// actually held.
#[must_use]
pub const fn max_pty_queued_input_bytes() -> usize {
    PTY_INPUT_QUEUE_CAPACITY.saturating_mul(MAX_PTY_INPUT_MESSAGE_BYTES)
}

/// Ring memory this pane's queued PTY output holds down.
///
/// The reader hands out `Bytes` views into a reused 64 KiB ring, so neither
/// obvious arithmetic describes the queue. The slot count times a chunk size
/// counts buffers that were never allocated; the sum of the view lengths
/// counts payload while ignoring the ring those views pin. Measured on a full
/// 64-slot queue from `/bin/sh`: 64 bytes of keystroke echo, 65,536 bytes of
/// ring — the payload figure would have understated the memory 1024x.
///
/// This reports the ring: the bytes that stay allocated until the queue
/// drains, which is what the governor is deciding about. Use
/// [`queued_output_payload_bytes`] for the data waiting to be parsed.
///
/// Maintained by the chunks as they are created and dropped, so this observes
/// without consuming and may be sampled from any thread without disturbing the
/// pump.
// Ordering: `ring_bytes` uses `Acquire` to sample the atomic accounting value, not to guard ring memory.
#[must_use]
pub fn queued_output_bytes(handle: &PtyHandle) -> usize {
    handle.output_meter.ring_bytes.load(Ordering::Acquire)
}

/// Payload bytes waiting in this pane's PTY output channel.
///
/// The sum of the queued view lengths — what the VT thread still has to parse.
/// This is not the memory cost: 64 keystroke echoes are 64 bytes here and
/// 65,536 bytes of pinned ring. [`queued_output_bytes`] reports the latter.
// Ordering: `payload_bytes` uses `Acquire` to sample the atomic accounting value, not to guard payload data.
#[must_use]
pub fn queued_output_payload_bytes(handle: &PtyHandle) -> usize {
    handle.output_meter.payload_bytes.load(Ordering::Acquire)
}

/// Reader ring size, and the granularity of [`queued_output_bytes`].
///
/// The reader fills one allocation of this size and splits views out of it
/// until the remaining headroom is too small for another read, so a queue's
/// pinned total is always a multiple of this.
pub const PTY_READ_RING_BYTES: usize = 64 * 1024;

/// Worst-case ring memory one pane's output queue can pin.
///
/// Reached only if every queued view came from a different ring, which needs
/// reads large enough to exhaust an allocation each. Real shells do not do
/// this: every `/bin/sh` workload measured pinned exactly one ring, because
/// PTY reads arrive far smaller than the ring.
#[must_use]
pub const fn max_queued_output_ring_bytes() -> usize {
    PTY_OUTPUT_QUEUE_CAPACITY.saturating_mul(PTY_READ_RING_BYTES)
}

// Ordering: `ACTIVE_PTY_IO_THREADS` uses `Acquire` only for the test-visible atomic thread count.
#[cfg(all(test, windows))]
#[must_use]
fn active_pty_io_threads() -> usize {
    ACTIVE_PTY_IO_THREADS.load(Ordering::Acquire)
}

struct ActivePtyIoThread;

impl ActivePtyIoThread {
    // Ordering: `ACTIVE_PTY_IO_THREADS` uses `AcqRel` for counting; only tests read it, so it orders no production state.
    fn enter() -> Self {
        ACTIVE_PTY_IO_THREADS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

// Lifecycle: dropping `ActivePtyIoThread` releases its `ACTIVE_PTY_IO_THREADS` count.
impl Drop for ActivePtyIoThread {
    // Ordering: `ACTIVE_PTY_IO_THREADS` uses `AcqRel` for counting; only tests read it, so it orders no production state.
    fn drop(&mut self) {
        ACTIVE_PTY_IO_THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PtyIoThread {
    handle: Option<thread::JoinHandle<()>>,
    done: Receiver<()>,
    failed: bool,
}

impl PtyIoThread {
    /// Duplicate this thread's handle so another thread can cancel its synchronous I/O.
    ///
    /// Returns `None` when the thread was already finished or detached, or when duplication fails.
    #[cfg(windows)]
    fn cancel_target(&self) -> Option<std::os::windows::io::OwnedHandle> {
        use std::os::windows::io::AsHandle;

        let Some(handle) = self.handle.as_ref() else {
            // When: `handle` absent means the thread was finished or detached, leaving no I/O to cancel.
            return None;
        };
        // A failed duplication leaves this thread's I/O uncancelled; `finish` still bounds the wait for it.
        handle
            .as_handle()
            .try_clone_to_owned()
            .inspect_err(|error| tracing::warn!(%error, "could not duplicate a PTY I/O thread handle for cancellation"))
            .ok()
    }

    fn finish(&mut self, name: &'static str) {
        let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
        while self.handle.as_ref().is_some_and(|handle| !handle.is_finished()) {
            if Instant::now() >= deadline {
                // When: deadline expires before is_finished, keep the handle and native custody for the next teardown pass.
                tracing::warn!("{name} did not exit within the PTY shutdown timeout");
                return;
            }
            // A done message can precede native capture destruction; it is only a wake hint, never permission to join.
            let _ = self.done.try_recv();
            thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                // An unwind never proves successful native cleanup, even though the thread returned.
                self.failed = true;
                tracing::warn!("{name} panicked during shutdown");
            }
        }
    }
}

/// Cancel the pending synchronous I/O of the thread whose duplicated handle is `duplicate`.
#[cfg(windows)]
fn cancel_thread_io(name: &'static str, duplicate: std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::{
        Foundation::{ERROR_NOT_FOUND, HANDLE},
        System::IO::CancelSynchronousIo,
    };

    let result =
        // SAFETY: duplicate owns the target thread handle for this call; cancellation affects only its pending synchronous IO.
        unsafe { CancelSynchronousIo(HANDLE(duplicate.as_raw_handle())) };
    match result {
        Ok(()) => true,
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_NOT_FOUND.0) => true,
        Err(error) => {
            // Errors other than ERROR_NOT_FOUND do not prove successful cancellation.
            tracing::warn!(%error, thread = name, "PTY synchronous IO cancellation failed");
            false
        }
    }
}

/// Run slotless cancellation under one shared deadline, retaining recovery slots and unfinished join handles.
///
/// Targets include their opaque duplicate tokens. Neither expiry nor OS spawn refusal can destroy sole custody.
#[cfg(windows)]
fn cancel_io_within<T: Send + 'static>(
    targets: &[(&'static str, Arc<Mutex<Option<T>>>)],
    workers: &mut [NativeWorker],
    spawner: &dyn sonicterm_types::lifecycle::NativeWorkerSpawner,
    deadline: Instant,
    cancel: fn(&'static str, T) -> bool,
) -> std::io::Result<bool> {
    let mut spawn_error = None;
    for (index, ((name, target), worker)) in targets.iter().zip(workers.iter_mut()).enumerate() {
        if target.lock().is_none() {
            // When: target is empty, its worker already consumed custody or no cancellation was needed.
            continue;
        }
        let name = *name;
        if let Err(error) = worker.start(
            spawner,
            3 + index,
            "sonic-pty-cancel",
            target.clone(),
            Arc::new(AtomicBool::new(false)),
            move |target| cancel(name, target),
        ) {
            // Spawn refusal keeps the target in its recovery slot while the sibling cancel may start.
            spawn_error = Some(error);
        }
    }
    let mut complete = true;
    for worker in workers {
        if worker.handle.is_some() {
            complete &= worker.wait_until(deadline);
        }
        complete &= !worker.failed;
    }
    match spawn_error {
        Some(error) => Err(error),
        None => Ok(complete),
    }
}

#[cfg(windows)]
fn cancel_owned_target(
    name: &'static str,
    mut native: NativeValue<std::os::windows::io::OwnedHandle>,
) -> bool {
    let succeeded =
        cancel_thread_io(name, native.value.take().expect("owned cancellation duplicate"));
    if !succeeded {
        // Native close returns its permit but cannot credit a failed cancellation phase.
        native.phase = None;
    }
    drop(native);
    succeeded
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
    ///
    /// **Private deliberately.** This is the raw bounded seam: sending on it
    /// directly skips the message-size cap and blocks the calling thread when
    /// the queue is full. Measured — typed refuses an oversized message, raw
    /// accepts it; typed refuses a full queue, raw blocks on it.
    ///
    /// Use [`PtyHandle::send_input_nonblocking`] for terminal input, or
    /// [`PtyHandle::reply_sender`] for a thread that forwards parser replies.
    in_tx: Sender<Outgoing>,
    /// Bytes currently waiting in `in_tx`, maintained exactly.
    ///
    /// The queue holds `Vec<u8>` messages of any size up to the per-message
    /// cap, so a slot count says nothing about bytes held. Incremented when a
    /// message enters the queue and decremented when the writer thread takes
    /// it out, both inside this crate — there is no consumer that could forget
    /// to account for one.
    queued_input_bytes: Arc<AtomicUsize>,
    writer_progress: Arc<PtyWriterProgress>,
    replies: PtyReplySender,
    /// Closure that resizes the pty to `(cols, rows)`, reporting native failure.
    pub resize: Box<dyn Fn(u16, u16) -> Result<()> + Send + Sync>,
    teardown: Option<PtyTeardown>,
    /// Resolved shell program path (the command we actually spawned).
    shell_program_path: String,
    /// Live ring/payload totals for `out_rx`, maintained by the chunks in it.
    output_meter: Arc<QueuedOutputMeter>,
}

/// Options controlling how `spawn_default_shell` constructs the shell
/// command line. Default is interactive behavior (preserve user profile,
/// banner, prompt). E2E gates / examples that need deterministic output
/// pass `clean_e2e: true` to suppress profile/logo and emit shell-family-
/// specific clean-startup args.
///
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
    /// Working-directory override. `None` preserves the existing `$HOME` fallback.
    pub cwd: Option<PathBuf>,
}

impl ShellSpawnOpts {
    /// Production default `TERM_PROGRAM` value.
    pub const DEFAULT_TERM_PROGRAM: &'static str = "SonicTerm";
}

impl Default for ShellSpawnOpts {
    fn default() -> Self {
        Self {
            clean_e2e: false,
            term_program: Self::DEFAULT_TERM_PROGRAM.to_string(),
            shell: None,
            cwd: None,
        }
    }
}

impl PtyHandle {
    /// Explicitly terminate the child shell. Idempotent — second call is a
    /// no-op because the underlying handle will report it's already gone.
    /// Called automatically on Drop, but exposed for callers that want
    /// deterministic shutdown earlier.
    ///
    /// # Errors
    ///
    /// Returns the platform termination error so explicit shutdown callers can
    /// report or retry a child that could not be stopped.
    pub fn kill(&self) -> std::io::Result<()> {
        let mut child = self
            .teardown
            .as_ref()
            .expect("live PTY custody")
            .child
            .try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "PTY child custody is busy")
            })?;
        terminate_child_for_platform(&mut child)
    }

    /// Install one atomically admitted native-handle unit; already-closed values immediately return their permits.
    pub fn install_native_permits(
        &mut self,
        permits: Vec<sonicterm_types::lifecycle::NativePermit>,
    ) -> Result<()> {
        let teardown = self.teardown.as_mut().expect("live PTY custody");
        anyhow::ensure!(!teardown.permits_installed, "PTY native permits already installed");
        anyhow::ensure!(
            permits.len() == PTY_NATIVE_HANDLE_DEMAND,
            "PTY native permit count does not match the complete unit"
        );
        teardown.permits_installed = true;
        for (slot, permit) in teardown.permit_slots.iter().zip(permits) {
            let mut custody = slot.lock();
            if !custody.closed {
                custody.permits.push(permit);
            }
        }
        Ok(())
    }

    /// Publish intentional closure and move native custody without making native calls or joining workers.
    pub fn into_teardown(mut self) -> PtyTeardown {
        self.writer_progress.closing.store(true, Ordering::SeqCst);
        let mut teardown = self.teardown.take().expect("live PTY custody");
        teardown.resize = Some(std::mem::replace(&mut self.resize, Box::new(|_, _| Ok(()))));
        teardown
    }

    /// Process id of the underlying shell, if the platform reports it. Used
    /// by the tab-title renderer to probe the foreground process running in
    /// this pane's pty (e.g. "zsh" vs "nvim" vs "ssh"). Returns `None` once
    /// SonicTerm has observed exit; a natural exit can remain visible until
    /// the next child-exit probe.
    pub fn pid(&self) -> Option<u32> {
        self.teardown.as_ref()?.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT)?.process_id()
    }

    /// Resolved shell program path (the command we actually spawned).
    pub fn shell_program_path(&self) -> &str {
        &self.shell_program_path
    }

    /// Build a cloneable probe for consumers that must observe natural exit
    /// even when a platform PTY reader remains blocked until master teardown.
    pub fn child_exit_probe(&self) -> PtyChildExitProbe {
        PtyChildExitProbe {
            child: self.teardown.as_ref().expect("live PTY custody").child.clone(),
            progress: self.writer_progress.clone(),
        }
    }

    /// Queue terminal input without blocking the event-loop thread.
    ///
    /// On failure, the error retains the rejected bytes so the caller can
    /// retry or notify the user instead of silently losing terminal input.
    pub fn send_input_nonblocking(&self, bytes: Vec<u8>) -> Result<(), PtyInputError> {
        try_queue_pty_input(&self.in_tx, &self.queued_input_bytes, bytes)
    }

    /// Bytes waiting in this pane's PTY input channel.
    ///
    /// The exact figure, not a slot count times an assumed message size. A
    /// single paste is accepted up to the per-message cap and broadcast to
    /// every pane, so the difference between "four slots" and the bytes in
    /// them is the difference between kilobytes and tens of megabytes.
    ///
    /// Observes without consuming, so any thread may sample it.
    // Ordering: `queued_input_bytes` uses `Relaxed`; it is an accounting snapshot, not a synchronization edge.
    #[must_use]
    pub fn queued_input_bytes(&self) -> usize {
        self.queued_input_bytes.load(Ordering::Relaxed)
    }

    /// An owned, size-capped input sender for a caller that cannot borrow the handle.
    #[must_use]
    pub fn input_sender(&self) -> PtyInputSender {
        PtyInputSender {
            tx: self.in_tx.clone(),
            queued_bytes: self.queued_input_bytes.clone(),
            writer_progress: self.writer_progress.clone(),
        }
    }

    /// Clone the dedicated reply spool sender, independent of UI input capacity.
    #[must_use]
    pub fn reply_sender(&self) -> PtyReplySender {
        self.replies.clone()
    }

    /// Sample queue occupancy and native writer progress; fields are concurrent observations, not one transaction.
    #[must_use]
    pub fn input_diagnostics(&self) -> PtyInputDiagnostics {
        self.writer_progress.observe(self.in_tx.len(), self.queued_input_bytes())
    }
}

// Lifecycle: PtyHandle drops only its optional teardown custody; an extracted payload leaves no native work on the caller.
impl Drop for PtyHandle {
    fn drop(&mut self) {
        if let Some(mut teardown) = self.teardown.take() {
            // Direct fixture callers retain bounded inline cleanup while native custody remains attached.
            teardown.resize = Some(std::mem::replace(&mut self.resize, Box::new(|_, _| Ok(()))));
            drop(teardown);
        }
    }
}

/// Native resize plus the last size it accepted, under one lock.
struct ResizeState {
    apply: Box<dyn FnMut(u16, u16) -> Result<()> + Send>,
    last_applied: Option<(u16, u16)>,
}

/// Build the pty resize callback over the native `apply`, skipping repeat sizes.
///
/// The returned callback holds the `ResizeState` lock across `apply`, so native
/// resizes are serialized and the stored size cannot disagree with the pty.
fn resize_callback(
    apply: Box<dyn FnMut(u16, u16) -> Result<()> + Send>,
) -> Box<dyn Fn(u16, u16) -> Result<()> + Send + Sync> {
    let state = Mutex::new(ResizeState { apply, last_applied: None });
    Box::new(move |cols, rows| {
        if cols == 0 || rows == 0 {
            // When: `cols` or `rows` is zero, refuse before `apply` and before `last_applied` moves.
            return Err(anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing pty resize to {cols}x{rows}"),
            )));
        }
        let mut state = state.lock();
        if state.last_applied == Some((cols, rows)) {
            // When: the request equals `last_applied`, skip a native reflow that would change nothing.
            return Ok(());
        }
        (state.apply)(cols, rows)?;
        state.last_applied = Some((cols, rows));
        Ok(())
    })
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

struct ThreadWorkerSpawner;

impl sonicterm_types::lifecycle::NativeWorkerSpawner for ThreadWorkerSpawner {
    fn spawn(
        &self,
        _slot: usize,
        name: &'static str,
        work: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<thread::JoinHandle<()>> {
        thread::Builder::new().name(name.into()).spawn(work)
    }
}

#[derive(Default)]
struct NativeWorker {
    handle: Option<thread::JoinHandle<()>>,
    result: Option<Arc<AtomicBool>>,
    joined: bool,
    failed: bool,
}

impl NativeWorker {
    // Ordering: phase stores Release only after work returns; its observer acquires the successful native phase.
    fn start<T: Send + 'static>(
        &mut self,
        spawner: &dyn sonicterm_types::lifecycle::NativeWorkerSpawner,
        slot: usize,
        name: &'static str,
        native: Arc<Mutex<Option<T>>>,
        phase: Arc<std::sync::atomic::AtomicBool>,
        work: impl FnOnce(T) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        if self.handle.is_some() || self.joined {
            // When: a handle exists or joined is true, this phase already started and must never consume native custody twice.
            return Ok(());
        }
        self.result = Some(phase.clone());
        let handle = spawner.spawn(
            slot,
            name,
            Box::new(move || {
                let native = native.lock().take();
                if let Some(native) = native {
                    let succeeded = work(native);
                    phase.store(succeeded, Ordering::Release);
                }
            }),
        )?;
        self.handle = Some(handle);
        Ok(())
    }

    // Ordering: result Acquire observes the started worker's Release success publication after actual thread exit.
    fn wait_until(&mut self, deadline: Instant) -> bool {
        while self.handle.as_ref().is_some_and(|handle| !handle.is_finished()) {
            if Instant::now() >= deadline {
                // When: deadline expires, retain the handle so the worker and its native permits remain observable.
                return false;
            }
            thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        if let Some(handle) = self.handle.take() {
            self.failed |= handle.join().is_err();
            self.failed |=
                self.result.as_ref().is_none_or(|result| !result.load(Ordering::Acquire));
            self.joined = true;
        }
        self.joined && !self.failed
    }
}

#[derive(Default)]
struct PermitCustody {
    permits: Vec<sonicterm_types::lifecycle::NativePermit>,
    closed: bool,
}

type PermitSlot = Arc<Mutex<PermitCustody>>;

/// Couples native destruction to its late-installed accounting and optional completion phase.
struct NativeValue<T> {
    value: Option<T>,
    permits: PermitSlot,
    phase: Option<(PtyCompletion, usize)>,
}

impl<T> NativeValue<T> {
    fn new(value: T) -> (Self, PermitSlot) {
        let permits = Arc::new(Mutex::new(PermitCustody::default()));
        (Self { value: Some(value), permits: permits.clone(), phase: None }, permits)
    }
}

impl<T: Read> Read for NativeValue<T> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.value.as_mut().expect("owned reader").read(buffer)
    }
}

impl<T: Write> Write for NativeValue<T> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.value.as_mut().expect("owned writer").write(buffer)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.value.as_mut().expect("owned writer").flush()
    }
}

// Lifecycle: NativeValue drops its T value before permits; an unwind never publishes a successful phase.
impl<T> Drop for NativeValue<T> {
    fn drop(&mut self) {
        drop(self.value.take());
        let permits = {
            let mut custody = self.permits.lock();
            custody.closed = true;
            std::mem::take(&mut custody.permits)
        };
        drop(permits);
        if !thread::panicking() {
            // Successful native destruction precedes phase publication and excludes unwind cleanup.
            if let Some((completion, phase)) = self.phase.take() {
                completion.complete_phase(phase);
            }
        }
    }
}

#[derive(Default)]
struct TeardownWorkers {
    reader: Option<PtyIoThread>,
    writer: Option<PtyIoThread>,
    native: [NativeWorker; 4],
}

impl TeardownWorkers {
    fn any_failed(&self) -> bool {
        [&self.reader, &self.writer]
            .into_iter()
            .any(|worker| worker.as_ref().is_some_and(|worker| worker.failed))
            || self.native.iter().any(|worker| worker.failed)
    }

    fn all_finished(&self) -> bool {
        [&self.reader, &self.writer].into_iter().all(|worker| {
            worker.as_ref().is_none_or(|worker| {
                !worker.failed && worker.handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
            })
        }) && self.native.iter().all(|worker| {
            !worker.failed && worker.handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
        })
    }

    fn all_joined(&self) -> bool {
        [&self.reader, &self.writer].into_iter().all(|worker| {
            worker.as_ref().is_none_or(|worker| !worker.failed && worker.handle.is_none())
        }) && self.native.iter().all(|worker| !worker.failed && worker.handle.is_none())
    }

    fn join_finished(&mut self) {
        for (worker, name) in
            [(&mut self.reader, "PTY reader thread"), (&mut self.writer, "PTY writer thread")]
        {
            if let Some(worker) = worker {
                // is_finished proves the native worker returned, so finish cannot block on its JoinHandle.
                if worker.handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                    worker.finish(name);
                }
            }
        }
        for worker in &mut self.native {
            if worker.handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                worker.wait_until(Instant::now());
            }
        }
    }
}

struct CompletionState {
    phases: [AtomicBool; 8],
    remaining: AtomicUsize,
    complete: AtomicBool,
    wake: Mutex<Option<Sender<()>>>,
    workers: Arc<Mutex<TeardownWorkers>>,
}

/// Whole native phase completion and thread-exit observation without locking the teardown payload.
#[derive(Clone)]
pub struct PtyCompletion(Arc<CompletionState>);

impl PtyCompletion {
    fn new() -> Self {
        #[cfg(windows)]
        let required_phases = 8;
        #[cfg(not(windows))]
        let required_phases = 5;
        Self(Arc::new(CompletionState {
            phases: std::array::from_fn(|_| AtomicBool::new(false)),
            remaining: AtomicUsize::new(required_phases),
            complete: AtomicBool::new(false),
            wake: Mutex::new(None),
            workers: Arc::new(Mutex::new(TeardownWorkers::default())),
        }))
    }

    // Ordering: state phases and remaining use AcqRel once per phase; complete Release publishes all native destruction.
    fn complete_phase(&self, phase: usize) {
        let state = &self.0;
        if !state.phases[phase].swap(true, Ordering::AcqRel) {
            // When: phases swaps from false, this phase owns exactly one remaining decrement.
            if state.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                // When: remaining reaches zero, publish completion before the bounded driver wake hint.
                state.complete.store(true, Ordering::Release);
                if let Some(wake) = state.wake.lock().as_ref() {
                    // When: wake is installed, the driver rechecks phase completion and actual worker exit.
                    let _ = wake.try_send(());
                }
            }
        }
    }

    // Ordering: state phases Acquire observes successful native destruction before a phase is skipped.
    fn phase_complete(&self, phase: usize) -> bool {
        let state = &self.0;
        state.phases[phase].load(Ordering::Acquire)
    }

    /// Return whether every required phase successfully released its owned native values.
    // Ordering: state complete Acquire observes the last successful phase's Release publication.
    pub fn is_complete(&self) -> bool {
        let state = &self.0;
        state.complete.load(Ordering::Acquire)
    }

    /// Observe actual exit of every started worker; busy worker custody remains conservatively unfinished.
    pub fn workers_finished(&self) -> bool {
        self.0.workers.try_lock().is_some_and(|workers| workers.all_finished())
    }

    /// Join only already-finished handles, retaining all others for the next observation.
    pub fn join_finished_workers(&self) {
        if let Some(mut workers) = self.0.workers.try_lock() {
            workers.join_finished();
        }
    }

    /// Install a bounded wake hint for the final successful phase; the driver still checks actual thread exit.
    pub fn set_wake(&self, wake: Sender<()>) {
        *self.0.wake.lock() = Some(wake);
    }
}

/// Owned native PTY custody that can leave the caller before teardown starts.
pub struct PtyTeardown {
    reader_cancel: Sender<()>,
    writer_cancel: Sender<()>,
    child: Arc<Mutex<ChildState>>,
    writer_progress: Arc<PtyWriterProgress>,
    replies: PtyReplySender,
    master: Arc<Mutex<Option<NativeValue<Box<dyn portable_pty::MasterPty + Send>>>>>,
    #[cfg(windows)]
    conpty_drain_reader: Arc<Mutex<Option<NativeValue<Box<dyn Read + Send>>>>>,
    #[cfg(windows)]
    cancel_targets: [Arc<Mutex<Option<NativeValue<std::os::windows::io::OwnedHandle>>>>; 2],
    permit_slots: Vec<PermitSlot>,
    permits_installed: bool,
    resize: Option<Box<dyn Fn(u16, u16) -> Result<()> + Send + Sync>>,
    completion: PtyCompletion,
    spawner: Arc<dyn sonicterm_types::lifecycle::NativeWorkerSpawner>,
    fallback_duplicate:
        Option<Arc<dyn Fn() -> sonicterm_types::lifecycle::NativePermit + Send + Sync>>,
    direct_drop: bool,
    pass_failed: bool,
}

impl PtyTeardown {
    /// Retry unfinished platform phases in teardown order and report only whole-payload settlement.
    pub fn run_remaining(&mut self) -> sonicterm_types::ReapResult {
        self.pass_failed = false;
        run_pty_teardown(self);
        self.join_finished_workers();
        let workers = self.completion.0.workers.try_lock();
        let failed =
            self.pass_failed || workers.as_ref().is_some_and(|workers| workers.any_failed());
        let joined = workers.as_ref().is_some_and(|workers| workers.all_joined());
        if failed {
            // Explicit phase errors and unwinds remain retryable failure, not wait expiry.
            sonicterm_types::ReapResult::Failed
        } else if self.is_complete() && joined {
            // When: is_complete and joined both hold, settlement includes native release and every started worker join.
            sonicterm_types::ReapResult::Settled
        } else {
            // When: completion or joined is still false without a phase error, native work merely exceeded this pass's waits.
            sonicterm_types::ReapResult::TimedOut
        }
    }

    /// Return whether every native phase has successfully completed; thread exit is checked separately.
    pub fn is_complete(&self) -> bool {
        self.completion.is_complete()
    }

    /// Return whether all started IO and native workers actually exited without an observed unwind.
    pub fn workers_finished(&self) -> bool {
        self.completion.workers_finished()
    }

    /// Clone completion custody that the supervisor can observe without locking this payload.
    pub fn completion(&self) -> PtyCompletion {
        self.completion.clone()
    }

    /// Join only worker handles already known to have finished.
    pub fn join_finished_workers(&mut self) {
        self.completion.join_finished_workers();
    }

    /// Use a whole-unit helper grant for every native worker started by this payload.
    pub fn set_worker_spawner(
        &mut self,
        spawner: Arc<dyn sonicterm_types::lifecycle::NativeWorkerSpawner>,
    ) {
        self.spawner = spawner;
    }

    /// Retain a sink-owned counter alongside each slotless cancellation duplicate.
    pub fn set_fallback_duplicate_tracker(
        &mut self,
        tracker: Arc<dyn Fn() -> sonicterm_types::lifecycle::NativePermit + Send + Sync>,
    ) {
        self.fallback_duplicate = Some(tracker);
    }

    /// Transfer final cleanup responsibility to a caller that retains incomplete custody in its process sink.
    pub fn retain_for_supervisor(&mut self) {
        self.direct_drop = false;
    }

    /// Attempt bounded child termination only; force cancellation never reaps the session leader.
    pub fn terminate_only(&mut self) -> sonicterm_types::CancelOutcome {
        if self.is_complete() && self.workers_finished() {
            // When: is_complete and workers_finished both hold, force cancellation has no native work left to perform.
            self.join_finished_workers();
            return sonicterm_types::CancelOutcome::Settled;
        }
        self.signal_cancel();
        self.terminate_child();
        sonicterm_types::CancelOutcome::TimedOut
    }
}

// Lifecycle: PtyTeardown runs unfinished cleanup while it still owns the child and master resources.
impl Drop for PtyTeardown {
    fn drop(&mut self) {
        if self.direct_drop {
            // When: direct_drop owns fixture cleanup, attempt one bounded pass before preserving any incomplete native custody.
            if !self.is_complete() {
                // When: is_complete is false, only direct fixture custody runs its bounded final cleanup pass.
                let _ = self.run_remaining();
            }
            self.join_finished_workers();
            if !self.is_complete() || !self.workers_finished() {
                std::mem::forget(self.child.clone());
                std::mem::forget(self.master.clone());
                std::mem::forget(self.completion.clone());
                std::mem::forget(self.spawner.clone());
                std::mem::forget(self.resize.take());
                #[cfg(windows)]
                {
                    std::mem::forget(self.conpty_drain_reader.clone());
                    for target in &self.cancel_targets {
                        std::mem::forget(target.clone());
                    }
                }
            }
        }
    }
}

impl PtyTeardownOps for PtyTeardown {
    fn signal_cancel(&mut self) {
        // Publish intentional closure before rejecting retained reply senders.
        self.writer_progress.closing.store(true, Ordering::SeqCst);
        self.replies.close();
        let _ = self.reader_cancel.try_send(());
        let _ = self.writer_cancel.try_send(());
    }

    // Lock order: completion workers before cancel_targets, permit_slots, and native permits; no worker takes the workers lock.
    fn cancel_io(&mut self) {
        #[cfg(windows)]
        {
            let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
            let mut workers = self.completion.0.workers.lock();
            for (index, name, phase) in [
                (0, "PTY reader thread", READER_CANCEL_PHASE),
                (1, "PTY writer thread", WRITER_CANCEL_PHASE),
            ] {
                if self.completion.phase_complete(phase)
                    || workers.native[index + 2].handle.is_some()
                    || workers.native[index + 2].joined
                {
                    // When: this phase already started, retries must not duplicate cancellation against the same IO thread.
                    continue;
                }
                let io = if index == 0 { &workers.reader } else { &workers.writer };
                if io
                    .as_ref()
                    .is_none_or(|io| io.handle.as_ref().is_none_or(thread::JoinHandle::is_finished))
                {
                    // When: the IO thread finished, there is no cancellation target; return its unused native permit once.
                    drop(self.cancel_targets[index].lock().take());
                    let permits = {
                        let mut custody = self.permit_slots[6 + index].lock();
                        custody.closed = true;
                        std::mem::take(&mut custody.permits)
                    };
                    drop(permits);
                    self.completion.complete_phase(phase);
                    continue;
                }
                if self.cancel_targets[index].lock().is_none() {
                    // When: cancel_targets is empty, create one owned duplicate; a failed spawn retains it for retry.
                    let Some(duplicate) = io.as_ref().and_then(PtyIoThread::cancel_target) else {
                        // When: cancel_target cannot duplicate a live thread, retain a retryable cancellation failure.
                        self.pass_failed = true;
                        continue;
                    };
                    let (mut native, _) = NativeValue::new(duplicate);
                    native.permits = self.permit_slots[6 + index].clone();
                    native.phase = Some((self.completion.clone(), phase));
                    if let Some(tracker) = &self.fallback_duplicate {
                        native.permits.lock().permits.push(tracker());
                    }
                    *self.cancel_targets[index].lock() = Some(native);
                }
                if !self.permits_installed {
                    // When: permits_installed is false, the fallback entry below starts both token-bearing targets together.
                    continue;
                }
                let result = workers.native[index + 2].start(
                    &*self.spawner,
                    3 + index,
                    "sonic-pty-cancel",
                    self.cancel_targets[index].clone(),
                    Arc::new(AtomicBool::new(false)),
                    move |native| cancel_owned_target(name, native),
                );
                if let Err(error) = result {
                    self.pass_failed = true;
                    tracing::warn!(%error, thread = name, "could not start PTY cancellation worker");
                }
            }
            if !self.permits_installed {
                // Slotless cancellation keeps tokens and join handles without borrowing an admitted grant.
                let targets = [
                    ("PTY reader thread", self.cancel_targets[0].clone()),
                    ("PTY writer thread", self.cancel_targets[1].clone()),
                ];
                if let Err(error) = cancel_io_within(
                    &targets,
                    &mut workers.native[2..],
                    &*self.spawner,
                    deadline,
                    cancel_owned_target,
                ) {
                    self.pass_failed = true;
                    tracing::warn!(%error, "could not start slotless PTY cancellation worker");
                }
            } else {
                // When: permits_installed is true, both reserved cancels share one deadline and retain unfinished handles.
                for worker in &mut workers.native[2..] {
                    worker.wait_until(deadline);
                }
            }
        }
    }

    fn terminate_child(&mut self) {
        if self.completion.phase_complete(TERMINATE_PHASE) {
            // When: TERMINATE_PHASE is complete, repeat passes must not signal an already settled child identity.
            return;
        }
        let Some(mut child) = self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
            // When: child custody stays busy through the bound, leave termination incomplete for the next pass.
            return;
        };
        child.closing = true;
        match terminate_child_without_reap(&mut child) {
            Ok(()) => self.completion.complete_phase(TERMINATE_PHASE),
            Err(error) => {
                self.pass_failed = true;
                tracing::warn!(%error, "failed to terminate PTY child");
            }
        }
    }

    fn finish_io(&mut self) {
        let mut workers = self.completion.0.workers.lock();
        if let Some(reader) = &mut workers.reader {
            reader.finish("PTY reader thread");
        }
        if let Some(writer) = &mut workers.writer {
            writer.finish("PTY writer thread");
        }
    }

    fn close_master(&mut self) {
        if self.completion.phase_complete(MASTER_PHASE) {
            // When: MASTER_PHASE is complete, the native master and its permits have already closed.
            return;
        }
        #[cfg(windows)]
        {
            let mut workers = self.completion.0.workers.lock();
            let drain = workers.native[0].start(
                &*self.spawner,
                1,
                "sonic-conpty-drain",
                self.conpty_drain_reader.clone(),
                Arc::new(AtomicBool::new(false)),
                |mut reader| {
                    let mut buffer = [0u8; 8192];
                    loop {
                        // When: reader reports EOF or a closed pipe, drain succeeded; an unexpected native read failure stays incomplete.
                        match reader.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(error)
                                if error
                                    .raw_os_error()
                                    .is_some_and(|code| code == 109 || code == 232) =>
                            {
                                break
                            }
                            Err(error) => {
                                reader.phase = None;
                                tracing::warn!(%error, "ConPTY output drain failed");
                                return false;
                            }
                        }
                    }
                    drop(reader);
                    true
                },
            );
            if let Err(error) = drain {
                // When: drain spawn is refused, leave the master and its recovery slot untouched for a later retry.
                self.pass_failed = true;
                tracing::warn!(%error, "could not start ConPTY drain worker");
                return;
            }
            if workers.native[0].handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                workers.native[0].wait_until(Instant::now());
            }
            if workers.native[0].failed {
                // When: the drain already failed, keep the undrained master in recovery custody rather than starting its closer.
                self.pass_failed = true;
                return;
            }
            // The callback contains only an Arc to master custody; dropping it cannot close the retained native master.
            drop(self.resize.take());
            let close = workers.native[1].start(
                &*self.spawner,
                2,
                "sonic-conpty-close",
                self.master.clone(),
                Arc::new(AtomicBool::new(false)),
                |master| {
                    drop(master);
                    true
                },
            );
            if let Err(error) = close {
                // When: close spawn fails, retain the master recovery slot and reuse the already-started drain on retry.
                self.pass_failed = true;
                tracing::warn!(%error, "could not start ConPTY close worker");
                return;
            }
            workers.native[1].wait_until(Instant::now() + CONPTY_CLOSE_TIMEOUT);
            workers.native[0].wait_until(Instant::now() + CONPTY_CLOSE_TIMEOUT);
        }
        #[cfg(not(windows))]
        {
            let Some(mut master) = self.master.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
                // When: master custody is busy, retain its descriptor for a later bounded close attempt.
                return;
            };
            let native = master.take();
            drop(master);
            drop(self.resize.take());
            drop(native);
        }
    }

    fn reap_child(&mut self) {
        if self.completion.phase_complete(REAP_PHASE) {
            // When: REAP_PHASE is complete, the child handle has already closed and its exit status stays cached.
            return;
        }
        if !self.completion.phase_complete(TERMINATE_PHASE) {
            // When: TERMINATE_PHASE remains incomplete, keep the leader unreaped so its session identity cannot be reused.
            return;
        }
        let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
        loop {
            let Some(mut child) = self.child.try_lock_until(deadline) else {
                // When: child custody cannot be acquired before deadline, retain the leader handle for retry.
                return;
            };
            // When: has_exited succeeds, close the retained process handle; errors fail this pass and live children keep polling.
            match child.has_exited() {
                Ok(true) => {
                    drop(child.child.take());
                    self.completion.complete_phase(REAP_PHASE);
                    return;
                }
                Err(error) => {
                    self.pass_failed = true;
                    tracing::warn!(%error, "failed to reap PTY child");
                    return;
                }
                Ok(false) => {}
            }
            drop(child);
            if Instant::now() >= deadline {
                // When: deadline expires while the leader is live, preserve its handle rather than waiting without a bound.
                return;
            }
            thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

fn apply_child_cwd(builder: &mut CommandBuilder, explicit: Option<&Path>, home: Option<&str>) {
    if let Some(cwd) = explicit.filter(|cwd| cwd.is_dir()) {
        builder.cwd(cwd);
    } else if let Some(home) = home {
        // When: `home` exists but no valid explicit directory does, use it as the child fallback.
        builder.cwd(home);
    }
}

/// Duplicate the Unix PTY master descriptor into an owned writable `File`,
/// whose close is silent. A master exposing no descriptor is an error, never a
/// fallback to a writer that injects bytes.
#[cfg(unix)]
fn unix_master_writer_file(master: &dyn portable_pty::MasterPty) -> Result<std::fs::File> {
    let raw = master.as_raw_fd().ok_or_else(|| {
        anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unix pty master exposed no descriptor to duplicate for input",
        ))
    })?;
    let borrowed = {
        // SAFETY: `raw` is owned and kept open by `master`, borrowed for this
        // whole call, and only duplicated here — never closed.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) }
    };
    // Duplicates via `F_DUPFD_CLOEXEC`: the new descriptor carries its own
    // FD_CLOEXEC, while file-status flags stay shared with the master.
    Ok(std::fs::File::from(borrowed.try_clone_to_owned()?))
}

/// Build the master-side input writer for a freshly opened PTY. A writer's
/// destructor is part of the child's input stream, so one seam decides it per
/// platform: `portable-pty`'s Unix writer writes a newline and `VEOF` when
/// dropped, delivering input no source produced.
#[cfg(unix)]
fn pty_writer(master: &dyn portable_pty::MasterPty) -> Result<Box<dyn Write + Send>> {
    Ok(Box::new(unix_master_writer_file(master)?))
}

/// Build the master-side input writer for a freshly opened PTY. Non-Unix keeps
/// `portable-pty`'s writer, whose ConPTY destructor writes nothing.
#[cfg(not(unix))]
fn pty_writer(master: &dyn portable_pty::MasterPty) -> Result<Box<dyn Write + Send>> {
    master.take_writer()
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
        apply_clean_e2e_environment(&mut builder, opts.clean_e2e);
        let home = std::env::var("HOME").ok();
        apply_child_cwd(&mut builder, opts.cwd.as_deref(), home.as_deref());
        apply_child_pty_env(&mut builder, &opts.term_program);

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let master = pair.master;
        #[cfg(unix)]
        let unix_session_id = child.process_id();
        #[cfg(not(unix))]
        let unix_session_id = None;
        let completion = PtyCompletion::new();
        let (mut reader, reader_permit) = NativeValue::new(master.try_clone_reader()?);
        reader.phase = Some((completion.clone(), READER_PHASE));
        #[cfg(windows)]
        let (mut drain_reader, drain_permit) = NativeValue::new(master.try_clone_reader()?);
        #[cfg(windows)]
        {
            drain_reader.phase = Some((completion.clone(), DRAIN_PHASE));
        }
        let (mut writer, writer_permit) = NativeValue::new(pty_writer(&*master)?);
        writer.phase = Some((completion.clone(), WRITER_PHASE));
        let (mut master, master_permit) = NativeValue::new(master);
        master.phase = Some((completion.clone(), MASTER_PHASE));
        let master = Arc::new(Mutex::new(Some(master)));
        let child = Arc::new(Mutex::new(ChildState::new(child, unix_session_id)));
        #[cfg(windows)]
        let permit_slots = vec![
            child.lock().child.as_ref().expect("owned child").permits.clone(),
            master_permit.clone(),
            master_permit,
            reader_permit,
            drain_permit,
            writer_permit,
            Arc::new(Mutex::new(PermitCustody::default())),
            Arc::new(Mutex::new(PermitCustody::default())),
        ];
        #[cfg(not(windows))]
        let permit_slots = vec![master_permit, reader_permit, writer_permit];

        let (out_tx, out_rx) = pty_output_channel();
        let (in_tx, in_rx) = pty_input_channel();
        let (reader_cancel, reader_cancel_rx) = crossbeam_channel::bounded(1);
        let (writer_cancel, writer_cancel_rx) = crossbeam_channel::bounded(1);

        // Reader thread: pty -> out_rx.
        let output_meter = Arc::new(QueuedOutputMeter::default());
        let reader_thread =
            spawn_reader_thread(Box::new(reader), out_tx, reader_cancel_rx, output_meter.clone());
        // Writer thread: in_rx -> pty.
        let queued_input_bytes = Arc::new(AtomicUsize::new(0));
        let writer_progress = Arc::new(PtyWriterProgress::new());
        let (replies, reply_reader) = reply_spool();
        let writer_thread = spawn_writer_thread(
            Box::new(writer),
            in_rx,
            writer_cancel_rx,
            queued_input_bytes.clone(),
            writer_progress.clone(),
            reply_reader,
        );

        let resize_master = master.clone();
        // Callers re-apply geometry on every tab activation, and each native
        // call is a ConPTY reflow or SIGWINCH, so unchanged sizes are skipped.
        let resize = resize_callback(Box::new(move |cols: u16, rows: u16| {
            let master = resize_master
                .try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT)
                .ok_or_else(|| anyhow::anyhow!("PTY master custody is busy"))?;
            let native = master
                .as_ref()
                .and_then(|master| master.value.as_ref())
                .ok_or_else(|| anyhow::anyhow!("PTY master is closing"))?;
            native.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
            Ok(())
        }));
        {
            let mut workers = completion.0.workers.lock();
            workers.reader = Some(reader_thread);
            workers.writer = Some(writer_thread);
        }

        Ok(Self {
            out_rx,
            in_tx,
            queued_input_bytes,
            writer_progress: writer_progress.clone(),
            replies: replies.clone(),
            resize,
            teardown: Some(PtyTeardown {
                reader_cancel,
                writer_cancel,
                child,
                writer_progress,
                replies,
                master,
                #[cfg(windows)]
                conpty_drain_reader: Arc::new(Mutex::new(Some(drain_reader))),
                #[cfg(windows)]
                cancel_targets: std::array::from_fn(|_| Arc::new(Mutex::new(None))),
                permit_slots,
                permits_installed: false,
                resize: None,
                completion,
                spawner: Arc::new(ThreadWorkerSpawner),
                fallback_duplicate: None,
                direct_drop: true,
                pass_failed: false,
            }),
            shell_program_path: cmd.to_string(),
            output_meter,
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
    meter: Arc<QueuedOutputMeter>,
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
            const RING_CAP: usize = PTY_READ_RING_BYTES;
            // Keep at least one full PTY chunk (typical kernel pipe buffer
            // is 4–16 KiB) of headroom before each read to avoid forcing a
            // realloc mid-read.
            const READ_HEADROOM: usize = 8 * 1024;
            let mut buf = BytesMut::with_capacity(RING_CAP);
            // The whole allocation, not the unsplit remainder. `split` hands
            // out views into one buffer and shrinks `buf`'s window; the memory
            // behind an earlier view is not reclaimable while any view lives,
            // so a charge computed from the shrunken window would undercount
            // the ring by everything already handed out. Sampled straight
            // after each `reserve`, when the window is the allocation.
            let mut ring_bytes = buf.capacity();
            // Weak, so the charge for the ring we are filling lives exactly as
            // long as some chunk still views into it. Holding an `Arc` here
            // would keep an emptied queue charged for a ring nobody is waiting
            // on; upgrading tells us whether this ring is already charged.
            let mut ring_charge: std::sync::Weak<RingCharge> = std::sync::Weak::new();
            loop {
                if buf.capacity() - buf.len() < READ_HEADROOM {
                    // If downstream has dropped its `Bytes` views, this
                    // reclaims the original buffer; otherwise it allocates
                    // a fresh one and drops our half of the previous ring.
                    buf.reserve(RING_CAP);
                    // Either way the bytes we hand out from here belong to a
                    // different ring than the charge above describes.
                    ring_bytes = buf.capacity();
                    ring_charge = std::sync::Weak::new();
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
                // When: `reader` data publishes its initialized prefix; EOF or error stops the pump.
                match reader.read(&mut buf[initial_len..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.truncate(initial_len + n);
                        let chunk = buf.split().freeze();
                        // Reuse this ring's charge if any queued view still
                        // holds it, so one allocation is counted once however
                        // many views come out of it.
                        let charge = match ring_charge.upgrade() {
                            Some(existing) => existing,
                            None => {
                                let fresh = RingCharge::new(ring_bytes, &meter);
                                ring_charge = Arc::downgrade(&fresh);
                                fresh
                            }
                        };
                        let chunk = PtyOutputChunk::new(chunk, charge, &meter);
                        if !send_pty_output(&tx, &cancel, chunk) {
                            // When: `send_pty_output` reports cancellation or disconnection, stop the reader pump.
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
    PtyIoThread { handle: Some(handle), done, failed: false }
}

// Ordering: queued_bytes, in_flight_bytes, completed_messages, operation use Relaxed; the channel transfers ownership.
fn spawn_writer_thread(
    mut writer: Box<dyn Write + Send>,
    rx: Receiver<Outgoing>,
    cancel: Receiver<()>,
    queued_bytes: Arc<AtomicUsize>,
    progress: Arc<PtyWriterProgress>,
    replies: ReplyReader,
) -> PtyIoThread {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let handle = thread::Builder::new()
        .name("sonic-pty-writer".into())
        .spawn(move || {
            let _active = ActivePtyIoThread::enter();
            let mut last_was_reply = false;
            let mut reply_wake = replies.wake.clone();
            loop {
                let cancelled =
                    !matches!(cancel.try_recv(), Err(crossbeam_channel::TryRecvError::Empty));
                if cancelled {
                    // When: cancelled is true, stop before beginning another native write.
                    break;
                }
                // A ready UI message gets the next turn after each reply chunk.
                // Otherwise select fairly between UI input and the reply wake hint.
                let ui_turn = last_was_reply.then(|| rx.try_recv().ok()).flatten();
                let (bytes, is_reply) = if let Some(bytes) = ui_turn {
                    (bytes, false)
                } else {
                    // When: ui_turn is absent, park until cancellation or either input source becomes ready.
                    crossbeam_channel::select! {
                        recv(cancel) -> _ => break,
                        recv(rx) -> result => match result {
                            Ok(bytes) => (bytes, false),
                            Err(_) => break,
                        },
                        recv(reply_wake) -> _ => match replies.pop() {
                            Ok(Some(bytes)) => (bytes, true),
                            Ok(None) => continue,
                            Err(error) => {
                                if !progress.closing.load(Ordering::SeqCst) {
                                    tracing::warn!(%error, "PTY reply spool read failed");
                                }
                                reply_wake = crossbeam_channel::never();
                                continue;
                            },
                        },
                    }
                };
                last_was_reply = is_reply;
                if !is_reply {
                    // Only UI admissions charged queued_bytes; replies own separate storage.
                    queued_bytes.fetch_sub(bytes.len(), Ordering::Relaxed);
                }
                progress.in_flight_bytes.store(bytes.len(), Ordering::Relaxed);
                progress.begin(PtyWriterPhase::Writing);
                if let Err(e) = writer.write_all(&bytes) {
                    // When: `write_all` fails, report it unless cancellation intentionally interrupted I/O.
                    if cancel.try_recv().is_err() {
                        tracing::warn!("pty write error: {e}");
                    }
                    replies.fail(e);
                    break;
                }
                progress.begin(PtyWriterPhase::Flushing);
                if let Err(error) = writer.flush() {
                    // When: flush fails, preserve the actual native failure rather than reporting a completed write.
                    if cancel.try_recv().is_err() {
                        tracing::warn!(%error, "pty flush error");
                    }
                    replies.fail(error);
                    break;
                }
                progress.completed_messages.fetch_add(1, Ordering::Relaxed);
                progress.in_flight_bytes.store(0, Ordering::Relaxed);
                progress.operation.store(0, Ordering::Relaxed);
            }
            // Reader lifetime, not surviving producer clones, owns pending spill storage.
            drop(replies);
            progress.in_flight_bytes.store(0, Ordering::Relaxed);
            progress.operation.store(3, Ordering::Relaxed);
            // Nothing will drain the queue again. Anything still in it is
            // released when the channel drops, so the figure must not keep
            // reporting it as held.
            queued_bytes.store(0, Ordering::Relaxed);
            let _ = done_tx.send(());
        })
        // PANIC: see `spawn_reader_thread` rationale above — OS-level
        // thread-spawn failure at PTY init is unrecoverable.
        .expect("spawn pty writer");
    PtyIoThread { handle: Some(handle), done, failed: false }
}

fn apply_clean_e2e_environment(builder: &mut CommandBuilder, clean_e2e: bool) {
    if clean_e2e {
        // Suppress shell hooks without replacing HOME or unrelated environment.
        builder.env_remove("ENV");
        builder.env_remove("BASH_ENV");
    }
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
        // When: `clean_e2e` is false, preserve the selected shell's normal interactive startup.
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

/// Reports whether macOS needs a UTF-8 locale fallback without overriding `LC_ALL`.
#[doc(hidden)]
pub fn should_apply_utf8_locale_fallback(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> bool {
    if !is_empty_env(lc_all) {
        // When: `lc_all` is explicit, it overrides lower-priority locale variables and must be preserved.
        return false;
    }
    !is_utf8_locale(lc_ctype) && !is_utf8_locale(lang)
}

/// Returns the `LANG` value used when no locale variables are set.
#[doc(hidden)]
pub const fn default_lang_utf8_locale() -> &'static str {
    DEFAULT_LANG_UTF8_LOCALE
}

/// Returns the UTF-8 `LC_CTYPE` value used by the macOS fallback.
#[doc(hidden)]
pub const fn default_lc_ctype_utf8_locale() -> &'static str {
    DEFAULT_LC_CTYPE_UTF8_LOCALE
}

fn is_empty_env(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or_default().is_empty()
}

fn is_utf8_locale(value: Option<&str>) -> bool {
    let Some(value) = value else {
        // When: `value` absent means no locale spelling can advertise UTF-8.
        return false;
    };
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
            format!("[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $OutputEncoding=[System.Text.UTF8Encoding]::new($false); chcp 65001 > $null; & {{\n{}\n}}", include_str!("../../../scripts/powershell-integration.ps1")),
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
    let status =
        // SAFETY: this size query passes no data buffer and gives `bytes` as writable output storage.
        unsafe {
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
    if status != ERROR_SUCCESS || !(2..=MAX_PATH_BYTES).contains(&bytes) {
        // When: `status` failed or `bytes` is implausible, do not allocate from untrusted registry size data.
        return None;
    }

    let mut value = vec![0u16; (bytes as usize).div_ceil(2)];
    let status =
        // SAFETY: `value` is writable for the byte count returned by the prior successful size query.
        unsafe {
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
        // When: the registry value read `status` failed, so `value` does not contain a usable path.
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
            // When: `lname` is outside the PowerShell package family, it cannot contain the target executable.
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

#[cfg(unix)]
fn resolve_unix_default_shell_with(
    environment_shell: Option<&str>,
    passwd_shell: Option<&str>,
    executable: impl Fn(&Path) -> bool,
) -> String {
    environment_shell
        .into_iter()
        .chain(passwd_shell)
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .find(|candidate| executable(Path::new(candidate)))
        .unwrap_or("/bin/sh")
        .to_string()
}

#[cfg(unix)]
fn unix_shell_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;

    let mut capacity = 16 * 1024;
    loop {
        let mut record: libc::passwd =
            // SAFETY: zeroed `passwd` is writable output storage for `getpwuid_r`.
            unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; capacity];
        let status =
            // SAFETY: `record`, `buffer`, and `result` remain writable for this call; `getuid` supplies the current real user id.
            unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    &mut record,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                )
            };
        if status == libc::ERANGE && capacity < 1024 * 1024 {
            // When: `status` is `ERANGE`, retry with bounded larger storage instead of reading a partial record.
            capacity *= 2;
            continue;
        }
        if status != 0 || result.is_null() || record.pw_shell.is_null() {
            // When: lookup `status`, `result`, or `pw_shell` is invalid, no trustworthy passwd shell is available.
            return None;
        }
        return
            // SAFETY: successful `getpwuid_r` stores a NUL-terminated `pw_shell` pointer into the live `buffer`.
            unsafe { CStr::from_ptr(record.pw_shell) }
                .to_str()
                .ok()
                .map(str::to_string);
    }
}

#[cfg(unix)]
fn default_shell_program() -> String {
    let environment_shell = std::env::var("SHELL").ok();
    let passwd_shell = passwd_shell();
    resolve_unix_default_shell_with(
        environment_shell.as_deref(),
        passwd_shell.as_deref(),
        unix_shell_is_executable,
    )
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn default_shell_program() -> String {
    "/bin/sh".to_string()
}

#[cfg(target_os = "windows")]
fn path_lookup(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        // When: `candidate` is an explicit existing path, return it without consulting PATH.
        return Some(candidate.to_string_lossy().to_string());
    }
    let path = std::env::var_os("PATH")?;
    let allow_windowsapps =
        std::env::var("SONICTERM_ALLOW_WINDOWSAPPS_SHELL").map(|v| v == "1").unwrap_or(false);
    std::env::split_paths(&path)
        .map(|dir: PathBuf| dir.join(name))
        .find(|candidate| {
            if !candidate.is_file() {
                // When: `candidate` is not a file, PATH lookup cannot spawn it.
                return false;
            }
            // skip Microsoft Store WindowsApps stubs for `pwsh.exe` /
            // `powershell.exe`. The App Execution Alias produces zero output
            // under ConPTY when spawned bare, so the e2e gates silently fail.
            // Escape hatch: SONICTERM_ALLOW_WINDOWSAPPS_SHELL=1 to opt back in.
            let lname = name.to_ascii_lowercase();
            let is_powershell = lname.ends_with("pwsh.exe") || lname.ends_with("powershell.exe");
            if is_powershell && !allow_windowsapps {
                // When: `is_powershell` and aliases are disallowed, reject only a per-user WindowsApps stub.
                let lpath = candidate.to_string_lossy().to_lowercase();
                // Skip only per-user App Execution Alias stubs. The real
                // Microsoft Store PowerShell package also lives under a
                // WindowsApps directory (usually `C:\Program Files\WindowsApps\
                // Microsoft.PowerShell_*\pwsh.exe`) and works correctly under
                // ConPTY; skipping every `\WindowsApps\` path made SonicTerm
                // fall back to Windows PowerShell 5.1, whose PSReadLine redraw
                // path emits literal `?` bytes for CJK edits.
                if is_windowsapps_alias_stub_path(&lpath) {
                    // When: `lpath` matched the per-user alias stub path, so skip it.
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
        "cmd.exe" | "cmd" => vec!["/D".to_string()],
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
/// The `TERM_PROGRAM_VERSION` to advertise for a given `TERM_PROGRAM`.
///
/// Running as ourselves, this is SonicTerm's own version. But `term_program`
/// is configurable precisely so a user can present as a terminal that tools
/// already recognise, and a name/version pair has to be internally consistent
/// to be useful: programs gate features on the version *of the terminal the
/// name claims*.
///
/// WezTerm versions its releases by datestamp, and consumers compare those
/// lexically. A semver string sorts below any datestamp, so sending
/// SonicTerm's `1.2.0` under WezTerm's name fails every such gate — the tool
/// takes its WezTerm branch on the name, then disables the features it just
/// decided the terminal was too old for. That is worse than either identity
/// alone.
///
/// The advertised datestamp is the release that introduced the capabilities
/// SonicTerm actually implements, not a moving "now": claiming to be newer
/// than we are would invite gates for features we do not have.
fn term_program_version(term_program: &str) -> &'static str {
    match term_program {
        "WezTerm" => WEZTERM_ADVERTISED_VERSION,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Datestamp advertised when presenting as WezTerm.
///
/// Above the thresholds consumers test for modern capabilities — notably
/// styled underlines, which is the gate that otherwise makes editors probe
/// with a DCS query instead of enabling the feature outright.
const WEZTERM_ADVERTISED_VERSION: &str = "20230712-072601";

/// Applies terminal capability, identity, and locale variables to a child command.
pub fn apply_child_pty_env(builder: &mut CommandBuilder, term_program: &str) {
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    // Identify the terminal to programs that branch on TERM_PROGRAM
    // (e.g. Copilot CLI, shells, prompt frameworks). Mirrors iTerm2 /
    // WezTerm, which set TERM_PROGRAM + TERM_PROGRAM_VERSION.
    builder.env("TERM_PROGRAM", term_program);
    builder.env("TERM_PROGRAM_VERSION", term_program_version(term_program));
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
