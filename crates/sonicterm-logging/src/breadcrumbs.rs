//! Bounded diagnostic breadcrumbs with a privacy-preserving event vocabulary.
//!
//! Callers can record only [`BreadcrumbEvent`] variants. There is no free-form
//! text variant, so terminal output, commands, environment values, tokens, and
//! credentials have no route into this file format.
//!
//! Recording uses [`std::sync::mpsc::SyncSender::try_send`]. A UI, renderer, or
//! PTY caller therefore either queues an event immediately or drops it
//! immediately; it never waits for filesystem IO or for queue capacity. A
//! background thread coalesces state updates, keeps a bounded ring, and rewrites
//! a bounded on-disk snapshot.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use crate::process_memory::{self, ProcessMemory};

const VERSION_CAPACITY: usize = 64;
const COALESCE_IDLE: Duration = Duration::from_millis(5);
const MAX_BATCH_TIME: Duration = Duration::from_millis(50);

/// A validated application version suitable for a breadcrumb.
///
/// The fixed-size representation keeps each queued event bounded. Parsing
/// accepts release-style versions such as `1.2.3` and `1.2.3-alpha.1`, but
/// rejects whitespace, line breaks, key/value separators, and path syntax so a
/// caller cannot use this field as a free-form text channel.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppVersion {
    bytes: [u8; VERSION_CAPACITY],
    len: u8,
}

impl AppVersion {
    /// Return the validated version text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let len = usize::from(self.len);
        // Parsing accepts ASCII only, so these bytes are always valid UTF-8.
        std::str::from_utf8(&self.bytes[..len]).expect("validated version is ASCII")
    }
}

impl fmt::Debug for AppVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AppVersion").field(&self.as_str()).finish()
    }
}

impl fmt::Display for AppVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an [`AppVersion`] string was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppVersionError;

impl fmt::Display for AppVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("version must be a bounded release-style ASCII identifier")
    }
}

impl std::error::Error for AppVersionError {}

impl FromStr for AppVersion {
    type Err = AppVersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > VERSION_CAPACITY
            || !bytes[0].is_ascii_digit()
            || bytes.iter().filter(|byte| **byte == b'.').count() < 2
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-' | b'+'))
        {
            return Err(AppVersionError);
        }

        let mut stored = [0; VERSION_CAPACITY];
        stored[..bytes.len()].copy_from_slice(bytes);
        Ok(Self { bytes: stored, len: u8::try_from(bytes.len()).map_err(|_| AppVersionError)? })
    }
}

/// Platform identity permitted in a breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Apple macOS.
    MacOs,
    /// Microsoft Windows.
    Windows,
    /// A non-shipping platform used for development or tests.
    Other,
}

impl Platform {
    /// Identify the platform this binary was compiled for.
    #[must_use]
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            Self::Other
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// Adapter classification permitted in a breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterClass {
    /// A hardware graphics adapter.
    Hardware,
    /// A CPU/software graphics adapter.
    Software,
}

impl AdapterClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
        }
    }
}

/// Renderer implementation identity permitted in a breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererIdentity {
    /// The wgpu renderer.
    Wgpu,
    /// SonicTerm's software renderer.
    Software,
}

impl RendererIdentity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::Software => "software",
        }
    }
}

/// Active rendering mode permitted in a breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererMode {
    /// Hardware GPU presentation.
    Gpu,
    /// Software rasterization or presentation.
    Software,
}

impl RendererMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Software => "software",
        }
    }
}

/// Allowlisted process lifecycle transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// Startup began.
    Started,
    /// Startup completed and the application can serve input.
    Ready,
    /// The explicit clean-shutdown path began.
    CleanShutdown,
}

impl LifecycleEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Ready => "ready",
            Self::CleanShutdown => "clean_shutdown",
        }
    }
}

/// One privacy-allowlisted breadcrumb.
///
/// No variant accepts arbitrary text. In particular there is no terminal,
/// shell, command, environment, token, or credential field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreadcrumbEvent {
    /// Application version.
    Version(AppVersion),
    /// Operating-system platform.
    Platform(Platform),
    /// Renderer implementation and active mode.
    Renderer {
        /// Renderer implementation.
        identity: RendererIdentity,
        /// Active rendering mode.
        mode: RendererMode,
        /// Hardware or software adapter classification.
        adapter: AdapterClass,
    },
    /// Current top-level window and terminal pane counts.
    Counts {
        /// Open top-level windows.
        windows: u32,
        /// Open terminal panes.
        panes: u32,
    },
    /// What the OS reports this process is holding.
    ResourceSnapshot(ProcessMemory),
    /// App-accounted session and renderer retention for the same sample cycle.
    RetentionSnapshot {
        /// Bytes retained across sampled pane seams.
        session_bytes: u64,
        /// Bytes retained by visible and warm renderers.
        renderer_bytes: u64,
        /// Number of live renderer instances.
        live_renderers: u32,
    },
    /// An allowlisted lifecycle transition.
    Lifecycle(LifecycleEvent),
}

