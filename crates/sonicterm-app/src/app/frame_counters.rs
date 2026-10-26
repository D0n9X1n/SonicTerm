//! Debug-only frame and lock counters.
//!
//! An App decides once, at construction, whether it counts; with its gate off nothing here
//! runs after that check: no clock read, no atomic or thread-local write and no allocation.
//! Every count and duration sum is cumulative and never reset, so each reader keeps its own
//! previous snapshot and takes deltas. Maximums and p95s are bucket bounds, never exact values.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, MutexGuard};

/// Upper bounds of the millisecond buckets; a larger value lands in the overflow bucket.
const MILLIS_BOUNDS: [u64; 9] = [4, 7, 9, 12, 17, 25, 34, 50, 100];
/// Upper bounds of the microsecond buckets; a larger value lands in the overflow bucket.
const MICROS_BOUNDS: [u64; 6] = [10, 50, 100, 500, 1_000, 5_000];
/// Bucket slots: the longer bound list plus its overflow bucket.
const BUCKETS: usize = MILLIS_BOUNDS.len() + 1;
/// Least spacing between two lines from one source.
const LINE_INTERVAL: Duration = Duration::from_secs(1);

/// The bucket scale of a histogram; every histogram sums microseconds either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistogramUnit {
    /// Millisecond buckets, for frame-scale durations.
    Millis,
    /// Microsecond buckets, for lock waits, holds and parses.
    Micros,
}

/// Where a maximum or p95 lies: at or below a bucket bound, or in the overflow bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// At or below this bound, in the histogram's unit.
    AtMost(u64),
    /// Above the largest bound, in the histogram's unit.
    Above(u64),
}

/// A cumulative bucket histogram and the exact sum of its recorded microseconds.
#[derive(Clone, Debug)]
pub(crate) struct Histogram {
    unit: HistogramUnit,
    buckets: [u64; BUCKETS],
    sum_us: u64,
}

/// A histogram that several threads record into.
#[derive(Debug)]
pub(crate) struct AtomicHistogram {
    unit: HistogramUnit,
    buckets: [AtomicU64; BUCKETS],
    sum_us: AtomicU64,
}

/// Limits one counter source to one line a second and one `final=1` line.
#[derive(Clone, Debug, Default)]
pub(crate) struct LineCadence {
    last_line: Option<Instant>,
    finished: bool,
}

impl LineCadence {
    /// Whether to print a line now. `authorized` is false for maintenance wakes, which count
    /// but never print, so no timer is ever armed for a line.
    pub(crate) fn line_due(&mut self, now: Instant, authorized: bool) -> bool {
        let too_soon =
            self.last_line.is_some_and(|last| now.saturating_duration_since(last) < LINE_INTERVAL);
        if self.finished || !authorized || too_soon {
            // When: `finished`, not `authorized` (a maintenance wake) or `too_soon`: no line prints.
            return false;
        }
        self.last_line = Some(now);
        true
    }

    /// Whether a line could print at `now`, checked before any record is built.
    pub(crate) fn ready(&self, now: Instant) -> bool {
        !self.finished
            && self
                .last_line
                .is_none_or(|last| now.saturating_duration_since(last) >= LINE_INTERVAL)
    }

    /// Close the source at window close or exit; true once, when `pending` counts need a line.
    pub(crate) fn final_line(&mut self, pending: bool) -> bool {
        let first = !self.finished;
        self.finished = true;
        first && pending
    }
}

/// An App's event-loop-thread parser waits.
#[derive(Debug)]
pub(crate) struct DispatchTotals {
    /// Waits for a pane parser's lock, in microseconds.
    pub(crate) wait: AtomicHistogram,
    /// Locks taken.
    pub(crate) locks: AtomicU64,
    /// Native redraw requests: per registered window, one shared count for every other id, and
    /// the tally storage dispatches reuse.
    native: Mutex<NativeTotals>,
}

/// Native redraw requests an App has drained. Only registered windows keep an entry, added at
/// registration and removed at retirement, so storage never grows per abandoned id.
#[derive(Debug, Default)]
struct NativeTotals {
    /// Each registered window's requests.
    registered: Vec<(winit::window::WindowId, u64)>,
    /// Requests for ids that were not registered when drained: before registration, after
    /// retirement, or never registered at all.
    unregistered: u64,
    /// The last dispatch's tally storage, cleared, for the next dispatch to reuse.
    spare: Vec<(winit::window::WindowId, u64)>,
}

