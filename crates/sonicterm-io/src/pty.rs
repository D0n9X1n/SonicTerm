//! Cross-platform PTY spawning.
//!
//! Wraps the `portable-pty` crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
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
pub use sonicterm_types::PaneExitStatus;

pub use crate::command_resolve::CommandSpawnError;

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

/// User-input ordinals and their comparisons are taken modulo this (2^20).
const INPUT_ORDINAL_MASK: u32 = (1 << 20) - 1;
/// Admission slots, one per ordinal modulo this. At most five writes are outstanding (four queued,
/// one taken), so five would suffice, but five does not divide 2^20: ordinal 2^20 - 1 and its wrapped
/// successor 0 would share a slot. Eight is the next power of two, so a wrapped ordinal keeps its own.
const INPUT_SLOTS: usize = 8;
/// `writer_on`: the writer is between messages.
const WRITER_ON_IDLE: u8 = 0;
/// `writer_on`: the writer is writing user input.
const WRITER_ON_INPUT: u8 = 1;
/// `writer_on`: the writer is writing a parser reply.
const WRITER_ON_REPLY: u8 = 2;
/// `terminal`: the writer is running.
const TERMINAL_RUNNING: u8 = 0;
/// `terminal`: a native `write_all` failed.
const TERMINAL_FAILED_WRITE: u8 = 1;
/// `terminal`: a native `flush` failed.
const TERMINAL_FAILED_FLUSH: u8 = 2;
/// `terminal`: cancelled, closed or disconnected on purpose.
const TERMINAL_STOPPED: u8 = 3;

/// The oldest user-input write that has not completed, or why none can be judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyInputProgress {
    /// Every admitted user-input write completed.
    Idle,
    /// The oldest outstanding write: its ordinal (mod 2^20), when it was admitted, and whether the
    /// writer is busy with a parser reply ahead of it.
    Oldest {
        /// Ordinal of the oldest outstanding user-input write, modulo 2^20.
        ordinal: u32,
        /// When that write was admitted.
        admitted_at: Instant,
        /// The writer is writing a parser reply rather than user input.
        on_reply: bool,
    },
    /// A completion raced the sample; nothing can be judged from it.
    Raced,
    /// The writer has stopped; `lost` admitted writes never completed.
    Terminal {
        /// The writer stopped on a native error, not on purpose.
        failed: bool,
        /// Admitted user-input writes that never completed.
        lost: u32,
    },
}

#[derive(Debug)]
struct PtyWriterProgress {
    closing: std::sync::atomic::AtomicBool,
    epoch: Instant,
    operation: AtomicU64,
    in_flight_bytes: AtomicUsize,
    completed_messages: AtomicU64,
    /// Held only around one admission and once at writer exit, so ordinal order is channel order
    /// and no admission succeeds after the writer recorded its end.
    admission: Mutex<()>,
    /// User-input writes admitted, wrapping; compared modulo 2^20.
    input_admitted: AtomicU32,
    /// User-input writes completed (flushed), wrapping; parser replies never count.
    input_completed: AtomicU32,
    /// Per ordinal modulo [`INPUT_SLOTS`]: `(ordinal << 44) | admitted_ms`.
    admitted_at: [AtomicU64; INPUT_SLOTS],
    /// What the writer is writing, for the tooltip.
    writer_on: AtomicU8,
    /// Whether and how the writer stopped.
    terminal: AtomicU8,
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
    /// When the native read that produced these bytes returned; `None` unless the PTY was spawned
    /// with diagnostic timestamps.
    read_at: Option<Instant>,
}

// The diagnostic stamp adds one `Option<Instant>` to every chunk, so each of the 64 reserved
// channel slots grows by 16 bytes on the supported 64-bit targets, from 48 to 64.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<Option<Instant>>() == 16);
    assert!(std::mem::size_of::<PtyOutputChunk>() == 64);
};

