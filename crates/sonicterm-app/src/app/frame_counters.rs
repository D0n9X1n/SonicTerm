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

impl HistogramUnit {
    fn bounds(self) -> &'static [u64] {
        match self {
            Self::Millis => &MILLIS_BOUNDS,
            Self::Micros => &MICROS_BOUNDS,
        }
    }

    fn micros_per_unit(self) -> u64 {
        match self {
            Self::Millis => 1_000,
            Self::Micros => 1,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Millis => "ms",
            Self::Micros => "us",
        }
    }

    /// The bucket a value falls in; a value equal to a bound belongs to that bound's bucket.
    fn bucket(self, value_us: u64) -> usize {
        let per_unit = self.micros_per_unit();
        let bounds = self.bounds();
        bounds.iter().position(|bound| value_us <= bound * per_unit).unwrap_or(bounds.len())
    }

    fn bound_of(self, bucket: usize) -> Bound {
        let bounds = self.bounds();
        match bounds.get(bucket) {
            Some(bound) => Bound::AtMost(*bound),
            None => Bound::Above(bounds[bounds.len() - 1]),
        }
    }
}

/// Where a maximum or p95 lies: at or below a bucket bound, or in the overflow bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// At or below this bound, in the histogram's unit.
    AtMost(u64),
    /// Above the largest bound, in the histogram's unit.
    Above(u64),
}

impl Bound {
    /// `le_<unit>=N` or `gt_<unit>=N`, the field form a line prints.
    fn field(self, suffix: &str) -> String {
        match self {
            Self::AtMost(bound) => format!("le_{suffix}={bound}"),
            Self::Above(bound) => format!("gt_{suffix}={bound}"),
        }
    }
}

/// A cumulative bucket histogram and the exact sum of its recorded microseconds.
#[derive(Clone, Debug)]
pub(crate) struct Histogram {
    unit: HistogramUnit,
    buckets: [u64; BUCKETS],
    sum_us: u64,
}

impl Histogram {
    /// An empty histogram with `unit` buckets.
    pub(crate) const fn new(unit: HistogramUnit) -> Self {
        Self { unit, buckets: [0; BUCKETS], sum_us: 0 }
    }

    /// Record one duration, in microseconds.
    pub(crate) fn record_us(&mut self, value_us: u64) {
        self.buckets[self.unit.bucket(value_us)] += 1;
        self.sum_us = self.sum_us.saturating_add(value_us);
    }

    /// Recorded durations.
    pub(crate) fn count(&self) -> u64 {
        self.buckets.iter().sum()
    }

    /// The exact sum of the recorded microseconds.
    pub(crate) fn sum_us(&self) -> u64 {
        self.sum_us
    }

    /// The bound of the highest bucket holding a value; `None` when empty.
    pub(crate) fn max(&self) -> Option<Bound> {
        self.buckets.iter().rposition(|count| *count > 0).map(|bucket| self.unit.bound_of(bucket))
    }

    /// The bound of the bucket holding the 95th percentile; `None` when empty.
    pub(crate) fn p95(&self) -> Option<Bound> {
        let rank = self.count().saturating_mul(95).div_ceil(100);
        if rank == 0 {
            // When: `rank` is 0, the histogram is empty, so it has no p95.
            return None;
        }
        let mut cumulative = 0;
        self.buckets
            .iter()
            .position(|count| {
                cumulative += count;
                cumulative >= rank
            })
            .map(|bucket| self.unit.bound_of(bucket))
    }

    /// The histogram's fields on a line: buckets, sum, p95 and max bounds; empty when no events.
    pub(crate) fn line_fields(&self, name: &str) -> String {
        let (Some(p95), Some(max)) = (self.p95(), self.max()) else {
            // When: `p95` or `max` is None, the histogram is empty and contributes no fields.
            return String::new();
        };
        let suffix = self.unit.suffix();
        let used = &self.buckets[..=self.unit.bounds().len()];
        let buckets: Vec<String> = used.iter().map(ToString::to_string).collect();
        format!(
            "{name}_{suffix}=[{}] {name}_sum_us={} {name}_p95_{} {name}_max_{}",
            buckets.join(","),
            self.sum_us,
            p95.field(suffix),
            max.field(suffix)
        )
    }

    /// Add `other`'s counts, as a closed window's totals join `closed_windows`.
    fn add(&mut self, other: &Self) {
        for (slot, count) in self.buckets.iter_mut().zip(other.buckets) {
            *slot += count;
        }
        self.sum_us += other.sum_us;
    }

