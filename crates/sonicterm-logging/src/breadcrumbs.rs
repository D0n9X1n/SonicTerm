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
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::process_memory::{self, ProcessMemory, ProcessPressure};

const VERSION_CAPACITY: usize = 64;
const COALESCE_IDLE: Duration = Duration::from_millis(5);
const MAX_BATCH_TIME: Duration = Duration::from_millis(50);
/// Smallest supported breadcrumb file budget in bytes.
pub const MIN_FILE_BYTES: u64 = 4096;
/// Smallest lifecycle history that preserves started, ready, and shutdown.
pub const MIN_LIFECYCLE_CAPACITY: usize = 3;
/// Largest accepted ordered lifecycle history.
pub const MAX_LIFECYCLE_CAPACITY: usize = 4096;
/// Largest accepted pending-event queue for one breadcrumb writer.
pub const MAX_QUEUE_CAPACITY: usize = 4096;
/// Largest accepted in-memory pressure-history ring.
pub const MAX_HISTORY_CAPACITY: usize = 4096;
/// Longest accepted interval between independent pressure samples.
pub const MAX_PRESSURE_INTERVAL: Duration = Duration::from_secs(86_400);

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
            // When: bytes fails a shape check — empty, past VERSION_CAPACITY,
            // or carrying syntax a caller could use as a free-form channel.
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

/// Allocator counters permitted in a retention breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreadcrumbAllocator {
    /// Bytes assigned to live allocations.
    pub allocated_bytes: u64,
    /// Bytes reserved across allocator blocks.
    pub reserved_bytes: u64,
    /// Number of live allocations.
    pub allocations: u32,
    /// Number of allocator blocks.
    pub blocks: u32,
    /// Largest allocator block in bytes.
    pub largest_block_bytes: u64,
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
        /// Allocator counters when this build can query them.
        allocator: Option<BreadcrumbAllocator>,
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
            Self::RetentionSnapshot {
                session_bytes,
                renderer_bytes,
                live_renderers,
                allocator,
            } => {
                let base = format!(
                    "{prefix}event=retention session_bytes={session_bytes} \
                     renderer_bytes={renderer_bytes} live_renderers={live_renderers}"
                );
                match allocator {
                    Some(allocator) => format!(
                        "{base} allocator_allocated_bytes={} allocator_reserved_bytes={} \
                         allocator_allocations={} allocator_blocks={} \
                         allocator_largest_block_bytes={}",
                        allocator.allocated_bytes,
                        allocator.reserved_bytes,
                        allocator.allocations,
                        allocator.blocks,
                        allocator.largest_block_bytes
                    ),
                    None => format!("{base} allocator=unsupported"),
                }
            }
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

impl EventKey {
    const ORDER: [Self; 6] = [
        Self::Version,
        Self::Platform,
        Self::Renderer,
        Self::Counts,
        Self::ResourceSnapshot,
        Self::RetentionSnapshot,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Version => 0,
            Self::Platform => 1,
            Self::Renderer => 2,
            Self::Counts => 3,
            Self::ResourceSnapshot => 4,
            Self::RetentionSnapshot => 5,
        }
    }
}

/// Bounds for one breadcrumb writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreadcrumbLimits {
    /// Maximum events waiting for the worker. A full queue drops new events.
    pub queue_capacity: usize,
    /// Maximum ordered lifecycle transitions retained in memory.
    pub ring_capacity: usize,
    /// Maximum bytes persisted in the session breadcrumb file.
    pub max_file_bytes: u64,
    /// Cadence for fixed-cost process-pressure samples.
    pub pressure_interval: Duration,
    /// Maximum process-pressure samples retained in memory.
    pub history_capacity: usize,
}