impl BreadcrumbEvent {
    /// Sample process memory through the crate's canonical OS sampler.
    #[must_use]
    pub fn sample_resources() -> Self {
        Self::ResourceSnapshot(process_memory::sample())
    }

    fn key(self) -> Option<EventKey> {
        match self {
            Self::Version(_) => Some(EventKey::Version),
            Self::Platform(_) => Some(EventKey::Platform),
            Self::Renderer { .. } => Some(EventKey::Renderer),
            Self::Counts { .. } => Some(EventKey::Counts),
            Self::ResourceSnapshot(_) => Some(EventKey::ResourceSnapshot),
            Self::RetentionSnapshot { .. } => Some(EventKey::RetentionSnapshot),
            Self::Lifecycle(_) => None,
        }
    }

    fn render(self, timestamp_unix_ms: i64) -> String {
        let prefix = format!("time_unix_ms={timestamp_unix_ms} ");
        match self {
            Self::Version(version) => format!("{prefix}event=version version={version}"),
            Self::Platform(platform) => {
                format!("{prefix}event=platform platform={}", platform.as_str())
            }
            Self::Renderer { identity, mode, adapter } => format!(
                "{prefix}event=renderer identity={} mode={} adapter={}",
                identity.as_str(),
                mode.as_str(),
                adapter.as_str()
            ),
            Self::Counts { windows, panes } => {
                format!("{prefix}event=counts windows={windows} panes={panes}")
            }
            Self::ResourceSnapshot(memory) => format!(
                "{prefix}event=resource private_committed={} resident={} virtual={}",
                memory.private_committed, memory.resident, memory.virtual_bytes
            ),
            Self::RetentionSnapshot { session_bytes, renderer_bytes, live_renderers } => format!(
                "{prefix}event=retention session_bytes={session_bytes} \
                 renderer_bytes={renderer_bytes} live_renderers={live_renderers}"
            ),
            Self::Lifecycle(event) => {
                format!("{prefix}event=lifecycle lifecycle={}", event.as_str())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKey {
    Version,
    Platform,
    Renderer,
    Counts,
    ResourceSnapshot,
    RetentionSnapshot,
}

/// Bounds for one breadcrumb writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreadcrumbLimits {
    /// Maximum events waiting for the worker. A full queue drops new events.
    pub queue_capacity: usize,
    /// Maximum coalesced events retained in memory.
    pub ring_capacity: usize,
    /// Maximum bytes persisted in the session breadcrumb file.
    pub max_file_bytes: u64,
}

impl Default for BreadcrumbLimits {
    fn default() -> Self {
        Self { queue_capacity: 128, ring_capacity: 64, max_file_bytes: 64 * 1024 }
    }
}

/// Result of a non-blocking record attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// The bounded worker queue accepted the event.
    Queued,
    /// The queue was full, so the event was dropped rather than blocking.
    DroppedFull,
    /// The worker had already stopped.
    WorkerStopped,
}

/// Counts of accepted and dropped record attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BreadcrumbStats {
    /// Events accepted by the worker queue.
    pub queued: u64,
    /// Events dropped because the queue was full or the worker had stopped.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct AtomicStats {
    queued: AtomicU64,
    dropped: AtomicU64,
}

impl AtomicStats {
    fn snapshot(&self) -> BreadcrumbStats {
        BreadcrumbStats {
            queued: self.queued.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkerMessage {
    Event(BreadcrumbEvent),
    Shutdown,
}

/// A cheap, clonable handle used by UI, renderer, and PTY paths.
#[derive(Debug, Clone)]
pub struct BreadcrumbRecorder {
    sender: mpsc::SyncSender<WorkerMessage>,
    stats: Arc<AtomicStats>,
}

impl BreadcrumbRecorder {
    #[cfg(test)]
    fn from_sender(sender: mpsc::SyncSender<WorkerMessage>) -> Self {
        Self { sender, stats: Arc::new(AtomicStats::default()) }
    }

    /// Attempt to queue one event without waiting.
    ///
    /// This method performs no filesystem IO and uses only `try_send`. It is
    /// therefore safe for latency-sensitive caller paths: pressure produces a
    /// drop result, never backpressure.
    #[must_use]
    pub fn record(&self, event: BreadcrumbEvent) -> RecordOutcome {
        match self.sender.try_send(WorkerMessage::Event(event)) {
            Ok(()) => {
                self.stats.queued.fetch_add(1, Ordering::Relaxed);
                RecordOutcome::Queued
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                RecordOutcome::DroppedFull
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                RecordOutcome::WorkerStopped
            }
        }
    }

    /// Snapshot accepted and dropped attempt counts.
    #[must_use]
    pub fn stats(&self) -> BreadcrumbStats {
        self.stats.snapshot()
    }
}

/// Owns the asynchronous breadcrumb worker.
#[derive(Debug)]
pub struct BreadcrumbWriter {
    sender: mpsc::SyncSender<WorkerMessage>,
    stats: Arc<AtomicStats>,
    worker: Option<thread::JoinHandle<io::Result<()>>>,
}

impl BreadcrumbWriter {
    /// Spawn a writer for one session.
    ///
    /// The function validates the session id and spawns a thread, but performs
    /// no filesystem IO. Directory creation and all writes happen on the worker.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] for an unsafe session id, or the
    /// thread-spawn error when the worker cannot be started.
    pub fn start(log_dir: &Path, session_id: &str, limits: BreadcrumbLimits) -> io::Result<Self> {
        let path = breadcrumb_path(log_dir, session_id)?;
        let (sender, receiver) = mpsc::sync_channel(limits.queue_capacity);
        let stats = Arc::new(AtomicStats::default());
        let worker = thread::Builder::new()
            .name("sonicterm-breadcrumbs".to_string())
            .spawn(move || run_worker(receiver, &path, limits))?;
        Ok(Self { sender, stats, worker: Some(worker) })
    }

    /// Return a clonable non-blocking recorder.
    #[must_use]
    pub fn recorder(&self) -> BreadcrumbRecorder {
        BreadcrumbRecorder { sender: self.sender.clone(), stats: Arc::clone(&self.stats) }
    }

    /// Flush queued events, stop the worker, and return final counters.
    ///
    /// Shutdown may wait for the background worker. Latency-sensitive paths use
    /// [`BreadcrumbRecorder::record`], which never waits.
    ///
    /// # Errors
    ///
    /// Returns a worker filesystem error or [`io::ErrorKind::Other`] if the
    /// worker panicked.
    pub fn shutdown(mut self) -> io::Result<BreadcrumbStats> {
        let _ = self.sender.send(WorkerMessage::Shutdown);
        let result = self.worker.take().map_or(Ok(()), |worker| {
            worker.join().map_err(|_| io::Error::other("breadcrumb worker panicked"))?
        });
        result.map(|()| self.stats.snapshot())
    }
}

#[derive(Debug, Clone, Copy)]
struct CapturedEvent {
    timestamp_unix_ms: i64,
    event: BreadcrumbEvent,
}

fn run_worker(
    receiver: mpsc::Receiver<WorkerMessage>,
    path: &Path,
    limits: BreadcrumbLimits,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "breadcrumb path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;

    let mut ring = VecDeque::with_capacity(limits.ring_capacity);
    // `recv` ends only when every sender is gone, which is what a dropped
    // writer looks like from here. Blocking on the first message of each batch
    // is what keeps an idle session from spinning this thread.
    while let Ok(first) = receiver.recv() {
        let mut shutting_down = process_message(first, &mut ring, limits.ring_capacity);
        let batch_started = Instant::now();

        while !shutting_down && batch_started.elapsed() < MAX_BATCH_TIME {
            match receiver.recv_timeout(COALESCE_IDLE) {
                Ok(message) => {
                    shutting_down = process_message(message, &mut ring, limits.ring_capacity);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    shutting_down = true;
                }
            }
        }

        persist_ring(path, &ring, limits.max_file_bytes)?;
        if shutting_down {
            break;
        }
    }
    Ok(())
}

fn process_message(
    message: WorkerMessage,
    ring: &mut VecDeque<CapturedEvent>,
    capacity: usize,
) -> bool {
    let WorkerMessage::Event(event) = message else { return true };
    if capacity == 0 {
        return false;
    }

    if let Some(key) = event.key() {
        if let Some(index) = ring.iter().position(|captured| captured.event.key() == Some(key)) {
            let _ = ring.remove(index);
        }
    }
    while ring.len() >= capacity {
        let _ = ring.pop_front();
    }
    ring.push_back(CapturedEvent {
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        event,
    });
    false
}

fn persist_ring(path: &Path, ring: &VecDeque<CapturedEvent>, max_bytes: u64) -> io::Result<()> {
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut selected = VecDeque::new();
    let mut used = 0usize;

    for captured in ring.iter().rev() {
        let line = captured.event.render(captured.timestamp_unix_ms);
        let line_bytes = line.len().saturating_add(1);
        if line_bytes > max_bytes.saturating_sub(used) {
            break;
        }
        used = used.saturating_add(line_bytes);
        selected.push_front(line);
    }

    let mut contents = String::with_capacity(used);
    for line in selected {
        contents.push_str(&line);
        contents.push('\n');
    }

    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match crate::path::replace_file(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Return the per-session breadcrumb file path.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when `session_id` is empty, too
/// long, or contains anything except ASCII letters, digits, `.`, `_`, and `-`.
pub fn breadcrumb_path(log_dir: &Path, session_id: &str) -> io::Result<PathBuf> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsafe breadcrumb session id"));
    }
    Ok(log_dir.join("breadcrumbs").join(format!("breadcrumbs-{session_id}.log")))
}

#[cfg(test)]
#[path = "breadcrumbs_tests.rs"]
mod breadcrumbs_tests;