impl Default for DispatchTotals {
    fn default() -> Self {
        Self {
            wait: AtomicHistogram::new(HistogramUnit::Micros),
            locks: AtomicU64::new(0),
            native: Mutex::new(NativeTotals::default()),
        }
    }
}

/// Distinct windows one dispatch tallies before it hands its requests to the App.
const NATIVE_TALLY_CAPACITY: usize = 16;

/// Plain counters a dispatch accumulates on the event-loop thread.
#[derive(Debug)]
struct DispatchTally {
    wait: Histogram,
    locks: u64,
    /// Native redraw requests by window in this dispatch, at most `NATIVE_TALLY_CAPACITY` ids.
    native: Vec<(winit::window::WindowId, u64)>,
    /// The App this dispatch counts for, so a full tally can hand its requests over.
    sink: Option<Arc<DispatchTotals>>,
}

thread_local! {
    /// Whether the open dispatch on this thread belongs to an App whose gate is on.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// The open dispatch's parser waits, drained into its App when the dispatch ends.
    static TALLY: RefCell<DispatchTally> = const { RefCell::new(DispatchTally::new()) };
}

/// One dispatch on the event-loop thread. While a counting scope is open, `lock_parser` times
/// each pane-parser lock; dropping the scope, on return or unwind, drains the waits into the App
/// that opened it and restores the enclosing dispatch.
pub(crate) struct DispatchScope {
    sink: Option<Arc<DispatchTotals>>,
    /// The enclosing dispatch's gate and tally, when this scope replaced them.
    outer: Option<(bool, DispatchTally)>,
}

// Lifecycle: DispatchScope drains its own tally into its App, then restores the enclosing dispatch.
impl Drop for DispatchScope {
    fn drop(&mut self) {
        let Some((enclosing_counts, outer_tally)) = self.outer.take() else {
            // When: `outer` is None, this scope replaced nothing, so nothing is drained or restored.
            return;
        };
        let tally = TALLY.with(|cell| cell.replace(outer_tally));
        COUNTING.with(|cell| cell.set(enclosing_counts));
        if let Some(sink) = &self.sink {
            sink.absorb(tally);
        }
    }
}

/// The four instants of one VT parser section: before `lock()`, as it returns, after parsing
/// and after the keyboard-snapshot store, the last three taken under the guard.
pub(crate) struct VtSectionTimes {
    /// Before `lock()`, outside the guard.
    pub(crate) before_lock: Instant,
    /// As `lock()` returned.
    pub(crate) locked_at: Instant,
    /// After `advance_with_replies`.
    pub(crate) parsed_at: Instant,
    /// After the keyboard-snapshot store, before the guard drops.
    pub(crate) released_at: Instant,
}

/// The App-wide VT statistics every pane's worker records into; it lives as long as the App.
#[derive(Debug)]
pub(crate) struct VtFrameStats {
    /// Waits for the parser lock.
    pub(crate) parser_lock_wait: AtomicHistogram,
    /// Holds of the parser lock.
    pub(crate) parser_lock_hold: AtomicHistogram,
    /// Parses under the lock.
    pub(crate) parse: AtomicHistogram,
    /// Bytes parsed.
    pub(crate) parse_bytes: AtomicU64,
    /// Output batches handled.
    pub(crate) batches: AtomicU64,
    /// Redraw requests sent after output, with or without a target.
    pub(crate) flushes: AtomicU64,
    /// Flushes while the pane had no redraw target.
    pub(crate) flushes_untargeted: AtomicU64,
    /// Flushes that found an earlier one still pending.
    pub(crate) flushes_coalesced: AtomicU64,
    /// Targeted flushes that sent no event because the pane's output event was outstanding.
    pub(crate) flushes_suppressed: AtomicU64,
    /// Synchronized updates (DEC 2026) a worker released at the 150 ms bound, not at their reset.
    pub(crate) sync_timeouts: AtomicU64,
    /// Nanoseconds the workers waited for a parser lock, summed over every section.
    pub(crate) parser_lock_wait_ns: AtomicU64,
    /// Parser sections recorded; the count `parser_lock_wait_ns` sums over.
    pub(crate) parser_sections: AtomicU64,
}

