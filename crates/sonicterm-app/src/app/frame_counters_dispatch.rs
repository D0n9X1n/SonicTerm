//! Gated dispatch accounting, parser lock timing and flush publication.

use super::*;

impl NativeTotals {
    /// Move `entries` into the totals by window, leaving `entries` empty with its capacity.
    pub(super) fn fold(&mut self, entries: &mut Vec<(winit::window::WindowId, u64)>) {
        for (id, count) in entries.drain(..) {
            match self.registered.iter_mut().find(|entry| entry.0 == id) {
                Some(entry) => entry.1 += count,
                None => self.unregistered += count,
            }
        }
    }
}

/// Add `count` requests for window `id` to `native`.
pub(super) fn add_native(
    native: &mut Vec<(winit::window::WindowId, u64)>,
    id: winit::window::WindowId,
    count: u64,
) {
    match native.iter_mut().find(|entry| entry.0 == id) {
        Some(entry) => entry.1 += count,
        None => native.push((id, count)),
    }
}

impl DispatchTally {
    pub(super) const fn new() -> Self {
        Self {
            wait: Histogram::new(HistogramUnit::Micros),
            locks: 0,
            native: Vec::new(),
            sink: None,
        }
    }
}

impl DispatchScope {
    /// Open a dispatch; `sink` is the App's totals when its gate is on, `None` when it is off.
    pub(crate) fn enter(sink: Option<Arc<DispatchTotals>>) -> Self {
        let enclosing_counts = COUNTING.with(Cell::get);
        if sink.is_none() && !enclosing_counts {
            // When: `sink` is None and no `enclosing_counts` dispatch counts, nothing is written.
            return Self { sink, outer: None };
        }
        // A counting dispatch reuses its App's request storage, so a request allocates nothing.
        let tally = DispatchTally {
            native: sink.as_deref().map_or_else(Vec::new, DispatchTotals::take_spare),
            sink: sink.clone(),
            ..DispatchTally::new()
        };
        let outer_tally = TALLY.with(|cell| cell.replace(tally));
        COUNTING.with(|cell| cell.set(sink.is_some()));
        Self { sink, outer: Some((enclosing_counts, outer_tally)) }
    }
}

/// Count one native redraw request for window `id` in the open dispatch; outside a counting
/// dispatch it reads one thread-local flag and writes nothing.
pub(crate) fn note_native_request(id: winit::window::WindowId) {
    if !COUNTING.with(Cell::get) {
        // When: `COUNTING` is false, no counting App is dispatching; nothing is recorded.
        return;
    }
    TALLY.with(|cell| {
        let tally = &mut *cell.borrow_mut();
        let full = tally.native.len() == NATIVE_TALLY_CAPACITY;
        if full && !tally.native.iter().any(|entry| entry.0 == id) {
            // the tally is full and `id` is new: its requests go to the App now, keeping the storage.
            if let Some(sink) = &tally.sink {
                sink.attribute(&mut tally.native);
            }
        }
        add_native(&mut tally.native, id, 1);
    });
}

/// Remove window `id`'s requests from the open dispatch, for a window that dispatch retires.
pub(super) fn take_pending_native(id: winit::window::WindowId) -> u64 {
    if !COUNTING.with(Cell::get) {
        // When: `COUNTING` is false, the open dispatch tallied nothing.
        return 0;
    }
    TALLY.with(|cell| {
        let native = &mut cell.borrow_mut().native;
        let position = native.iter().position(|entry| entry.0 == id);
        position.map_or(0, |index| native.swap_remove(index).1)
    })
}

#[cfg(test)]
pub(super) fn pending_native_capacity() -> usize {
    TALLY.with(|cell| cell.borrow().native.capacity())
}

#[cfg(test)]
pub(super) fn pending_native_requests() -> u64 {
    TALLY.with(|cell| cell.borrow().native.iter().map(|entry| entry.1).sum())
}

/// Lock a pane parser on the event-loop thread, timing the wait inside a counting dispatch.
/// Outside one it is exactly `lock()`.
pub(crate) fn lock_parser<Value>(parser: &Mutex<Value>) -> MutexGuard<'_, Value> {
    lock_counted(parser, &mut Instant::now)
}