    /// What was recorded since `earlier`, a snapshot of this same cumulative histogram.
    pub(crate) fn delta_since(&self, earlier: &Self) -> Self {
        let mut buckets = [0; BUCKETS];
        for (slot, (later, before)) in
            buckets.iter_mut().zip(self.buckets.iter().zip(&earlier.buckets))
        {
            *slot = later.saturating_sub(*before);
        }
        Self { unit: self.unit, buckets, sum_us: self.sum_us.saturating_sub(earlier.sum_us) }
    }
}

/// A histogram that several threads record into.
#[derive(Debug)]
pub(crate) struct AtomicHistogram {
    unit: HistogramUnit,
    buckets: [AtomicU64; BUCKETS],
    sum_us: AtomicU64,
}

impl AtomicHistogram {
    /// An empty shared histogram with `unit` buckets.
    pub(crate) fn new(unit: HistogramUnit) -> Self {
        Self {
            unit,
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
        }
    }

    /// Record one duration, in microseconds.
    // Ordering: buckets and sum_us use Relaxed; they are statistics and order nothing.
    pub(crate) fn record_us(&self, value_us: u64) {
        self.buckets[self.unit.bucket(value_us)].fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(value_us, Ordering::Relaxed);
    }

    /// Add a thread-local histogram's counts, as a dispatch scope drains its tally.
    // Ordering: shared buckets and sum_us use Relaxed; they are statistics and order nothing.
    fn add(&self, local: &Histogram) {
        for (shared, count) in self.buckets.iter().zip(local.buckets) {
            shared.fetch_add(count, Ordering::Relaxed);
        }
        self.sum_us.fetch_add(local.sum_us, Ordering::Relaxed);
    }

    /// The cumulative counts, read field by field; a concurrent record may land in either read.
    // Ordering: buckets and sum_us load Relaxed; a snapshot is observational and orders nothing.
    pub(crate) fn snapshot(&self) -> Histogram {
        Histogram {
            unit: self.unit,
            buckets: std::array::from_fn(|bucket| self.buckets[bucket].load(Ordering::Relaxed)),
            sum_us: self.sum_us.load(Ordering::Relaxed),
        }
    }
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
    /// Foreground-process probes made.
    pub(crate) probe_calls: AtomicU64,
    /// Panes those probes covered.
    pub(crate) probe_panes: AtomicU64,
    /// Each probe's duration.
    pub(crate) probe: AtomicHistogram,
}

impl Default for DispatchTotals {
    fn default() -> Self {
        Self {
            wait: AtomicHistogram::new(HistogramUnit::Micros),
            locks: AtomicU64::new(0),
            probe_calls: AtomicU64::new(0),
            probe_panes: AtomicU64::new(0),
            probe: AtomicHistogram::new(HistogramUnit::Micros),
        }
    }
}

impl DispatchTotals {
    // Ordering: locks, probe_calls and probe_panes use Relaxed; they are statistics, ordering nothing.
    fn absorb(&self, tally: &DispatchTally) {
        self.wait.add(&tally.wait);
        self.locks.fetch_add(tally.locks, Ordering::Relaxed);
        self.probe_calls.fetch_add(tally.probe_calls, Ordering::Relaxed);
        self.probe_panes.fetch_add(tally.probe_panes, Ordering::Relaxed);
        self.probe.add(&tally.probe);
    }
}

/// Plain counters a dispatch accumulates on the event-loop thread.
#[derive(Debug)]
struct DispatchTally {
    wait: Histogram,
    locks: u64,
    probe_calls: u64,
    probe_panes: u64,
    probe: Histogram,
}

impl DispatchTally {
    const fn new() -> Self {
        Self {
            wait: Histogram::new(HistogramUnit::Micros),
            locks: 0,
            probe_calls: 0,
            probe_panes: 0,
            probe: Histogram::new(HistogramUnit::Micros),
        }
    }
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

impl DispatchScope {
    /// Open a dispatch; `sink` is the App's totals when its gate is on, `None` when it is off.
    pub(crate) fn enter(sink: Option<Arc<DispatchTotals>>) -> Self {
        let enclosing_counts = COUNTING.with(Cell::get);
        if sink.is_none() && !enclosing_counts {
            // When: `sink` is None and no `enclosing_counts` dispatch counts, nothing is written.
            return Self { sink, outer: None };
        }
        let outer_tally = TALLY.with(|cell| cell.replace(DispatchTally::new()));
        COUNTING.with(|cell| cell.set(sink.is_some()));
        Self { sink, outer: Some((enclosing_counts, outer_tally)) }
    }
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
            sink.absorb(&tally);
        }
    }
}