impl Default for VtFrameStats {
    fn default() -> Self {
        Self {
            parser_lock_wait: AtomicHistogram::new(HistogramUnit::Micros),
            parser_lock_hold: AtomicHistogram::new(HistogramUnit::Micros),
            parse: AtomicHistogram::new(HistogramUnit::Micros),
            parse_bytes: AtomicU64::new(0),
            batches: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            flushes_untargeted: AtomicU64::new(0),
            flushes_coalesced: AtomicU64::new(0),
            flushes_suppressed: AtomicU64::new(0),
            sync_timeouts: AtomicU64::new(0),
            parser_lock_wait_ns: AtomicU64::new(0),
            parser_sections: AtomicU64::new(0),
        }
    }
}

/// A pane's counter handles, present only when its App's gate is on. The pane and its VT
/// worker share them: the worker publishes into `pending_flush` and the event loop takes it.
#[derive(Clone, Debug)]
pub(crate) struct PaneFrameCounters {
    /// The App-wide VT statistics.
    pub(crate) vt: Arc<VtFrameStats>,
    /// The oldest flush not yet consumed, as `flush_clock_ns`; 0 means none.
    pub(crate) pending_flush: Arc<AtomicU64>,
    /// The pane's echo watch; it lives as long as the pane or its worker's handles.
    pub(crate) echo: Arc<super::echo_watch::EchoWatch>,
    /// The pane's guard-correlation section log, which its worker records into; `None` until the App
    /// attaches one, and the pane's owner token, not this handle, closes it.
    pub(crate) sections: Option<Arc<super::guard_correlation::PaneSectionLog>>,
}

#[cfg(test)]
thread_local! {
    /// Test-only: `flush_clock_ns` reads made on this thread.
    static FLUSH_CLOCK_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The run clock's process-wide epoch: set once, at its first use, and never reset.
static CLOCK_EPOCH: OnceLock<Instant> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Test-only: the epoch cell `AppFrameCounters::new` initializes on this thread instead of the process's,
    /// so a test can observe the constructor setting a cell nothing else has touched.
    pub(crate) static CONSTRUCTOR_EPOCH_CELL: std::cell::Cell<Option<&'static OnceLock<Instant>>> =
        const { std::cell::Cell::new(None) };
}

/// The frame and lock counters one App owns, present only when its gate is on.
#[derive(Debug)]
pub(crate) struct AppFrameCounters {
    /// VT statistics every pane's worker records into; fixed in size for the App's lifetime.
    pub(crate) vt: Arc<VtFrameStats>,
    /// Event-loop-thread parser waits, drained at each dispatch end.
    pub(crate) dispatch: Arc<DispatchTotals>,
    /// Every counting frame's parser-guard custody and dispatch interval.
    pub(crate) custody: Arc<super::guard_custody::CustodyTotals>,
    /// Foreground-probe worker statistics, shared with the App's worker when it starts.
    pub(crate) fg_worker: Arc<super::fg_probe::ForegroundWorkerStats>,
    /// `new_events` wakes by `StartCause::Init`.
    pub(crate) wake_init: u64,
    /// `new_events` wakes by `StartCause::Poll`.
    pub(crate) wake_poll: u64,
    /// `new_events` wakes by `StartCause::WaitCancelled`.
    pub(crate) wake_wait_cancelled: u64,
    /// `new_events` wakes by `StartCause::ResumeTimeReached`.
    pub(crate) wake_resume_time: u64,
    /// `user_event` dispatches.
    pub(crate) wake_user: u64,
    /// `about_to_wait` dispatch durations.
    pub(crate) about_to_wait: Histogram,
    /// `user_event` dispatch durations.
    pub(crate) user_event: Histogram,
    /// `new_events` dispatch durations.
    pub(crate) new_events: Histogram,
    /// The `window=app` line's cadence and previous record.
    pub(crate) line: LineState,
    /// Every closed window's totals, merged so the sum never drops; fixed in size.
    pub(crate) closed_windows: CounterRecord,
    /// Windows registered so far; numbers each window's line.
    windows_registered: u64,
    /// The window whose `window_event` dispatch is running; its retirement waits for its
    /// handler time.
    dispatching: Option<winit::window::WindowId>,
    /// A window that dispatch closed, kept until its handler time is recorded.
    closing: Option<ClosingWindow>,
    /// Each pane's armed S10 attribution watch, by pane id; at most one per pane.
    pub(crate) s10_watches: std::collections::HashMap<u64, super::perf_present::S10Watch>,
    /// The arming id the next S10 watch takes; starts at 1, so 0 never names an arming.
    pub(crate) next_s10_arming: u64,
    /// Raw guard-correlation records: every pane's section log, the UI span store and the take sequence.
    pub(crate) correlation: super::guard_correlation::GuardCorrelation,
    /// URL and path hover work on the event-loop thread.
    pub(crate) hover: HoverCounters,
    /// Status line samples, timer wakes and strip-only redraw requests.
    pub(crate) status_line: StatusLineCounters,
    /// The main window's startup step times.
    pub(crate) startup: StartupSpans,
}