/// Cloneable, non-blocking probe for a PTY child process's exit state.
#[derive(Clone)]
pub struct PtyChildExitProbe {
    child: Arc<Mutex<ChildState>>,
    progress: Arc<PtyWriterProgress>,
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
    status: Option<PaneExitStatus>,
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
    exit_status: Option<PaneExitStatus>,
    unix_session_id: Option<u32>,
    process_group_signalled: bool,
    /// Set once exit is observed or intended, before anything can release the child's identity.
    ///
    /// Shared with [`PtyTeardown`] and readable through [`PtyExitObserved`], so a foreground
    /// probe result for a pid that may already belong to another process is refused.
    exit_observed: Arc<AtomicBool>,
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
    /// Pid and start token captured at spawn, while the child was unreaped.
    process_identity: Option<crate::proc_info::ProcessIdentity>,
}

/// Whether a PTY child's exit has been observed or intended; see [`PtyHandle::exit_observed`].
///
/// Once true it stays true. A reader that sees `false` knows the child's pid has not been
/// released, so a foreground sample taken for that pid still describes this child.
#[derive(Clone, Debug)]
pub struct PtyExitObserved {
    exit_observed: Arc<AtomicBool>,
}

impl PtyExitObserved {
    /// Whether exit has been published.
    // Ordering: exit_observed loads Acquire, pairing with the Release store in `publish_exit`.
    pub fn is_exited(&self) -> bool {
        self.exit_observed.load(Ordering::Acquire)
    }

    /// Test-only: a flag attached to no child, for app tests that inject a foreground probe.
    #[doc(hidden)]
    pub fn detached() -> Self {
        Self { exit_observed: Arc::new(AtomicBool::new(false)) }
    }