/// This thread's undrained `(locks, waits)`.
#[cfg(test)]
pub(super) fn current_tally() -> (u64, u64) {
    TALLY.with(|cell| {
        let tally = cell.borrow();
        (tally.locks, tally.wait.count())
    })
}

/// Whole microseconds from `from` to `to`; zero when `to` is earlier.
pub(super) fn micros_between(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_micros()).unwrap_or(u64::MAX)
}

impl VtFrameStats {
    /// Record one parser section after its guard dropped: wait, parse and hold, its bytes, and the wait's
    /// nanosecond total and section count.
    // A seam for span correlation: this section's lock-acquisition interval would be kept here, per pane.
    // Ordering: parse_bytes, parser_lock_wait_ns and parser_sections use Relaxed; statistics, ordering nothing.
    pub(crate) fn record_section(&self, times: &VtSectionTimes, parsed_bytes: u64) {
        self.parser_lock_wait.record_us(micros_between(times.before_lock, times.locked_at));
        let wait = times.locked_at.saturating_duration_since(times.before_lock);
        let wait_ns = u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX);
        self.parser_lock_wait_ns.fetch_add(wait_ns, Ordering::Relaxed);
        self.parser_sections.fetch_add(1, Ordering::Relaxed);
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

impl PaneFrameCounters {
    /// A pane's handles over its App's VT statistics, with no pending flush, an idle watch and no log.
    pub(crate) fn new(vt: Arc<VtFrameStats>) -> Self {
        Self {
            vt,
            pending_flush: Arc::new(AtomicU64::new(0)),
            echo: Arc::new(super::super::echo_watch::EchoWatch::new()),
            sections: None,
        }
    }
}

/// Test-only: `flush_clock_ns` reads made so far on this thread.
#[cfg(test)]
pub(crate) fn flush_clock_reads() -> u64 {
    FLUSH_CLOCK_READS.with(std::cell::Cell::get)
}

/// The process-wide epoch every frame-counter and guard-correlation nanosecond counts from. The
/// first call sets it; the perf harness calls it before its startup phase so every phase converts.
#[doc(hidden)]
pub fn clock_epoch() -> Instant {
    epoch_in(&CLOCK_EPOCH)
}

/// The epoch held by `cell`, set to now by the first caller; concurrent first callers agree.
pub(crate) fn epoch_in(cell: &OnceLock<Instant>) -> Instant {
    *cell.get_or_init(Instant::now)
}

/// The epoch cell the counters' constructor initializes: the process's, or a test's injected one.
pub(super) fn constructor_epoch_cell() -> &'static OnceLock<Instant> {
    #[cfg(test)]
    if let Some(cell) = CONSTRUCTOR_EPOCH_CELL.with(std::cell::Cell::get) {
        // When: a test injected a cell on this thread, the constructor initializes that one.
        return cell;
    }
    &CLOCK_EPOCH
}

/// Nanoseconds since a process-wide epoch, never 0, which a flush slot reserves for "none".
pub(crate) fn flush_clock_ns() -> u64 {
    #[cfg(test)]
    FLUSH_CLOCK_READS.with(|reads| reads.set(reads.get() + 1));
    u64::try_from(clock_epoch().elapsed().as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Publish a targeted flush before its redraw request is sent. The oldest pending flush stays;
/// one that finds it still pending is counted as coalesced, and returns true.
// Ordering: slot's Release exchange precedes send_event; consume_flush's Acquire swap pairs with it.
// Its failure load, flushes and flushes_coalesced are Relaxed.
pub(crate) fn publish_flush(slot: &AtomicU64, now_ns: u64, stats: &VtFrameStats) -> bool {
    stats.flushes.fetch_add(1, Ordering::Relaxed);
    let stored = slot.compare_exchange(0, now_ns.max(1), Ordering::Release, Ordering::Relaxed);
    let coalesced = stored.is_err();
    if coalesced {
        // an earlier flush is still pending, it keeps its time and this one only counts.
        stats.flushes_coalesced.fetch_add(1, Ordering::Relaxed);
    }
    coalesced
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

#[cfg(test)]
#[path = "frame_counters_dispatch_tests.rs"]
mod frame_counters_dispatch_tests;