/// Status line work on the event-loop thread, cumulative for the App's lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusLineCounters {
    /// Memory samples the status line took.
    pub(crate) samples: u64,
    /// Syncs that serviced a due status line sample or pending path check.
    pub(crate) wakes: u64,
    /// Windows whose strip pixels changed, each one `StatusLine` redraw request.
    pub(crate) redraws: u64,
    /// Selection counts the frame passes ran; an unchanged selection is never recounted.
    pub(crate) sel_counts: u64,
    /// Cells those counts read, at most 200,000 per count.
    pub(crate) sel_cells: u64,
    /// Times `History: trimmed` appeared in a window's strip.
    pub(crate) history_shows: u64,
    /// Times `Input: held` appeared in a window's strip.
    pub(crate) held_shows: u64,
    /// Times `Glyphs:` appeared in a window's strip.
    pub(crate) glyph_shows: u64,
    /// `OSC 9;4` progress changes the App received, one per VT batch that changed it.
    pub(crate) program_progress: u64,
    /// OSC 9 / OSC 777 notification batches the App received.
    pub(crate) program_notifies: u64,
    /// Redraws a program toast requested: one per batch it opened or counted.
    pub(crate) program_toast_redraws: u64,
    /// Values the taskbar, Dock or launcher surface received.
    pub(crate) taskbar_deliveries: u64,
    /// Accessibility trees sent; stays 0 while no assistive technology is connected.
    pub(crate) tree_builds: u64,
    /// Syncs that ran the full per-window pass; stays 0 while the strip is hidden and unheard.
    pub(crate) full_passes: u64,
}

/// Time the main window's startup steps took between the wgpu adapter and its first pane, in ns, so
/// a startup regression in that interval names its step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartupSpans {
    /// Creating the native window.
    pub(crate) window_ns: u64,
    /// Installing the dormant accessibility adapter.
    pub(crate) adapter_install_ns: u64,
    /// Creating the renderer, its surface and its font stacks.
    pub(crate) renderer_ns: u64,
    /// Recording the startup load failures for the status line.
    pub(crate) load_checks_ns: u64,
    /// Seeding the first tab and launching its pane.
    pub(crate) pane_spawn_ns: u64,
}

/// URL and path detection work on the event-loop thread, cumulative for the App's lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HoverCounters {
    /// Hover lookups that reached a pane cell, from a frame or a pointer or modifier event.
    pub(crate) lookups: u64,
    /// Lookups that ran text detection.
    pub(crate) detections: u64,
    /// Lookups answered from the window's detection cache without running detection.
    pub(crate) cache_hits: u64,
    /// Rows detection read, a repeated read of one row counting each time.
    pub(crate) rows_read: u64,
    /// Openability probes hover submitted to the probe worker.
    pub(crate) probes_submitted: u64,
    /// Rows quick select scanned for URLs.
    pub(crate) quick_select_rows_scanned: u64,
}

/// A retired window's counters, held until the dispatch that closed it has been timed.
#[derive(Debug)]
pub(crate) struct ClosingWindow {
    id: winit::window::WindowId,
    counters: Box<WindowFrameCounters>,
    stats: Option<sonicterm_gpu::frame_stats::FrameStats>,
    native: u64,
}

/// An App-level dispatch whose duration is a possible stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchKind {
    /// `about_to_wait`.
    AboutToWait,
    /// `user_event`.
    UserEvent,
    /// `new_events`.
    NewEvents,
}