impl Default for BreadcrumbLimits {
    fn default() -> Self {
        Self {
            queue_capacity: 128,
            ring_capacity: 64,
            max_file_bytes: 64 * 1024,
            pressure_interval: Duration::from_secs(5),
            history_capacity: 48,
        }
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
    // Ordering: queued and dropped load Relaxed — each counter is read for
    // reporting only and orders nothing against other memory.
    fn snapshot(&self) -> BreadcrumbStats {
        BreadcrumbStats {
            queued: self.queued.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerMessage {
    Event(BreadcrumbEvent),
    Deadline,
    Shutdown,
}

#[derive(Debug, Default)]
struct SamplerCancellation {
    cancelled: Mutex<bool>,
    changed: Condvar,
}

impl SamplerCancellation {
    fn cancel(&self) {
        let mut cancelled = self.cancelled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *cancelled = true;
        self.changed.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        *self.cancelled.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn wait_until_cancelled(&self) {
        let mut cancelled = self.cancelled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*cancelled {
            cancelled =
                self.changed.wait(cancelled).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

trait PressureSampler: Send + Sync + 'static {
    fn sample(&self, cancellation: &SamplerCancellation) -> Option<ProcessPressure>;
}

#[derive(Debug)]
struct OsPressureSampler;

impl PressureSampler for OsPressureSampler {
    fn sample(&self, cancellation: &SamplerCancellation) -> Option<ProcessPressure> {
        if cancellation.is_cancelled() {
            // When: cancellation.is_cancelled before the query, no new sample
            // may begin or be delivered to the worker.
            return None;
        }
        let pressure = process_memory::sample_pressure();
        if cancellation.is_cancelled() {
            // When: shutdown arrived during the fixed-cost query, its result is
            // discarded rather than becoming post-cancellation history.
            return None;
        }
        Some(pressure)
    }
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
    // Ordering: queued and dropped fetch_add Relaxed — the counters are a drop
    // tally, never a happens-before edge for the event itself.
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
    cancellation: Arc<SamplerCancellation>,
    worker: Option<thread::JoinHandle<io::Result<()>>>,
}

impl BreadcrumbWriter {
    /// Spawn a writer for one session.
    ///
    /// The function validates the session id and spawns its background worker,
    /// but performs no filesystem IO on the caller.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] for unsafe input limits or session
    /// id, or a thread-spawn error.
    pub fn start(log_dir: &Path, session_id: &str, limits: BreadcrumbLimits) -> io::Result<Self> {
        Self::start_with_sampler_boxed(log_dir, session_id, limits, Arc::new(OsPressureSampler))
    }

    #[cfg(test)]
    fn start_with_sampler<S: PressureSampler>(
        log_dir: &Path,
        session_id: &str,
        limits: BreadcrumbLimits,
        sampler: S,
    ) -> io::Result<Self> {
        Self::start_with_sampler_boxed(log_dir, session_id, limits, Arc::new(sampler))
    }

    fn start_with_sampler_boxed(
        log_dir: &Path,
        session_id: &str,
        limits: BreadcrumbLimits,
        pressure_sampler: Arc<dyn PressureSampler>,
    ) -> io::Result<Self> {
        validate_limits(limits)?;
        let path = breadcrumb_path(log_dir, session_id)?;
        let (sender, receiver) = mpsc::sync_channel(limits.queue_capacity);
        let stats = Arc::new(AtomicStats::default());
        let cancellation = Arc::new(SamplerCancellation::default());
        let worker_cancellation = Arc::clone(&cancellation);
        let worker =
            thread::Builder::new().name("sonicterm-breadcrumbs".to_string()).spawn(move || {
                run_worker(receiver, &path, limits, pressure_sampler, &worker_cancellation)
            })?;
        Ok(Self { sender, stats, cancellation, worker: Some(worker) })
    }

    /// Return a clonable non-blocking recorder.
    #[must_use]
    pub fn recorder(&self) -> BreadcrumbRecorder {
        BreadcrumbRecorder { sender: self.sender.clone(), stats: Arc::clone(&self.stats) }
    }

    /// Flush queued events, stop the background worker, and return counters.
    ///
    /// # Errors
    ///
    /// Returns a worker filesystem error or [`io::ErrorKind::Other`] if a worker
    /// panicked.
    pub fn shutdown(mut self) -> io::Result<BreadcrumbStats> {
        self.cancel_and_join()?;
        Ok(self.stats.snapshot())
    }

    fn cancel_and_join(&mut self) -> io::Result<()> {
        self.cancellation.cancel();
        let _ = self.sender.send(WorkerMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| io::Error::other("breadcrumb worker panicked"))??;
        }
        Ok(())
    }
}

// Lifecycle: BreadcrumbWriter::drop cancels and joins its worker thread handle;
// completed shutdown has already released that handle.
impl Drop for BreadcrumbWriter {
    fn drop(&mut self) {
        let _ = self.cancel_and_join();
    }
}

fn validate_limits(limits: BreadcrumbLimits) -> io::Result<()> {
    let required = required_file_bytes(limits.ring_capacity)?;
    if limits.max_file_bytes < MIN_FILE_BYTES
        || limits.max_file_bytes < required
        || limits.ring_capacity < MIN_LIFECYCLE_CAPACITY
        || limits.ring_capacity > MAX_LIFECYCLE_CAPACITY
        || limits.history_capacity == 0
        || limits.history_capacity > MAX_HISTORY_CAPACITY
        || limits.queue_capacity == 0
        || limits.queue_capacity > MAX_QUEUE_CAPACITY
        || limits.pressure_interval.is_zero()
        || limits.pressure_interval > MAX_PRESSURE_INTERVAL
    {
        // When: a size/capacity bound fails or pressure_interval is unusable,
        // starting cannot preserve the bounded scheduling and storage contract.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid breadcrumb limits"));
    }
    Ok(())
}

fn required_file_bytes(lifecycle_capacity: usize) -> io::Result<u64> {
    let timestamp = i64::MIN;
    let version = AppVersion { bytes: [b'9'; VERSION_CAPACITY], len: VERSION_CAPACITY as u8 };
    let pinned = [
        BreadcrumbEvent::Version(version),
        BreadcrumbEvent::Platform(Platform::Windows),
        BreadcrumbEvent::Renderer {
            identity: RendererIdentity::Software,
            mode: RendererMode::Software,
            adapter: AdapterClass::Software,
        },
        BreadcrumbEvent::Counts { windows: u32::MAX, panes: u32::MAX },
        BreadcrumbEvent::ResourceSnapshot(ProcessMemory {
            private_committed: crate::process_memory::MemoryMetric::Bytes(u64::MAX),
            resident: crate::process_memory::MemoryMetric::Bytes(u64::MAX),
            virtual_bytes: crate::process_memory::MemoryMetric::Bytes(u64::MAX),
        }),
        BreadcrumbEvent::RetentionSnapshot {
            session_bytes: u64::MAX,
            renderer_bytes: u64::MAX,
            live_renderers: u32::MAX,
            allocator: Some(BreadcrumbAllocator {
                allocated_bytes: u64::MAX,
                reserved_bytes: u64::MAX,
                allocations: u32::MAX,
                blocks: u32::MAX,
                largest_block_bytes: u64::MAX,
            }),
        },
    ];
    let pinned_bytes = pinned.iter().try_fold(0u64, |total, event| {
        rendered_line_bytes(event.render(timestamp)).and_then(|bytes| {
            total.checked_add(bytes).ok_or_else(|| invalid_limits("breadcrumb byte bound overflow"))
        })
    })?;
    let lifecycle_line = rendered_line_bytes(
        BreadcrumbEvent::Lifecycle(LifecycleEvent::CleanShutdown).render(timestamp),
    )?;
    let lifecycle_count = u64::try_from(lifecycle_capacity)
        .map_err(|_| invalid_limits("lifecycle capacity exceeds u64"))?;
    let lifecycle_bytes = lifecycle_line
        .checked_mul(lifecycle_count)
        .ok_or_else(|| invalid_limits("lifecycle byte bound overflow"))?;
    let history_bytes = maximum_history_line_bytes()?;
    pinned_bytes
        .checked_add(lifecycle_bytes)
        .and_then(|bytes| bytes.checked_add(history_bytes))
        .ok_or_else(|| invalid_limits("breadcrumb byte bound overflow"))
}

fn maximum_history_line_bytes() -> io::Result<u64> {
    rendered_line_bytes(render_pressure(
        i64::MIN,
        ProcessPressure {
            private_committed: crate::process_memory::MemoryMetric::Bytes(u64::MAX),
            resident: crate::process_memory::MemoryMetric::Bytes(u64::MAX),
        },
    ))
}

fn rendered_line_bytes(line: String) -> io::Result<u64> {
    u64::try_from(line.len())
        .ok()
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| invalid_limits("rendered breadcrumb line exceeds u64"))
}

fn invalid_limits(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[derive(Debug, Clone, Copy)]
struct CapturedEvent {
    timestamp_unix_ms: i64,
    event: BreadcrumbEvent,
}

#[derive(Debug)]
struct WorkerState {
    pinned: [Option<CapturedEvent>; 6],
    lifecycle: VecDeque<CapturedEvent>,
    history: VecDeque<(i64, ProcessPressure)>,
    lifecycle_capacity: usize,
    history_capacity: usize,
}

impl WorkerState {
    fn new(lifecycle_capacity: usize, history_capacity: usize) -> Self {
        Self {
            pinned: [None; 6],
            lifecycle: VecDeque::with_capacity(lifecycle_capacity),
            history: VecDeque::with_capacity(history_capacity),
            lifecycle_capacity,
            history_capacity,
        }
    }

    fn capture(&mut self, event: BreadcrumbEvent) {
        let captured =
            CapturedEvent { timestamp_unix_ms: chrono::Utc::now().timestamp_millis(), event };
        match event.key() {
            Some(key) => self.pinned[key.index()] = Some(captured),
            None => self.capture_lifecycle(captured),
        }
    }

    fn capture_lifecycle(&mut self, captured: CapturedEvent) {
        if self.lifecycle.len() >= self.lifecycle_capacity {
            // When: lifecycle.len reaches lifecycle_capacity, discard the oldest
            // transition before appending the accepted captured transition.
            let _ = self.lifecycle.pop_front();
        }
        self.lifecycle.push_back(captured);
    }

    fn capture_pressure(&mut self, pressure: ProcessPressure) {
        while self.history.len() >= self.history_capacity {
            let _ = self.history.pop_front();
        }
        self.history.push_back((chrono::Utc::now().timestamp_millis(), pressure));
    }

    fn render_for_limit(&self, max_bytes: u64) -> Vec<String> {
        let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let mut lines = Vec::new();
        let mut used = 0usize;
        for key in EventKey::ORDER {
            if let Some(captured) = self.pinned[key.index()] {
                push_line(&mut lines, &mut used, captured.event.render(captured.timestamp_unix_ms));
            }
        }
        for captured in &self.lifecycle {
            push_line(&mut lines, &mut used, captured.event.render(captured.timestamp_unix_ms));
        }

        let mut selected_history = VecDeque::new();
        for (timestamp, pressure) in self.history.iter().rev() {
            let line = render_pressure(*timestamp, *pressure);
            let bytes = line.len().saturating_add(1);
            if bytes > max_bytes.saturating_sub(used) {
                // When: bytes exceeds max_bytes - used, this next-oldest history
                // line and every older line are omitted from the byte budget.
                break;
            }
            used = used.saturating_add(bytes);
            selected_history.push_front(line);
        }
        lines.extend(selected_history);
        lines
    }
}

fn push_line(lines: &mut Vec<String>, used: &mut usize, line: String) {
    *used = used.saturating_add(line.len().saturating_add(1));
    lines.push(line);
}

fn render_pressure(timestamp_unix_ms: i64, pressure: ProcessPressure) -> String {
    format!(
        "time_unix_ms={timestamp_unix_ms} event=resource_history private_committed={} resident={}",
        pressure.private_committed, pressure.resident
    )
}

fn run_worker(
    receiver: mpsc::Receiver<WorkerMessage>,
    path: &Path,
    limits: BreadcrumbLimits,
    sampler: Arc<dyn PressureSampler>,
    cancellation: &SamplerCancellation,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "breadcrumb path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;

    let mut state = WorkerState::new(limits.ring_capacity, limits.history_capacity);
    if let Some(pressure) = sampler.sample(cancellation) {
        state.capture_pressure(pressure);
    }
    persist_state(path, &state, limits.max_file_bytes)?;
    let mut next_sample_at = Instant::now() + limits.pressure_interval;

    loop {
        if cancellation.is_cancelled() {
            // When: cancellation is set, drain at most the bounded queue snapshot,
            // persist it, and exit even if recorder clones remain alive.
            for message in
                std::iter::from_fn(|| receiver.try_recv().ok()).take(limits.queue_capacity)
            {
                let _ = process_message(message, &mut state);
            }
            persist_state(path, &state, limits.max_file_bytes)?;
            break;
        }
        let now = Instant::now();
        if now >= next_sample_at {
            // When: now reaches next_sample_at, sample before receiving another
            // caller event so a continuously populated queue cannot starve it.
            if let Some(pressure) = sampler.sample(cancellation) {
                state.capture_pressure(pressure);
                persist_state(path, &state, limits.max_file_bytes)?;
            }
            next_sample_at = Instant::now() + limits.pressure_interval;
            continue;
        }

        let message = receive_next_message(
            &receiver,
            next_sample_at.saturating_duration_since(Instant::now()),
        );
        if message == WorkerMessage::Deadline {
            // When: message is Deadline, return to the priority check so the
            // sampler runs before any subsequently queued caller event.
            continue;
        }
        let mut shutting_down = process_message(message, &mut state);
        let batch_started = Instant::now();
        while !shutting_down
            && !cancellation.is_cancelled()
            && batch_started.elapsed() < MAX_BATCH_TIME
            && Instant::now() < next_sample_at
        {
            let until_deadline = next_sample_at.saturating_duration_since(Instant::now());
            let wait = COALESCE_IDLE.min(until_deadline);
            match receiver.recv_timeout(wait) {
                Ok(message) => shutting_down = process_message(message, &mut state),
                Err(error) => {
                    // When: recv_timeout returns an error, Timeout closes this
                    // batch and Disconnected also requests worker shutdown.
                    if error == mpsc::RecvTimeoutError::Disconnected {
                        shutting_down = true;
                    }
                    break;
                }
            }
        }
        persist_state(path, &state, limits.max_file_bytes)?;
        if shutting_down {
            // When: shutting_down is set, the final accepted state is persisted
            // before the worker exits.
            break;
        }
    }
    Ok(())
}

fn receive_next_message(
    receiver: &mpsc::Receiver<WorkerMessage>,
    timeout: Duration,
) -> WorkerMessage {
    match receiver.recv_timeout(timeout) {
        Ok(message) => message,
        Err(mpsc::RecvTimeoutError::Timeout) => WorkerMessage::Deadline,
        Err(mpsc::RecvTimeoutError::Disconnected) => WorkerMessage::Shutdown,
    }
}

fn process_message(message: WorkerMessage, state: &mut WorkerState) -> bool {
    match message {
        WorkerMessage::Event(event) => {
            state.capture(event);
            false
        }
        WorkerMessage::Deadline => false,
        WorkerMessage::Shutdown => true,
    }
}

fn persist_state(path: &Path, state: &WorkerState, max_bytes: u64) -> io::Result<()> {
    persist_state_with_replace(path, state, max_bytes, crate::path::replace_file)
}

fn persist_state_with_replace(
    path: &Path,
    state: &WorkerState,
    max_bytes: u64,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let lines = state.render_for_limit(max_bytes);
    let used = lines.iter().map(|line| line.len().saturating_add(1)).sum();
    let mut contents = String::with_capacity(used);
    for line in lines {
        contents.push_str(&line);
        contents.push('\n');
    }

    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match replace(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            // When: replace returns error, remove the partial candidate and
            // retain the previous complete destination.
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
        // When: session_id is empty, over-long, or carries path syntax, so it
        // could steer the write outside the breadcrumbs directory.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsafe breadcrumb session id"));
    }
    Ok(log_dir.join("breadcrumbs").join(format!("breadcrumbs-{session_id}.log")))
}

#[cfg(test)]
#[path = "breadcrumbs_tests.rs"]
mod breadcrumbs_tests;