/// Lock a pane parser on the event-loop thread, timing the wait inside a counting dispatch.
/// Outside one it is exactly `lock()`.
pub(crate) fn lock_parser<Value>(parser: &Mutex<Value>) -> MutexGuard<'_, Value> {
    lock_counted(parser, &mut Instant::now)
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

/// This thread's undrained `(locks, waits)`.
/// Run a foreground-process probe covering `panes`, timed once inside a counting dispatch.
/// An empty batch makes no snapshot, so it is neither timed nor counted.
pub(crate) fn time_probe<Output>(panes: usize, probe: impl FnOnce() -> Output) -> Output {
    time_probe_with(panes, &mut Instant::now, probe)
}

/// `time_probe` with an injectable clock.
pub(crate) fn time_probe_with<Output>(
    panes: usize,
    clock: &mut impl FnMut() -> Instant,
    probe: impl FnOnce() -> Output,
) -> Output {
    if panes == 0 || !COUNTING.with(Cell::get) {
        // When: `panes` is 0 (no snapshot) or `COUNTING` is false, nothing is timed or counted.
        return probe();
    }
    let before_probe = clock();
    let output = probe();
    let probe_us = micros_between(before_probe, clock());
    TALLY.with(|cell| {
        let mut tally = cell.borrow_mut();
        tally.probe_calls += 1;
        tally.probe_panes += panes as u64;
        tally.probe.record_us(probe_us);
    });
    output
}

#[cfg(test)]
fn current_tally() -> (u64, u64) {
    TALLY.with(|cell| {
        let tally = cell.borrow();
        (tally.locks, tally.wait.count())
    })
}

/// Whole microseconds from `from` to `to`; zero when `to` is earlier.
fn micros_between(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_micros()).unwrap_or(u64::MAX)
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
        }
    }
}

impl VtFrameStats {
    /// Record one parser section after its guard dropped: wait, parse and hold, and its bytes.
    // Ordering: parse_bytes uses Relaxed; the counts are statistics and order nothing.
    pub(crate) fn record_section(&self, times: &VtSectionTimes, parsed_bytes: u64) {
        self.parser_lock_wait.record_us(micros_between(times.before_lock, times.locked_at));
        self.parse.record_us(micros_between(times.locked_at, times.parsed_at));
        self.parser_lock_hold.record_us(micros_between(times.locked_at, times.released_at));
        self.parse_bytes.fetch_add(parsed_bytes, Ordering::Relaxed);
    }