impl AppFrameCounters {
    /// Builds zeroed counters, as the gate does when it is on; app-module tests enable counting with it.
    pub(super) fn new() -> Self {
        // Both activation paths build the counters here, so the run clock's epoch is set no later than now.
        epoch_in(constructor_epoch_cell());
        Self {
            vt: Arc::new(VtFrameStats::default()),
            dispatch: Arc::new(DispatchTotals::default()),
            custody: Arc::default(),
            fg_worker: Arc::new(super::fg_probe::ForegroundWorkerStats::default()),
            wake_init: 0,
            wake_poll: 0,
            wake_wait_cancelled: 0,
            wake_resume_time: 0,
            wake_user: 0,
            about_to_wait: Histogram::new(HistogramUnit::Millis),
            user_event: Histogram::new(HistogramUnit::Millis),
            new_events: Histogram::new(HistogramUnit::Millis),
            line: LineState::new(Instant::now()),
            closed_windows: CounterRecord::default(),
            windows_registered: 0,
            dispatching: None,
            closing: None,
            s10_watches: std::collections::HashMap::new(),
            next_s10_arming: 1,
            correlation: super::guard_correlation::GuardCorrelation::new(),
            hover: HoverCounters::default(),
            status_line: StatusLineCounters::default(),
            startup: StartupSpans::default(),
        }
    }

    /// Count one `new_events` wake by its cause.
    pub(crate) fn note_wake(&mut self, cause: &winit::event::StartCause) {
        use winit::event::StartCause;
        match cause {
            StartCause::Init => self.wake_init += 1,
            StartCause::Poll => self.wake_poll += 1,
            StartCause::WaitCancelled { .. } => self.wake_wait_cancelled += 1,
            StartCause::ResumeTimeReached { .. } => self.wake_resume_time += 1,
        }
    }

    /// Record one dispatch's duration.
    pub(crate) fn record_dispatch(&mut self, kind: DispatchKind, elapsed_us: u64) {
        let histogram = match kind {
            DispatchKind::AboutToWait => &mut self.about_to_wait,
            DispatchKind::UserEvent => &mut self.user_event,
            DispatchKind::NewEvents => &mut self.new_events,
        };
        histogram.record_us(elapsed_us);
    }

    /// The counters for a new App: on only when the `frame_counters` target admits DEBUG.
    pub(crate) fn from_tracing() -> Option<Self> {
        tracing::enabled!(target: "frame_counters", tracing::Level::DEBUG).then(Self::new)
    }
}

/// `App::force_frame_counters_on` was called after the App created a window or a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCountersTooLate;

impl std::fmt::Display for FrameCountersTooLate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "frame counters can be forced on only before the App creates a window or pane",
        )
    }
}

/// The deferral rule that won in `begin_window_redraw`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeferRule {
    /// A surface timeout within the frame period.
    Timeout,
    /// The window's lock-contention retry floor.
    Contention,
    /// A visible pane's synchronized update (DEC 2026) is still open.
    Sync,
    /// Streaming output paced to the frame period.
    Streaming,
}

/// The first deferral predicate that holds. A later predicate is never evaluated after an earlier
/// one was true, because the sync and streaming checks do Acquire loads and streaming allocates.
pub(crate) fn defer_rule(
    timeout: impl FnOnce() -> bool,
    contention: impl FnOnce() -> bool,
    sync: impl FnOnce() -> bool,
    streaming: impl FnOnce() -> bool,
) -> Option<DeferRule> {
    if timeout() {
        Some(DeferRule::Timeout)
    } else if contention() {
        // When: `contention` holds after no surface timeout, the retry floor wins.
        Some(DeferRule::Contention)
    } else if sync() {
        // When: `sync` holds after timeout and contention did not, a visible open update holds the frame.
        Some(DeferRule::Sync)
    } else {
        // When: neither `timeout` nor `contention` held, so the streaming check runs, and only now.
        streaming().then_some(DeferRule::Streaming)
    }
}