    /// Test-only: publish exit on a detached flag.
    #[doc(hidden)]
    pub fn publish_for_test(&self) {
        publish_exit(&self.exit_observed);
    }
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
    /// Stamp each output chunk with the instant its native read returned
    /// ([`PtyOutputChunk::read_at`]). Diagnostic only: it changes nothing about the shell, its
    /// arguments, environment, working directory, geometry or teardown. Fixed for the reader's
    /// life; defaults to `false`.
    pub diagnostic_timestamps: bool,
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
            diagnostic_timestamps: false,
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
        let teardown = self.teardown.as_ref().expect("live PTY custody");
        // Publish before the custody lock, so a busy lock cannot leave a stopping child unpublished.
        publish_exit(&teardown.exit_observed);
        let mut child = teardown.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT).ok_or_else(|| {
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
        // Retirement stops the child: publish before closing and before custody moves to the reaper.
        publish_exit(&self.teardown.as_ref().expect("live PTY custody").exit_observed);
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

    /// The shell's pid and start token, captured once at spawn from the retained child.
    ///
    /// `None` when the platform reports no start token (every platform but macOS and
    /// Windows) or the query failed; callers then sample no foreground process.
    pub fn process_identity(&self) -> Option<crate::proc_info::ProcessIdentity> {
        self.process_identity
    }

    /// A cloneable view of whether this child's exit has been observed or intended.
    pub fn exit_observed(&self) -> PtyExitObserved {
        PtyExitObserved {
            exit_observed: self.teardown.as_ref().expect("live PTY custody").exit_observed.clone(),
        }
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
        try_queue_pty_input(&self.in_tx, &self.queued_input_bytes, &self.writer_progress, bytes)
    }

    /// The oldest outstanding user-input write, or how the writer stopped; reads atomics only.
    #[must_use]
    pub fn input_progress(&self) -> PtyInputProgress {
        self.writer_progress.input_progress()
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
            // A dropped handle stops its child: publish before inline teardown closes the handle.
            publish_exit(&teardown.exit_observed);
            // Direct fixture callers retain bounded inline cleanup while native custody remains attached.
            teardown.resize = Some(std::mem::replace(&mut self.resize, Box::new(|_, _| Ok(()))));
            drop(teardown);
        }
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

struct ThreadWorkerSpawner;

#[derive(Default)]
struct NativeWorker {
    handle: Option<thread::JoinHandle<()>>,
    result: Option<Arc<AtomicBool>>,
    joined: bool,
    failed: bool,
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

#[derive(Default)]
struct TeardownWorkers {
    reader: Option<PtyIoThread>,
    writer: Option<PtyIoThread>,
    native: [NativeWorker; 4],
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

/// Owned native PTY custody that can leave the caller before teardown starts.
pub struct PtyTeardown {
    reader_cancel: Sender<()>,
    writer_cancel: Sender<()>,
    child: Arc<Mutex<ChildState>>,
    /// The child's `exit_observed`, held here so publication never waits for the custody lock.
    exit_observed: Arc<AtomicBool>,
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

/// A program a launcher asked SonicTerm to run directly, with no shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpawn {
    /// The program as the launcher spelled it. A name without a separator is
    /// searched on SonicTerm's own `PATH` (absolute entries only); a path is
    /// used as given, and a relative one resolves against `cwd`.
    pub program: std::ffi::OsString,
    /// The program's arguments, passed verbatim.
    pub args: Vec<std::ffi::OsString>,
    /// The child's working directory: absolute and existing, or the spawn fails.
    pub cwd: PathBuf,
}

#[cfg(test)]
#[path = "pty_tests.rs"]
mod pty_tests;

#[path = "pty_input.rs"]
mod pty_input;

#[path = "pty_output.rs"]
mod pty_output;

#[path = "pty_child.rs"]
mod pty_child;

#[path = "pty_session.rs"]
mod pty_session;

#[path = "pty_native.rs"]
mod pty_native;

#[path = "pty_teardown.rs"]
mod pty_teardown;

#[path = "pty_resize.rs"]
mod pty_resize;

#[path = "pty_writer_file.rs"]
mod pty_writer_file;

#[path = "pty_spawn.rs"]
mod pty_spawn;

#[path = "pty_reader.rs"]
mod pty_reader;

#[path = "pty_writer.rs"]
mod pty_writer;

#[path = "pty_shell.rs"]
mod pty_shell;

#[cfg(all(test, unix))]
use pty_child::unix_child_exit_pending;
use pty_child::{publish_exit, terminate_child_for_platform, terminate_child_without_reap};
pub use pty_input::{max_pty_queued_input_bytes, pty_input_message_allowed};
use pty_input::{pty_input_channel, try_queue_pty_input};
#[cfg(windows)]
use pty_native::{cancel_io_within, cancel_owned_target};
use pty_output::pty_output_channel;
pub use pty_output::{
    max_queued_output_ring_bytes, queued_output_bytes, queued_output_payload_bytes,
    PTY_READ_RING_BYTES,
};
#[cfg(test)]
use pty_reader::send_pty_output;
use pty_reader::spawn_reader_thread;
use pty_resize::resize_callback;
#[cfg(unix)]
use pty_session::terminate_unix_session;
pub use pty_shell::{
    apply_child_pty_env, default_lang_utf8_locale, default_lc_ctype_utf8_locale,
    shell_startup_args, should_apply_utf8_locale_fallback,
};
use pty_shell::{apply_clean_e2e_environment, resolve_spawn_shell};
use pty_writer::spawn_writer_thread;
use pty_writer_file::pty_writer;

#[cfg(test)]
#[path = "pty_test_support.rs"]
mod pty_test_support;

/// The conventional name of Unix signal `signal`, or `signal N` for an unlisted one.
#[cfg(unix)]
pub(crate) fn unix_signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => Some("SIGHUP"),
        libc::SIGINT => Some("SIGINT"),
        libc::SIGQUIT => Some("SIGQUIT"),
        libc::SIGILL => Some("SIGILL"),
        libc::SIGTRAP => Some("SIGTRAP"),
        libc::SIGABRT => Some("SIGABRT"),
        libc::SIGBUS => Some("SIGBUS"),
        libc::SIGFPE => Some("SIGFPE"),
        libc::SIGKILL => Some("SIGKILL"),
        libc::SIGUSR1 => Some("SIGUSR1"),
        libc::SIGSEGV => Some("SIGSEGV"),
        libc::SIGUSR2 => Some("SIGUSR2"),
        libc::SIGPIPE => Some("SIGPIPE"),
        libc::SIGALRM => Some("SIGALRM"),
        libc::SIGTERM => Some("SIGTERM"),
        libc::SIGXCPU => Some("SIGXCPU"),
        libc::SIGXFSZ => Some("SIGXFSZ"),
        libc::SIGSYS => Some("SIGSYS"),
        _ => None,
    };
    name.map_or_else(|| format!("signal {signal}"), str::to_string)
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

fn shell_file_name(shell_path: &str) -> String {
    Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(all(test, unix))]
use pty_session::unix_process_is_active;