    /// Count one output batch; a batch may take the lock several times.
    // Ordering: batches uses Relaxed; the count is a statistic and orders nothing.
    pub(crate) fn note_batch(&self) {
        self.batches.fetch_add(1, Ordering::Relaxed);
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
}

impl PaneFrameCounters {
    /// A pane's handles over its App's VT statistics, with no pending flush.
    pub(crate) fn new(vt: Arc<VtFrameStats>) -> Self {
        Self { vt, pending_flush: Arc::new(AtomicU64::new(0)) }
    }
}

/// Nanoseconds since a process-wide epoch, never 0, which a flush slot reserves for "none".
pub(crate) fn flush_clock_ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Publish a targeted flush before its redraw request is sent. The oldest pending flush stays;
/// one that finds it still pending is counted as coalesced.
// Ordering: slot's Release exchange precedes send_event; consume_flush's Acquire swap pairs with it.
// Its failure load, flushes and flushes_coalesced are Relaxed.
pub(crate) fn publish_flush(slot: &AtomicU64, now_ns: u64, stats: &VtFrameStats) {
    stats.flushes.fetch_add(1, Ordering::Relaxed);
    let stored = slot.compare_exchange(0, now_ns.max(1), Ordering::Release, Ordering::Relaxed);
    if stored.is_err() {
        // an earlier flush is still pending, it keeps its time and this one only counts.
        stats.flushes_coalesced.fetch_add(1, Ordering::Relaxed);
    }
}

/// Count a flush while the pane has no redraw target; no timestamp is stored.
// Ordering: flushes and flushes_untargeted use Relaxed; they are statistics and order nothing.
pub(crate) fn note_untargeted_flush(stats: &VtFrameStats) {
    stats.flushes.fetch_add(1, Ordering::Relaxed);
    stats.flushes_untargeted.fetch_add(1, Ordering::Relaxed);
}

/// Take the pane's pending flush at a redraw: its age in nanoseconds, or `None` when none was
/// pending. A flush published during the swap is taken now or left for the next redraw.
// Ordering: slot swaps Acquire, pairing with publish_flush's Release, so the time read is the one stored.
pub(crate) fn consume_flush(slot: &AtomicU64, now_ns: u64) -> Option<u64> {
    let flushed_at_ns = slot.swap(0, Ordering::Acquire);
    (flushed_at_ns != 0).then(|| now_ns.saturating_sub(flushed_at_ns))
}

/// The frame and lock counters one App owns, present only when its gate is on.
#[derive(Debug)]
pub(crate) struct AppFrameCounters {
    /// VT statistics every pane's worker records into; fixed in size for the App's lifetime.
    pub(crate) vt: Arc<VtFrameStats>,
    /// Event-loop-thread parser waits, drained at each dispatch end.
    pub(crate) dispatch: Arc<DispatchTotals>,
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
    fn new() -> Self {
        Self {
            vt: Arc::new(VtFrameStats::default()),
            dispatch: Arc::new(DispatchTotals::default()),
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

impl std::error::Error for FrameCountersTooLate {}

impl super::App {
    /// Turn this App's frame counters on whatever the log filter says. Only an App that has
    /// created no window, pane or renderer accepts it, so no reader ever sees the gate change.
    ///
    /// # Errors
    ///
    /// Returns [`FrameCountersTooLate`] once the App has created a window or a pane.
    #[doc(hidden)]
    pub fn force_frame_counters_on(&mut self) -> Result<(), FrameCountersTooLate> {
        if *self.frame_counters_sealed.get_mut() {
            // When: `frame_counters_sealed` is set, a window or pane exists and may have read the gate.
            return Err(FrameCountersTooLate);
        }
        self.frame_counters.get_or_insert_with(AppFrameCounters::new);
        Ok(())
    }

    /// The counter handles for a pane being created; `None` when the gate is off. The first
    /// pane seals the gate.
    /// The start of a timed dispatch; `None`, with no clock read, when the gate is off.
    pub(super) fn frame_clock_start(&self) -> Option<Instant> {
        self.frame_counters.as_ref().map(|_| Instant::now())
    }

    /// Count a `new_events` wake by its cause.
    pub(super) fn note_frame_wake(&mut self, cause: &winit::event::StartCause) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // the gate is on, every wake counts; none authorizes a line by itself.
            counters.note_wake(cause);
        }
    }

    /// Count a `user_event` wake.
    pub(super) fn note_frame_user_wake(&mut self) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // the gate is on, every user event counts.
            counters.wake_user += 1;
        }
    }

    /// Record a dispatch started at `started`, when the gate is on.
    pub(super) fn note_frame_dispatch(&mut self, kind: DispatchKind, started: Option<Instant>) {
        if let (Some(started), Some(counters)) = (started, self.frame_counters.as_mut()) {
            // the gate is on, the dispatch's duration is recorded.
            counters.record_dispatch(kind, micros_between(started, Instant::now()));
        }
    }

    /// A `RedrawRequested` for window `id`: count it and take every pending flush of its panes.
    /// Every pane the window owns is taken, hidden tabs included, because their flushes target
    /// this window too and walking the tab's leaves would allocate per frame.
    pub(super) fn note_redraw_requested(&mut self, id: winit::window::WindowId) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window is gone, there is nothing to count.
            return;
        };
        let panes = &window.panes;
        let Some(counters) = window.redraw.frame_counters.as_deref_mut() else {
            // When: the window's `frame_counters` is None, the gate is off; no flush atomic is touched.
            return;
        };
        let slots = panes.values().filter_map(|pane| pane.frame_counters.as_ref());
        counters.note_redraw(slots.map(|pane| pane.pending_flush.as_ref()), flush_clock_ns());
    }

    /// Record a `window_event` dispatch for window `id` started at `started`.
    pub(super) fn note_window_handler(&mut self, id: winit::window::WindowId, started: Instant) {
        let window = self.windows.get_mut(&id);
        if let Some(counters) =
            window.and_then(|window| window.redraw.frame_counters.as_deref_mut())
        {
            // the window still exists and counts, its handler time is recorded.
            counters.note_handler(micros_between(started, Instant::now()));
        }
    }

    /// Count a `UserEvent::RequestRedraw` for window `id`.
    pub(super) fn note_user_request_redraw(&mut self, id: winit::window::WindowId) {
        let window = self.windows.get_mut(&id);
        if let Some(counters) =
            window.and_then(|window| window.redraw.frame_counters.as_deref_mut())
        {
            // the window counts, the output's redraw request is recorded.
            counters.note_user_request();
        }
    }

    /// Open one dispatch on the event-loop thread for this App.
    pub(super) fn frame_dispatch_scope(&self) -> DispatchScope {
        DispatchScope::enter(
            self.frame_counters.as_ref().map(|counters| Arc::clone(&counters.dispatch)),
        )
    }

    // Ordering: frame_counters_sealed stores Relaxed; only force_frame_counters_on reads it, via &mut self.
    pub(super) fn pane_frame_counters(&self) -> Option<PaneFrameCounters> {
        self.frame_counters_sealed.store(true, Ordering::Relaxed);
        self.frame_counters
            .as_ref()
            .map(|counters| PaneFrameCounters::new(Arc::clone(&counters.vt)))
    }
}