/// One window's frame counters, cumulative for the window's lifetime.
#[derive(Clone, Debug)]
pub(crate) struct WindowFrameCounters {
    /// Redraws that went on to collect a frame.
    pub(crate) attempts: u64,
    /// Frames that presented.
    pub(crate) presented: u64,
    /// Frames re-presented from the cached frame.
    pub(crate) cached: u64,
    /// Attempts that settled without presenting.
    pub(crate) settled: u64,
    /// Attempts the renderer asked to retry.
    pub(crate) retry: u64,
    /// Attempts that hit a surface retry.
    pub(crate) surface_retry: u64,
    /// Attempts that found the device stopped.
    pub(crate) stopped: u64,
    /// Attempts that failed.
    pub(crate) failed: u64,
    /// Frame collections that found a visible parser busy.
    pub(crate) contention_parser: u64,
    /// Frame collections that found a visible image store busy.
    pub(crate) contention_images: u64,
    /// Redraws deferred by a surface timeout.
    pub(crate) defer_timeout: u64,
    /// Redraws deferred by the contention retry floor.
    pub(crate) defer_contention: u64,
    /// Redraws deferred by streaming pacing.
    /// Frames held because a visible pane's synchronized update was open.
    pub(crate) defer_sync: u64,
    pub(crate) defer_streaming: u64,
    /// Attempts whose completion kept the streaming clock: hardware input attempts that settled.
    pub(crate) stream_clock_exempt: u64,
    /// Display-link ticks the tick handler accepted; stale ticks are not counted.
    pub(crate) display_link_ticks: u64,
    /// `Link`-mode streaming frames admitted by a tick.
    pub(crate) display_link_admissions: u64,
    /// `Link`-mode streaming frames admitted by the fallback ceiling.
    pub(crate) display_link_fallbacks: u64,
    /// Contention retries armed.
    pub(crate) contention_retry_armed: u64,
    /// Pending acknowledgement receipts dropped at a collection: the pane was not held, its parser
    /// differed, or an identity changed. Each costs a later re-assembly, never pixels.
    pub(crate) dirt_ack_dropped: u64,
    /// Intervals between consecutive presented frames.
    pub(crate) present_interval: Histogram,
    /// Output events serviced for the window: `PaneOutput` and `RequestRedraw`.
    pub(crate) user_request_redraw: u64,
    /// `RedrawRequested` events for the window.
    pub(crate) redraw_requested: u64,
    /// `window_event` dispatch durations for the window.
    pub(crate) handler: Histogram,
    /// Oldest pending flush to the first redraw that took it.
    pub(crate) flush_to_redraw: Histogram,
    last_presented: Option<Instant>,
    /// Registration order within the App, for `child-N` labels.
    pub(crate) ordinal: u64,
    /// Whether the window registered as the App's main window.
    pub(crate) main: bool,
    /// The window line's cadence and previous record.
    pub(crate) line: LineState,
    /// `attempts` when the window's last line printed.
    line_attempts: u64,
    /// `flush_to_redraw` observations when the window's last line printed.
    line_flushes: u64,
    /// Records built for the window's lines, so tests can see a refused redraw built none.
    #[cfg(test)]
    pub(crate) records_built: u64,
}

impl Default for WindowFrameCounters {
    fn default() -> Self {
        Self {
            attempts: 0,
            presented: 0,
            cached: 0,
            settled: 0,
            retry: 0,
            surface_retry: 0,
            stopped: 0,
            failed: 0,
            contention_parser: 0,
            contention_images: 0,
            defer_timeout: 0,
            defer_contention: 0,
            defer_sync: 0,
            defer_streaming: 0,
            stream_clock_exempt: 0,
            display_link_ticks: 0,
            display_link_admissions: 0,
            display_link_fallbacks: 0,
            contention_retry_armed: 0,
            dirt_ack_dropped: 0,
            present_interval: Histogram::new(HistogramUnit::Millis),
            user_request_redraw: 0,
            redraw_requested: 0,
            handler: Histogram::new(HistogramUnit::Millis),
            flush_to_redraw: Histogram::new(HistogramUnit::Millis),
            last_presented: None,
            ordinal: 0,
            main: false,
            line: LineState::new(Instant::now()),
            line_attempts: 0,
            line_flushes: 0,
            #[cfg(test)]
            records_built: 0,
        }
    }
}

/// A set of cumulative counts and histograms, the unit lines and snapshots are built from.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct CounterRecord {
    counts: Vec<(&'static str, u64)>,
    histograms: Vec<(&'static str, Histogram)>,
}

/// One histogram of a [`CounterRecord`], exported for the perf harness.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistogramBuckets<'record> {
    /// `"ms"` or `"us"`.
    pub unit: &'static str,
    /// Upper bucket bounds in `unit`.
    pub bounds: &'static [u64],
    /// Events per bucket; one more than `bounds`, the last is overflow.
    pub counts: &'record [u64],
    /// The exact sum of the recorded values, in microseconds.
    pub sum_us: u64,
}

/// One line source's cadence and the record its last line printed.
#[derive(Clone, Debug)]
pub(crate) struct LineState {
    cadence: LineCadence,
    previous: CounterRecord,
    since: Instant,
}

/// Every counter an App holds, read field by field; nothing is reset.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct FrameCountersSnapshot {
    /// App-wide VT statistics and the App-global counters.
    pub app: CounterRecord,
    /// Each live window's counters and its renderer's statistics.
    pub windows: Vec<(winit::window::WindowId, CounterRecord)>,
    /// Every closed window's totals.
    pub closed_windows: CounterRecord,
}

#[cfg(test)]
#[path = "frame_counters_tests.rs"]
mod frame_counters_tests;

#[path = "frame_counters_dispatch.rs"]
mod dispatch;
#[path = "frame_counters_histogram.rs"]
mod histogram;
pub use dispatch::clock_epoch;
use dispatch::{constructor_epoch_cell, micros_between, take_pending_native};
pub(crate) use dispatch::{
    consume_flush, epoch_in, flush_clock_ns, lock_parser, note_native_request,
    note_untargeted_flush, publish_flush,
};
#[path = "frame_counters_record.rs"]
mod record;
use record::log_line;

#[cfg(test)]
use dispatch::current_tally;

#[cfg(test)]
pub(crate) use dispatch::flush_clock_reads;

impl DispatchTotals {
    // Ordering: locks uses Relaxed; it is a statistic, ordering nothing.
    fn absorb(&self, mut tally: DispatchTally) {
        self.wait.add(&tally.wait);
        self.locks.fetch_add(tally.locks, Ordering::Relaxed);
        // The requests join their windows and the emptied storage waits for the next dispatch.
        let mut native = self.native.lock();
        native.fold(&mut tally.native);
        native.spare = std::mem::take(&mut tally.native);
    }
}

impl DispatchTotals {
    /// Storage for a dispatch's requests: the previous dispatch's, or a new one at the first
    /// dispatch and in a nested one, which finds the spare taken.
    fn take_spare(&self) -> Vec<(winit::window::WindowId, u64)> {
        let spare = std::mem::take(&mut self.native.lock().spare);
        if spare.capacity() == 0 {
            // When: no `spare` storage is left, this dispatch allocates the bounded tally once.
            return Vec::with_capacity(NATIVE_TALLY_CAPACITY);
        }
        spare
    }
}

impl DispatchTotals {
    /// Move a full tally's requests into the totals mid-dispatch, keeping its storage.
    fn attribute(&self, entries: &mut Vec<(winit::window::WindowId, u64)>) {
        self.native.lock().fold(entries);
    }
}

impl DispatchTotals {
    /// Give registered window `id` an entry, so its requests are attributed to it.
    pub(crate) fn register_native(&self, id: winit::window::WindowId) {
        let mut native = self.native.lock();
        if !native.registered.iter().any(|entry| entry.0 == id) {
            // the window is new, so it gets an entry starting at zero.
            native.registered.push((id, 0));
        }
    }
}

impl DispatchTotals {
    /// Native redraw requests drained so far for window `id`.
    fn native_requests(&self, id: winit::window::WindowId) -> u64 {
        let native = self.native.lock();
        native.registered.iter().find(|entry| entry.0 == id).map_or(0, |entry| entry.1)
    }
}

impl DispatchTotals {
    /// Requests drained for ids that were not registered.
    fn unregistered_requests(&self) -> u64 {
        self.native.lock().unregistered
    }
}

impl DispatchTotals {
    /// Windows with a drained-request entry.
    #[cfg(test)]
    fn registered_entries(&self) -> usize {
        self.native.lock().registered.len()
    }
}

impl DispatchTotals {
    /// Remove retiring window `id`'s entry, returning its drained requests.
    fn take_native_requests(&self, id: winit::window::WindowId) -> u64 {
        let mut native = self.native.lock();
        let position = native.registered.iter().position(|entry| entry.0 == id);
        position.map_or(0, |index| native.registered.swap_remove(index).1)
    }
}

/// Ask `window` to redraw, counting the request for its window inside a counting dispatch.
/// Every App-side native request goes through here; scheduling is exactly `request_redraw`.
pub(crate) fn request_native_redraw(window: &winit::window::Window) {
    note_native_request(window.id());
    window.request_redraw();
}