/// The deferral rule that won in `begin_window_redraw`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeferRule {
    /// A surface timeout within the frame period.
    Timeout,
    /// The window's lock-contention retry floor.
    Contention,
    /// Streaming output paced to the frame period.
    Streaming,
}

/// The first deferral predicate that holds. A later predicate is never evaluated after an earlier
/// one was true, because the streaming check allocates and does Acquire loads.
pub(crate) fn defer_rule(
    timeout: impl FnOnce() -> bool,
    contention: impl FnOnce() -> bool,
    streaming: impl FnOnce() -> bool,
) -> Option<DeferRule> {
    if timeout() {
        Some(DeferRule::Timeout)
    } else if contention() {
        // When: `contention` holds after no surface timeout, the retry floor wins.
        Some(DeferRule::Contention)
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
    pub(crate) defer_streaming: u64,
    /// Contention retries armed.
    pub(crate) contention_retry_armed: u64,
    /// Intervals between consecutive presented frames.
    pub(crate) present_interval: Histogram,
    /// Native redraw requests the redraw scheduler issued.
    pub(crate) native_request_redraw: u64,
    /// `UserEvent::RequestRedraw` events for the window.
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
            defer_streaming: 0,
            contention_retry_armed: 0,
            present_interval: Histogram::new(HistogramUnit::Millis),
            native_request_redraw: 0,
            user_request_redraw: 0,
            redraw_requested: 0,
            handler: Histogram::new(HistogramUnit::Millis),
            flush_to_redraw: Histogram::new(HistogramUnit::Millis),
            last_presented: None,
            ordinal: 0,
            main: false,
            line: LineState::new(Instant::now()),
        }
    }
}

impl WindowFrameCounters {
    /// Count one settled attempt; a presented frame also records the interval since the last one.
    pub(super) fn record_settlement(
        &mut self,
        outcome: super::redraw::FrameSettlement,
        now: Instant,
    ) {
        use super::redraw::FrameSettlement;
        match outcome {
            FrameSettlement::Presented => {
                self.presented += 1;
                if let Some(previous) = self.last_presented {
                    // an earlier frame presented, the interval between the two is recorded.
                    self.present_interval.record_us(micros_between(previous, now));
                }
                self.last_presented = Some(now);
            }
            FrameSettlement::Cached => self.cached += 1,
            FrameSettlement::Settled => self.settled += 1,
            FrameSettlement::Retry(_) => self.retry += 1,
            FrameSettlement::SurfaceRetry(_) => self.surface_retry += 1,
            FrameSettlement::Stopped(_) => self.stopped += 1,
            FrameSettlement::Failed => self.failed += 1,
        }
    }