/// `lock_parser` with an injectable clock.
// Lock order: parser -> cell; the TALLY cell is borrowed briefly while the parser guard is held.
pub(crate) fn lock_counted<'parser, Value>(
    parser: &'parser Mutex<Value>,
    clock: &mut impl FnMut() -> Instant,
) -> MutexGuard<'parser, Value> {
    if !COUNTING.with(Cell::get) {
        // When: `COUNTING` is false, no counting dispatch is open; no clock is read, nothing written.
        return parser.lock();
    }
    let before_lock = clock();
    let guard = parser.lock();
    let wait_us = micros_between(before_lock, clock());
    TALLY.with(|cell| {
        let mut tally = cell.borrow_mut();
        tally.wait.record_us(wait_us);
        tally.locks += 1;
    });
    guard
}

impl super::App {
    /// A window was removed: print its `final=1` line and move its totals to `closed_windows`.
    /// A window its own running dispatch removed waits for that dispatch's handler time.
    pub(super) fn retire_window_counters(
        &mut self,
        id: winit::window::WindowId,
        window: &mut super::WindowState,
    ) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and there is nothing to retire.
            return;
        };
        if let Some(renderer) = window.renderer.as_mut() {
            // the window is leaving, so its renderer settles its growth episodes before the read.
            renderer.finalize_frame_stats();
        }
        let stats = window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
        let Some(counters) = window.redraw.frame_counters.as_deref_mut() else {
            // When: the window never counted, there is nothing to retire.
            return;
        };
        // Requests already drained and any the closing dispatch took both leave with the window.
        let native = app.dispatch.take_native_requests(id) + take_pending_native(id);
        if app.dispatching == Some(id) {
            // When: `dispatching` is this window, its handler is still running; finalize afterwards.
            app.finish_closing();
            let counters = window.redraw.frame_counters.take().expect("counters checked above");
            app.closing = Some(ClosingWindow { id, counters, stats, native });
            return;
        }
        app.retire(counters, stats, native);
    }
}

impl super::App {
    /// [`Self::emit_frame_lines`] at `now`.
    pub(super) fn emit_frame_lines_at(
        &mut self,
        window: Option<(winit::window::WindowId, bool)>,
        now: Instant,
    ) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and nothing prints.
            return;
        };
        if let Some((id, state)) =
            window.and_then(|(id, _)| self.windows.get_mut(&id).map(|state| (id, state)))
        {
            let renderer = state.renderer.as_ref();
            if let Some(counters) = state.redraw.frame_counters.as_deref_mut() {
                // the event's window counts. A redraw authorizes only after a new attempt or
                // flush, decided from plain counts; the record is built only for a line that
                // will print, so a refused or early frame allocates nothing.
                let redraw = window.is_some_and(|(_, redraw)| redraw);
                let authorized = !redraw || counters.redraw_activity_since_line();
                if authorized && counters.line.ready(now) {
                    let stats = renderer.map(sonicterm_gpu::core::GpuRenderer::frame_stats);
                    let record = counters.record(stats, app.dispatch.native_requests(id));
                    #[cfg(test)]
                    {
                        counters.records_built += 1;
                    }
                    if let Some(line) = counters.line.line(&counters.label(), &record, now, true) {
                        counters.mark_line();
                        log_line(&line);
                    }
                }
            }
        }
        if app.line.ready(now) {
            // The App's record is built at most once a second.
            let record = app.record();
            if let Some(line) = app.line.line("app", &record, now, true) {
                log_line(&line);
            }
        }
    }
}

impl super::App {
    /// At exit, print each source's `final=1` line.
    pub(super) fn finish_frame_lines(&mut self) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and nothing prints.
            return;
        };
        app.finish_closing();
        let now = Instant::now();
        for (id, window) in &mut self.windows {
            if let Some(renderer) = window.renderer.as_mut() {
                // the App is exiting, so each renderer settles its growth episodes before the read.
                renderer.finalize_frame_stats();
            }
            let stats = window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
            if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
                // the window counts, its pending counts get a final line.
                let record = counters.record(stats, app.dispatch.native_requests(*id));
                if let Some(line) = counters.line.final_line(&counters.label(), &record, now) {
                    log_line(&line);
                }
            }
        }
        let record = app.record();
        if let Some(line) = app.line.final_line("app", &record, now) {
            log_line(&line);
        }
    }
}

#[path = "frame_counters_app.rs"]
mod app;
pub(crate) use app::for_each_shown_pane;

impl std::error::Error for FrameCountersTooLate {}