    /// Count a `RedrawRequested` and take each pane's pending flush once.
    pub(crate) fn note_redraw<'slot>(
        &mut self,
        slots: impl Iterator<Item = &'slot AtomicU64>,
        now_ns: u64,
    ) {
        self.redraw_requested += 1;
        for slot in slots {
            if let Some(age_ns) = consume_flush(slot, now_ns) {
                // the pane had a pending flush, its age to this redraw is one observation.
                self.flush_to_redraw.record_us(age_ns / 1_000);
            }
        }
    }

    /// Record one `window_event` dispatch's duration.
    pub(crate) fn note_handler(&mut self, elapsed_us: u64) {
        self.handler.record_us(elapsed_us);
    }

    /// Count a `UserEvent::RequestRedraw` for the window.
    pub(crate) fn note_user_request(&mut self) {
        self.user_request_redraw += 1;
    }

    /// Count a native redraw request the scheduler issued.
    pub(crate) fn note_native_request(&mut self) {
        self.native_request_redraw += 1;
    }

    /// Count the rule that deferred a redraw.
    pub(crate) fn note_defer(&mut self, rule: DeferRule) {
        match rule {
            DeferRule::Timeout => self.defer_timeout += 1,
            DeferRule::Contention => self.defer_contention += 1,
            DeferRule::Streaming => self.defer_streaming += 1,
        }
    }

    /// Count a frame collection that found a visible lock busy.
    pub(crate) fn note_contention(&mut self, images: bool) {
        if images {
            self.contention_images += 1;
        } else {
            // When: `images` is false, the busy lock was a visible parser, not an image store.
            self.contention_parser += 1;
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

impl CounterRecord {
    /// The count named `name`, if the record has it.
    pub fn count(&self, name: &str) -> Option<u64> {
        self.counts.iter().find(|(field, _)| *field == name).map(|(_, value)| *value)
    }

    /// Events in the histogram named `name`, if the record has it.
    pub fn histogram_count(&self, name: &str) -> Option<u64> {
        self.histogram(name).map(Histogram::count)
    }

    /// The exact microsecond sum of the histogram named `name`, if the record has it.
    pub fn histogram_sum_us(&self, name: &str) -> Option<u64> {
        self.histogram(name).map(Histogram::sum_us)
    }

    fn histogram(&self, name: &str) -> Option<&Histogram> {
        self.histograms.iter().find(|(field, _)| *field == name).map(|(_, histogram)| histogram)
    }

    /// Append a count.
    pub(crate) fn push_count(&mut self, name: &'static str, value: u64) {
        self.counts.push((name, value));
    }

    /// Append a histogram.
    pub(crate) fn push_histogram(&mut self, name: &'static str, histogram: Histogram) {
        self.histograms.push((name, histogram));
    }

    /// Add `other`'s counts and histograms, matching fields by name.
    pub(crate) fn merge(&mut self, other: &Self) {
        for (name, value) in &other.counts {
            match self.counts.iter_mut().find(|entry| entry.0 == *name) {
                Some(entry) => entry.1 += value,
                None => self.counts.push((name, *value)),
            }
        }
        for (name, histogram) in &other.histograms {
            match self.histograms.iter_mut().find(|entry| entry.0 == *name) {
                Some(entry) => entry.1.add(histogram),
                None => self.histograms.push((name, histogram.clone())),
            }
        }
    }

    /// What grew since `earlier`, an older record of the same source.
    pub(crate) fn delta_since(&self, earlier: &Self) -> Self {
        let counts = self
            .counts
            .iter()
            .map(|(name, value)| (*name, value.saturating_sub(earlier.count(name).unwrap_or(0))))
            .collect();
        let histograms = self
            .histograms
            .iter()
            .map(|(name, histogram)| {
                let delta = earlier
                    .histogram(name)
                    .map_or_else(|| histogram.clone(), |before| histogram.delta_since(before));
                (*name, delta)
            })
            .collect();
        Self { counts, histograms }
    }

    /// Whether any of `names` grew since `earlier`.
    pub(crate) fn any_grew(&self, earlier: &Self, names: &[&str]) -> bool {
        names.iter().any(|name| {
            self.count(name) > earlier.count(name)
                || self.histogram_count(name) > earlier.histogram_count(name)
        })
    }

    /// The record's nonzero fields on a line.
    fn line_fields(&self) -> String {
        let counts = self
            .counts
            .iter()
            .filter(|(_, value)| *value > 0)
            .map(|(name, value)| format!("{name}={value}"));
        let histograms =
            self.histograms.iter().map(|(name, histogram)| histogram.line_fields(name));
        counts.chain(histograms).filter(|field| !field.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

/// One line source's cadence and the record its last line printed.
#[derive(Clone, Debug)]
pub(crate) struct LineState {
    cadence: LineCadence,
    previous: CounterRecord,
    since: Instant,
}

impl LineState {
    /// A source that opened at `now`.
    pub(crate) fn new(now: Instant) -> Self {
        Self { cadence: LineCadence::default(), previous: CounterRecord::default(), since: now }
    }

    /// Whether a line could print at `now`; nothing is allocated to find out.
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.cadence.ready(now)
    }

    /// The record the last line printed.
    pub(crate) fn previous(&self) -> &CounterRecord {
        &self.previous
    }

    /// The line for `current` at `now`, at most one a second and only when `authorized`.
    pub(crate) fn line(
        &mut self,
        label: &str,
        current: &CounterRecord,
        now: Instant,
        authorized: bool,
    ) -> Option<String> {
        if !self.cadence.line_due(now, authorized) {
            // When: `line_due` refuses, the source printed within a second, only maintains, or is closed.
            return None;
        }
        Some(self.render(label, "", current, now))
    }

    /// The `final=1` line at close or exit, when counts are pending; nothing prints after it.
    pub(crate) fn final_line(
        &mut self,
        label: &str,
        current: &CounterRecord,
        now: Instant,
    ) -> Option<String> {
        let pending = !current.delta_since(&self.previous).line_fields().is_empty();
        if !self.cadence.final_line(pending) {
            // When: nothing is pending, or the final line already printed, the source just closes.
            return None;
        }
        Some(self.render(label, " final=1", current, now))
    }

    fn render(
        &mut self,
        label: &str,
        marker: &str,
        current: &CounterRecord,
        now: Instant,
    ) -> String {
        let delta = current.delta_since(&self.previous);
        let span_ms = now.saturating_duration_since(self.since).as_millis();
        self.previous = current.clone();
        self.since = now;
        format!("window={label}{marker} span_ms={span_ms} {}", delta.line_fields())
            .trim_end()
            .to_owned()
    }
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

/// Print one counter line.
fn log_line(line: &str) {
    tracing::debug!(target: "frame_counters", "[frame_counters] {line}");
}

impl AppFrameCounters {
    /// Counters for a window registering now.
    pub(crate) fn window_counters(&mut self, main: bool) -> Box<WindowFrameCounters> {
        self.windows_registered += 1;
        Box::new(WindowFrameCounters {
            ordinal: self.windows_registered,
            main,
            ..WindowFrameCounters::default()
        })
    }

    /// The App's cumulative counts: VT statistics, dispatch waits and probes, wakes and stalls.
    // Ordering: each value loads Relaxed; a record is observational and orders nothing.
    pub(crate) fn record(&self) -> CounterRecord {
        let mut record = CounterRecord::default();
        let (vt, dispatch) = (&self.vt, &self.dispatch);
        for (name, value) in [
            ("parse_bytes", &vt.parse_bytes),
            ("batches", &vt.batches),
            ("flushes", &vt.flushes),
            ("flushes_untargeted", &vt.flushes_untargeted),
            ("flushes_coalesced", &vt.flushes_coalesced),
            ("ui_parser_locks", &dispatch.locks),
            ("fg_probe_calls", &dispatch.probe_calls),
            ("fg_probe_panes", &dispatch.probe_panes),
        ] {
            record.push_count(name, value.load(Ordering::Relaxed));
        }
        for (name, value) in [
            ("wake_init", self.wake_init),
            ("wake_poll", self.wake_poll),
            ("wake_wait_cancelled", self.wake_wait_cancelled),
            ("wake_resume_time", self.wake_resume_time),
            ("wake_user", self.wake_user),
        ] {
            record.push_count(name, value);
        }
        for (name, histogram) in [
            ("parser_lock_wait", vt.parser_lock_wait.snapshot()),
            ("parser_lock_hold", vt.parser_lock_hold.snapshot()),
            ("parse", vt.parse.snapshot()),
            ("ui_parser_wait", dispatch.wait.snapshot()),
            ("fg_probe", dispatch.probe.snapshot()),
            ("about_to_wait", self.about_to_wait.clone()),
            ("user_event", self.user_event.clone()),
            ("new_events", self.new_events.clone()),
        ] {
            record.push_histogram(name, histogram);
        }
        record
    }
}

impl WindowFrameCounters {
    /// The window's line label: `main`, or `child-N` by registration order.
    pub(crate) fn label(&self) -> String {
        if self.main {
            "main".to_owned()
        } else {
            // When: `main` is false, the window is numbered by registration order.
            format!("child-{}", self.ordinal)
        }
    }

    /// The window's cumulative counts, with its renderer's statistics when it has a renderer.
    pub(crate) fn record(
        &self,
        renderer: Option<sonicterm_gpu::frame_stats::FrameStats>,
    ) -> CounterRecord {
        let mut record = CounterRecord::default();
        for (name, value) in [
            ("attempts", self.attempts),
            ("presented", self.presented),
            ("cached", self.cached),
            ("settled", self.settled),
            ("retry", self.retry),
            ("surface_retry", self.surface_retry),
            ("stopped", self.stopped),
            ("failed", self.failed),
            ("contention_parser", self.contention_parser),
            ("contention_images", self.contention_images),
            ("defer_timeout", self.defer_timeout),
            ("defer_contention", self.defer_contention),
            ("defer_streaming", self.defer_streaming),
            ("contention_retry_armed", self.contention_retry_armed),
            ("native_request_redraw", self.native_request_redraw),
            ("user_request_redraw", self.user_request_redraw),
            ("redraw_requested", self.redraw_requested),
        ] {
            record.push_count(name, value);
        }
        for (name, histogram) in [
            ("present_interval", &self.present_interval),
            ("handler", &self.handler),
            ("flush_to_redraw", &self.flush_to_redraw),
        ] {
            record.push_histogram(name, histogram.clone());
        }
        if let Some(stats) = renderer {
            // the window has a renderer, its statistics join the window's record.
            for (name, value) in [
                ("vertex_bytes", stats.vertex_bytes),
                ("index_bytes", stats.index_bytes),
                ("damage_permille_sum", stats.damage_permille_sum),
                ("damaged_frames", stats.damaged_frames),
                ("software_frames", stats.software_frames),
                ("gpu_frames", stats.gpu_frames),
                ("row_cache_hits", stats.row_cache_hits),
                ("row_cache_misses", stats.row_cache_misses),
                ("shape_requests", stats.shape_requests),
            ] {
                record.push_count(name, value);
            }
        }
        record
    }
}

impl super::App {
    /// This App's counters, read field by field; `None` when its gate is off. Nothing is reset.
    #[doc(hidden)]
    pub fn frame_counters_snapshot(&self) -> Option<FrameCountersSnapshot> {
        let app = self.frame_counters.as_ref()?;
        let windows = self
            .windows
            .iter()
            .filter_map(|(id, window)| {
                let counters = window.redraw.frame_counters.as_deref()?;
                let stats =
                    window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
                Some((*id, counters.record(stats)))
            })
            .collect();
        Some(FrameCountersSnapshot {
            app: app.record(),
            windows,
            closed_windows: app.closed_windows.clone(),
        })
    }

    /// A window was removed: print its `final=1` line and move its totals to `closed_windows`.
    pub(super) fn retire_window_counters(&mut self, window: &mut super::WindowState) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and there is nothing to retire.
            return;
        };
        let stats = window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
        let Some(counters) = window.redraw.frame_counters.as_deref_mut() else {
            // When: the window never counted, there is nothing to retire.
            return;
        };
        let record = counters.record(stats);
        if let Some(line) = counters.line.final_line(&counters.label(), &record, Instant::now()) {
            log_line(&line);
        }
        app.closed_windows.merge(&record);
    }

    /// Print the lines a `window_event` or `user_event` authorizes. `window` names the window
    /// and whether the event was its `RedrawRequested`, which authorizes a line only when it
    /// led to a frame attempt or took a flush.
    pub(super) fn emit_frame_lines(&mut self, window: Option<(winit::window::WindowId, bool)>) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off; no clock is read and nothing prints.
            return;
        };
        let now = Instant::now();
        if let Some(state) = window.and_then(|(id, _)| self.windows.get_mut(&id)) {
            let stats = state.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
            if let Some(counters) = state.redraw.frame_counters.as_deref_mut() {
                // the event's window counts; its record is built only when a line could print,
                // so a frame within the second allocates nothing.
                if counters.line.ready(now) {
                    let record = counters.record(stats);
                    let redraw = window.is_some_and(|(_, redraw)| redraw);
                    let authorized = !redraw
                        || record
                            .any_grew(counters.line.previous(), &["attempts", "flush_to_redraw"]);
                    if let Some(line) =
                        counters.line.line(&counters.label(), &record, now, authorized)
                    {
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

    /// At exit, print each source's `final=1` line.
    pub(super) fn finish_frame_lines(&mut self) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and nothing prints.
            return;
        };
        let now = Instant::now();
        for window in self.windows.values_mut() {
            let stats = window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
            if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
                // the window counts, its pending counts get a final line.
                let record = counters.record(stats);
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

#[cfg(test)]
#[path = "frame_counters_tests.rs"]
mod frame_counters_tests;
