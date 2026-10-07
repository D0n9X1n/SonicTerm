//! Raw guard-correlation records for the frame counters.
//!
//! While the counter gate is on, each pane's VT worker records every parser section it is about to lock
//! into a bounded per-pane log, and the event loop records each parser guard a counting frame holds into
//! a bounded span store. Nothing is correlated here: [`super::App::take_guard_correlation_v1`] moves the
//! completed records out, with every loss, refusal and still-pending section, for an offline join.
//!
//! Every buffer has a fixed capacity allocated before recording starts and again before each take, so
//! recording never allocates. A record that does not fit is counted, and a located one adds a loss
//! interval; nothing is overwritten. Every nanosecond counts from the run clock's epoch
//! ([`super::frame_counters::clock_epoch`]); a time that cannot be converted is counted as unlocated.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

/// Published sections one pane log holds between takes.
pub(crate) const SECTION_CAPACITY: usize = 32_768;
/// Located abandonments one pane log holds between takes.
pub(crate) const ABANDONED_CAPACITY: usize = 64;
/// Loss intervals one log or the span store holds; a full list merges its nearest pair.
pub(crate) const LOSS_CAPACITY: usize = 64;
/// UI guard spans the span store holds between takes.
pub(crate) const SPAN_CAPACITY: usize = 16_384;
/// Guards a collection records individually; later ones are summarized by one conservative hull.
pub(crate) const INLINE_GUARDS: usize = 16;

/// One published worker section: the D2.1a reads before `lock()` and after it returned.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionRecordV1 {
    /// The section's identity within its pane, from 1.
    pub section_seq: u64,
    /// Nanoseconds from the run clock's epoch, read before `lock()` was called.
    pub before_lock_ns: u64,
    /// Nanoseconds from the run clock's epoch, read after `lock()` returned.
    pub locked_at_ns: u64,
}

/// One UI parser guard a counting frame held.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanRecordV1 {
    /// The pane whose parser guard was held.
    pub pane_id: u64,
    /// The collection that held it.
    pub collection_seq: u64,
    /// Nanoseconds from the epoch, read after the guard's `try_lock` succeeded.
    pub acquired_ns: u64,
    /// Nanoseconds from the epoch, read once after the collection's last guard was released.
    pub released_ns: u64,
}

/// The one section a pane log has issued and not yet published or abandoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSectionV1 {
    /// The issued identity.
    pub section_seq: u64,
    /// Nanoseconds from the epoch, read under the log mutex at registration; `None` when the read failed.
    pub registered_ns: Option<u64>,
}

/// A section issued and then abandoned without publishing, with both of its times known.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbandonedSectionV1 {
    /// The abandoned identity.
    pub section_seq: u64,
    /// Nanoseconds from the epoch at its registration.
    pub registered_ns: u64,
    /// Nanoseconds from the epoch at its abandonment.
    pub abandoned_ns: u64,
}

/// An interval in which a located record was dropped or summarized; it may hide any number of them.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LossIntervalV1 {
    /// Nanoseconds from the epoch at the interval's start.
    pub from_ns: u64,
    /// Nanoseconds from the epoch at the interval's end.
    pub to_ns: u64,
}

const _: () = assert!(std::mem::size_of::<SectionRecordV1>() == 24);
const _: () = assert!(std::mem::size_of::<SpanRecordV1>() == 32);
const _: () = assert!(std::mem::size_of::<PendingSectionV1>() == 24);
const _: () = assert!(std::mem::size_of::<AbandonedSectionV1>() == 24);
const _: () = assert!(std::mem::size_of::<LossIntervalV1>() == 16);

/// What [`super::App::take_guard_correlation_v1`] found.
#[derive(Debug)]
pub enum GuardCorrelationTakeV1 {
    /// The counter gate is off, so nothing is recorded.
    GateOff,
    /// The take sequence is exhausted; nothing was transferred.
    TakeExhausted,
    /// The transferred records and evidence.
    Taken(GuardCorrelationV1),
}

/// One take: every pane log's records and evidence, and the UI span batch.
#[derive(Debug)]
pub struct GuardCorrelationV1 {
    /// The epoch every nanosecond of this take counts from.
    pub clock_epoch: Instant,
    /// This take's identity, from 1.
    pub take_seq: u64,
    /// Nanoseconds from the epoch when the take started; `None` when the read failed.
    pub taken_at_ns: Option<u64>,
    /// One container per registered pane log, each pane once.
    pub panes: Vec<PaneSectionsV1>,
    /// The UI span batch.
    pub spans: SpanBatchV1,
}

/// One pane log's transfer.
#[derive(Debug)]
pub struct PaneSectionsV1 {
    /// The pane the log belongs to.
    pub pane_id: u64,
    /// The pane's owner has dropped, so the log refuses new sections.
    pub closed: bool,
    /// The section sequence reached its exhausted sentinel; sticky.
    pub identities_exhausted: bool,
    /// The first identity this take's range can hold: issued identities are `[prev, next)`.
    pub prev_next_section_seq: u64,
    /// The next identity the log will issue; `u64::MAX` once exhausted.
    pub next_section_seq: u64,
    /// The previous take's pending identity, which this take accounts for.
    pub carried_pending: Option<u64>,
    /// Published sections, oldest first.
    pub records: Vec<SectionRecordV1>,
    /// The section issued and not yet resolved, which stays pending into the next take.
    pub pending: Option<PendingSectionV1>,
    /// Located abandonments.
    pub abandoned: Vec<AbandonedSectionV1>,
    /// Located abandonments that did not fit; each added a loss interval.
    pub abandoned_overflow: u64,
    /// Abandonments with an unreadable registration or abandonment time.
    pub abandoned_unlocated: u64,
    /// Issued sections whose record did not fit; each added a loss interval.
    pub issued_dropped_located: u64,
    /// Issued sections whose times could not be converted.
    pub issued_dropped_unlocated: u64,
    /// Loss intervals, sorted, at most 64.
    pub losses: Vec<LossIntervalV1>,
    /// Loss intervals inserted before any merge.
    pub loss_events: u64,
    /// A full loss list merged its nearest pair since the previous take; diagnostic only.
    pub losses_merged: bool,
    /// Registrations refused because the log was closed; no identity was issued.
    pub refused_closed: u64,
    /// Registrations refused because the sequence was exhausted; no identity was issued.
    pub refused_exhausted: u64,
    /// Refusals whose time could not be read (also counted by reason).
    pub refused_unlocated: u64,
    /// The earliest located refusal since the previous take.
    pub first_refusal_ns: Option<u64>,
}

/// The UI span store's transfer.
#[derive(Debug)]
pub struct SpanBatchV1 {
    /// Recorded spans, in record order.
    pub spans: Vec<SpanRecordV1>,
    /// Guards counting collections acquired since the previous take, recorded or not.
    pub spans_issued: u64,
    /// Spans not stored but covered by a loss interval.
    pub spans_dropped_located: u64,
    /// Spans not stored, with unreadable timing.
    pub spans_dropped_unlocated: u64,
    /// Loss intervals, sorted, at most 64.
    pub losses: Vec<LossIntervalV1>,
    /// Loss intervals inserted before any merge.
    pub loss_events: u64,
    /// A full loss list merged its nearest pair since the previous take; diagnostic only.
    pub losses_merged: bool,
    /// Counting collections with an identity that held at least one guard.
    pub collections_issued: u64,
    /// Collections refused an identity.
    pub collections_refused_exhausted: u64,
    /// The first collection identity this take's range can hold.
    pub prev_next_collection_seq: u64,
    /// The next collection identity; `u64::MAX` once exhausted.
    pub next_collection_seq: u64,
    /// The collection sequence reached its exhausted sentinel; sticky.
    pub identities_exhausted: bool,
    /// Collections open at the take.
    pub open_collections: u32,
}

/// Issue the next identity from `next`. `u64::MAX` is the exhausted sentinel: at it nothing is issued;
/// reaching it sets `exhausted` at once, so `next == u64::MAX` holds exactly when the flag is set.
pub(crate) fn issue(next: &mut u64, exhausted: &mut bool) -> Option<u64> {
    if *next == u64::MAX {
        // When: `next` is the sentinel, the sequence is exhausted: refuse, keep `next`, set the flag.
        *exhausted = true;
        return None;
    }
    let identity = *next;
    *next += 1;
    if *next == u64::MAX {
        *exhausted = true;
    }
    Some(identity)
}

#[cfg(test)]
thread_local! {
    /// Test-only: a clock that replaces [`correlation_clock`] on this thread.
    pub(crate) static CLOCK_OVERRIDE: RefCell<Option<Box<dyn FnMut() -> Instant>>> =
        const { RefCell::new(None) };
    /// Test-only: [`correlation_clock`] reads made on this thread.
    pub(crate) static CLOCK_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Test-only: runs as an abandonment takes the log mutex, to check no parser guard is held.
    pub(crate) static ABANDON_PROBE: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    /// Test-only: recording buffers (log, span store and loss list) allocated on this thread.
    pub(crate) static RECORDER_ALLOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Test-only: runs as a take holds a log mutex, before and after its swap, to count its allocations.
    pub(crate) static TAKE_LOCKED_PROBE: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// The correlation's single clock read; a test may substitute it.
pub(crate) fn correlation_clock() -> Instant {
    #[cfg(test)]
    {
        CLOCK_READS.with(|reads| reads.set(reads.get() + 1));
        let injected =
            CLOCK_OVERRIDE.with(|clock| clock.borrow_mut().as_mut().map(|clock| clock()));
        if let Some(injected) = injected {
            // When: injected is Some, a test's clock replaces Instant::now on this thread.
            return injected;
        }
    }
    Instant::now()
}

/// Nanoseconds from the run clock's epoch to `at`; `None` before the epoch or past `u64`.
pub(crate) fn epoch_ns(at: Instant) -> Option<u64> {
    let elapsed = at.checked_duration_since(super::frame_counters::clock_epoch())?;
    u64::try_from(elapsed.as_nanos()).ok()
}

/// Test-only: count one recording-buffer allocation on this thread.
#[cfg(test)]
fn note_recorder_allocation() {
    RECORDER_ALLOCATIONS.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
fn run_probe(probe: &'static std::thread::LocalKey<RefCell<Option<Box<dyn FnMut()>>>>) {
    probe.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            probe();
        }
    });
}

/// A bounded, sorted list of loss intervals. A full list merges its nearest pair into their hull,
/// so it never grows and every inserted interval stays covered.
#[derive(Debug)]
struct LossList {
    intervals: Vec<LossIntervalV1>,
    events: u64,
    merged: bool,
}

impl LossList {
    fn new() -> Self {
        #[cfg(test)]
        note_recorder_allocation();
        Self { intervals: Vec::with_capacity(LOSS_CAPACITY), events: 0, merged: false }
    }

    /// Insert `[from_ns, to_ns]`, merging the nearest pair first when the list is full.
    fn insert(&mut self, from_ns: u64, to_ns: u64) {
        self.events += 1;
        if self.intervals.len() == LOSS_CAPACITY {
            self.merge_nearest();
        }
        let interval = LossIntervalV1 { from_ns: from_ns.min(to_ns), to_ns: from_ns.max(to_ns) };
        let position = self.intervals.partition_point(|held| held.from_ns <= interval.from_ns);
        self.intervals.insert(position, interval);
    }

    /// Merge the adjacent pair with the smallest gap into their hull, freeing one slot.
    fn merge_nearest(&mut self) {
        let mut best = 0;
        let mut best_gap = u64::MAX;
        for index in 0..self.intervals.len() - 1 {
            let gap = self.intervals[index + 1].from_ns.saturating_sub(self.intervals[index].to_ns);
            if gap < best_gap {
                best = index;
                best_gap = gap;
            }
        }
        let later = self.intervals.remove(best + 1);
        let earlier = &mut self.intervals[best];
        earlier.to_ns = earlier.to_ns.max(later.to_ns);
        self.merged = true;
    }

    /// Move the intervals out for `replacement`, and reset the counts.
    fn take(&mut self, replacement: Vec<LossIntervalV1>) -> (Vec<LossIntervalV1>, u64, bool) {
        let intervals = std::mem::replace(&mut self.intervals, replacement);
        let taken = (intervals, self.events, self.merged);
        self.events = 0;
        self.merged = false;
        taken
    }
}

/// One pane log's state; every field is read and written under its mutex.
#[derive(Debug)]
pub(crate) struct LogState {
    records: Vec<SectionRecordV1>,
    pending: Option<PendingSectionV1>,
    abandoned: Vec<AbandonedSectionV1>,
    abandoned_overflow: u64,
    abandoned_unlocated: u64,
    issued_dropped_located: u64,
    issued_dropped_unlocated: u64,
    losses: LossList,
    refused_closed: u64,
    refused_exhausted: u64,
    refused_unlocated: u64,
    first_refusal_ns: Option<u64>,
    identities_exhausted: bool,
    closed: bool,
    next_section_seq: u64,
    /// The previous take's `next_section_seq`, the start of this take's issued range.
    prev_next_section_seq: u64,
    /// The previous take's pending identity.
    carried_pending: Option<u64>,
}

/// One pane's section log, shared by the pane's owner token and its VT worker.
#[derive(Debug)]
pub struct PaneSectionLog {
    pane_id: u64,
    state: Mutex<LogState>,
}

/// A worker section's registration: published once its endpoints are read, abandoned if dropped first.
#[derive(Debug)]
pub(crate) struct SectionRegistration<'log> {
    log: &'log PaneSectionLog,
    section_seq: u64,
    published: bool,
}

/// The buffers one log takes in exchange for its own, allocated before its mutex is taken.
#[derive(Debug)]
pub(crate) struct LogBuffers {
    records: Vec<SectionRecordV1>,
    abandoned: Vec<AbandonedSectionV1>,
    losses: Vec<LossIntervalV1>,
}

impl LogBuffers {
    /// Replacement buffers at the recording capacities.
    pub(crate) fn new() -> Self {
        #[cfg(test)]
        note_recorder_allocation();
        Self {
            records: Vec::with_capacity(SECTION_CAPACITY),
            abandoned: Vec::with_capacity(ABANDONED_CAPACITY),
            losses: Vec::with_capacity(LOSS_CAPACITY),
        }
    }
}

impl PaneSectionLog {
    /// An open log for `pane_id`, its buffers allocated now so recording never allocates.
    pub(crate) fn new(pane_id: u64) -> Self {
        Self::starting_at(pane_id, 1)
    }

    /// An open log whose next identity is `next_section_seq`, so a test can start near exhaustion.
    pub(crate) fn starting_at(pane_id: u64, next_section_seq: u64) -> Self {
        let buffers = LogBuffers::new();
        let state = LogState {
            records: buffers.records,
            pending: None,
            abandoned: buffers.abandoned,
            abandoned_overflow: 0,
            abandoned_unlocated: 0,
            issued_dropped_located: 0,
            issued_dropped_unlocated: 0,
            losses: LossList { intervals: buffers.losses, events: 0, merged: false },
            refused_closed: 0,
            refused_exhausted: 0,
            refused_unlocated: 0,
            first_refusal_ns: None,
            identities_exhausted: next_section_seq == u64::MAX,
            closed: false,
            next_section_seq,
            prev_next_section_seq: next_section_seq,
            carried_pending: None,
        };
        Self { pane_id, state: Mutex::new(state) }
    }

    /// Register a section before its parser lock: issue an identity and hold it pending, or refuse.
    /// A refusal issues nothing and records its time, or counts it unlocated.
    pub(crate) fn register(&self) -> Option<SectionRegistration<'_>> {
        let mut state = self.state.lock();
        let refused_closed = state.closed;
        let LogState { next_section_seq, identities_exhausted, .. } = &mut *state;
        // A closed log issues nothing; an open one follows the sentinel rule.
        let issued =
            (!refused_closed).then(|| issue(next_section_seq, identities_exhausted)).flatten();
        let at = epoch_ns(correlation_clock());
        let Some(section_seq) = issued else {
            // When: issued is None (the log is closed or exhausted), the section is refused with no identity.
            if refused_closed {
                state.refused_closed += 1;
            } else {
                // When: refused_closed is false, the refusal is the exhausted sequence's.
                state.refused_exhausted += 1;
            }
            match at {
                Some(at) => {
                    state.first_refusal_ns =
                        Some(state.first_refusal_ns.map_or(at, |first| first.min(at)));
                }
                None => state.refused_unlocated += 1,
            }
            return None;
        };
        state.pending = Some(PendingSectionV1 { section_seq, registered_ns: at });
        Some(SectionRegistration { log: self, section_seq, published: false })
    }

    /// Mark the log closed: it keeps everything it holds and refuses later registrations.
    pub(crate) fn close(&self) {
        self.state.lock().closed = true;
    }

    /// Exchange the log's buffers for `buffers` and return its transfer, and whether it may be pruned:
    /// closed, with nothing pending, so no later record can arrive.
    pub(crate) fn take(&self, buffers: LogBuffers) -> (PaneSectionsV1, bool) {
        let mut state = self.state.lock();
        #[cfg(test)]
        run_probe(&TAKE_LOCKED_PROBE);
        let records = std::mem::replace(&mut state.records, buffers.records);
        let abandoned = std::mem::replace(&mut state.abandoned, buffers.abandoned);
        let (losses, loss_events, losses_merged) = state.losses.take(buffers.losses);
        let pending = state.pending;
        let taken = PaneSectionsV1 {
            pane_id: self.pane_id,
            closed: state.closed,
            identities_exhausted: state.identities_exhausted,
            prev_next_section_seq: state.prev_next_section_seq,
            next_section_seq: state.next_section_seq,
            carried_pending: state.carried_pending,
            records,
            pending,
            abandoned,
            abandoned_overflow: std::mem::take(&mut state.abandoned_overflow),
            abandoned_unlocated: std::mem::take(&mut state.abandoned_unlocated),
            issued_dropped_located: std::mem::take(&mut state.issued_dropped_located),
            issued_dropped_unlocated: std::mem::take(&mut state.issued_dropped_unlocated),
            losses,
            loss_events,
            losses_merged,
            refused_closed: std::mem::take(&mut state.refused_closed),
            refused_exhausted: std::mem::take(&mut state.refused_exhausted),
            refused_unlocated: std::mem::take(&mut state.refused_unlocated),
            first_refusal_ns: state.first_refusal_ns.take(),
        };
        state.prev_next_section_seq = state.next_section_seq;
        state.carried_pending = pending.map(|pending| pending.section_seq);
        let prunable = state.closed && pending.is_none();
        #[cfg(test)]
        run_probe(&TAKE_LOCKED_PROBE);
        (taken, prunable)
    }
}

impl SectionRegistration<'_> {
    /// Publish the section with D2.1a's own reads before and after `lock()`. A time that cannot be
    /// converted is an unlocated drop; a full log is a located drop with its interval as a loss.
    pub(crate) fn publish(mut self, before_lock: Instant, locked_at: Instant) {
        self.published = true;
        let endpoints = epoch_ns(before_lock).zip(epoch_ns(locked_at));
        let mut state = self.log.state.lock();
        if state.pending.is_some_and(|pending| pending.section_seq == self.section_seq) {
            state.pending = None;
        }
        let Some((before_lock_ns, locked_at_ns)) = endpoints else {
            // When: endpoints is None (an endpoint precedes the epoch or overflows), it is an unlocated drop.
            state.issued_dropped_unlocated += 1;
            return;
        };
        if state.records.len() < SECTION_CAPACITY {
            let record =
                SectionRecordV1 { section_seq: self.section_seq, before_lock_ns, locked_at_ns };
            state.records.push(record);
        } else {
            // When: records.len() reached SECTION_CAPACITY, the section is a located drop and a loss.
            state.issued_dropped_located += 1;
            state.losses.insert(before_lock_ns, locked_at_ns);
        }
    }

    /// Abandon the section under the log mutex: located if both times convert and it fits.
    fn abandon(&self) {
        #[cfg(test)]
        run_probe(&ABANDON_PROBE);
        let mut state = self.log.state.lock();
        let abandoned_ns = epoch_ns(correlation_clock());
        let registered = state
            .pending
            .take_if(|pending| pending.section_seq == self.section_seq)
            .and_then(|pending| pending.registered_ns);
        let Some((registered_ns, abandoned_ns)) = registered.zip(abandoned_ns) else {
            // When: registered or abandoned_ns is None (unreadable), the abandonment is counted unlocated.
            state.abandoned_unlocated += 1;
            return;
        };
        if state.abandoned.len() < ABANDONED_CAPACITY {
            let entry =
                AbandonedSectionV1 { section_seq: self.section_seq, registered_ns, abandoned_ns };
            state.abandoned.push(entry);
        } else {
            // When: the abandoned list is full, it is an overflow and its interval a loss.
            state.abandoned_overflow += 1;
            state.losses.insert(registered_ns, abandoned_ns);
        }
    }
}

// Lifecycle: dropping an unpublished SectionRegistration calls abandon, under the log mutex.
impl Drop for SectionRegistration<'_> {
    fn drop(&mut self) {
        if !self.published {
            self.abandon();
        }
    }
}

/// The pane's unique owner of its log: dropping it with the pane closes the log. The worker's `Arc`
/// never closes it.
#[derive(Debug)]
pub(crate) struct CorrelationOwner {
    log: Arc<PaneSectionLog>,
}

// Lifecycle: dropping CorrelationOwner with its PaneState closes the log; its records stay until taken.
impl Drop for CorrelationOwner {
    fn drop(&mut self) {
        self.log.close();
    }
}

/// The App's UI span store; only the event loop touches it.
#[derive(Debug)]
pub(crate) struct SpanStore {
    spans: Vec<SpanRecordV1>,
    spans_issued: u64,
    spans_dropped_located: u64,
    spans_dropped_unlocated: u64,
    losses: LossList,
    collections_issued: u64,
    collections_refused_exhausted: u64,
    next_collection_seq: u64,
    prev_next_collection_seq: u64,
    identities_exhausted: bool,
    open_collections: u32,
}

/// The event loop's handle on the span store.
pub(crate) type SharedSpanStore = Rc<RefCell<SpanStore>>;

impl SpanStore {
    /// An empty store at its recording capacity, whose next collection identity is `next_collection_seq`.
    pub(crate) fn starting_at(next_collection_seq: u64) -> Self {
        #[cfg(test)]
        note_recorder_allocation();
        Self {
            spans: Vec::with_capacity(SPAN_CAPACITY),
            spans_issued: 0,
            spans_dropped_located: 0,
            spans_dropped_unlocated: 0,
            losses: LossList::new(),
            collections_issued: 0,
            collections_refused_exhausted: 0,
            next_collection_seq,
            prev_next_collection_seq: next_collection_seq,
            identities_exhausted: next_collection_seq == u64::MAX,
            open_collections: 0,
        }
    }

    /// Exchange the buffers for `spans` and `losses` and return the batch.
    fn take(&mut self, spans: Vec<SpanRecordV1>, losses: Vec<LossIntervalV1>) -> SpanBatchV1 {
        let (losses, loss_events, losses_merged) = self.losses.take(losses);
        let batch = SpanBatchV1 {
            spans: std::mem::replace(&mut self.spans, spans),
            spans_issued: std::mem::take(&mut self.spans_issued),
            spans_dropped_located: std::mem::take(&mut self.spans_dropped_located),
            spans_dropped_unlocated: std::mem::take(&mut self.spans_dropped_unlocated),
            losses,
            loss_events,
            losses_merged,
            collections_issued: std::mem::take(&mut self.collections_issued),
            collections_refused_exhausted: std::mem::take(&mut self.collections_refused_exhausted),
            prev_next_collection_seq: self.prev_next_collection_seq,
            next_collection_seq: self.next_collection_seq,
            identities_exhausted: self.identities_exhausted,
            open_collections: self.open_collections,
        };
        self.prev_next_collection_seq = self.next_collection_seq;
        batch
    }
}

/// One counting collection's span bookkeeping, prepared before its first `try_lock`; all on the stack.
#[derive(Debug)]
pub(crate) struct SpanCollection {
    store: SharedSpanStore,
    /// The collection's identity; `None` when the sequence refused one.
    collection_seq: Option<u64>,
    inline: [(u64, Option<u64>); INLINE_GUARDS],
    inline_len: usize,
    overflow_count: u32,
    overflow_earliest_acquired: Option<u64>,
    overflow_unlocated: bool,
    closed: bool,
}

impl SpanCollection {
    /// Open a collection on `store`: issue its identity, or count the refusal, and count it open.
    pub(crate) fn open(store: &SharedSpanStore) -> Self {
        let collection_seq = {
            let mut held = store.borrow_mut();
            let SpanStore { next_collection_seq, identities_exhausted, .. } = &mut *held;
            let collection_seq = issue(next_collection_seq, identities_exhausted);
            if collection_seq.is_none() {
                held.collections_refused_exhausted += 1;
            }
            held.open_collections += 1;
            collection_seq
        };
        Self {
            store: Rc::clone(store),
            collection_seq,
            inline: [(0, None); INLINE_GUARDS],
            inline_len: 0,
            overflow_count: 0,
            overflow_earliest_acquired: None,
            overflow_unlocated: false,
            closed: false,
        }
    }

    /// Note one guard acquired on `pane_id`, read once after its `try_lock` succeeded.
    pub(crate) fn note_guard(&mut self, pane_id: u64) {
        let acquired_ns = epoch_ns(correlation_clock());
        if self.inline_len < INLINE_GUARDS {
            // When: inline_len is below INLINE_GUARDS, the guard keeps its own exact span slot.
            self.inline[self.inline_len] = (pane_id, acquired_ns);
            self.inline_len += 1;
            return;
        }
        self.overflow_count += 1;
        match acquired_ns {
            Some(acquired_ns) => {
                let earliest = self
                    .overflow_earliest_acquired
                    .map_or(acquired_ns, |held| held.min(acquired_ns));
                self.overflow_earliest_acquired = Some(earliest);
            }
            None => self.overflow_unlocated = true,
        }
    }

    /// Close the collection after every guard was released at `released_ns` (`None`: unreadable):
    /// store its spans, count its drops and losses, and count it closed. Runs once.
    pub(crate) fn close(&mut self, released_ns: Option<u64>) {
        if self.closed {
            // When: the collection already closed, its spans were counted once.
            return;
        }
        self.closed = true;
        let mut store = self.store.borrow_mut();
        let guards = self.inline_len as u64 + u64::from(self.overflow_count);
        store.spans_issued += guards;
        store.open_collections = store.open_collections.saturating_sub(1);
        let (Some(collection_seq), Some(released_ns)) = (self.collection_seq, released_ns) else {
            // When: collection_seq or released_ns is None, every guard is an unlocated drop.
            store.spans_dropped_unlocated += guards;
            if self.collection_seq.is_some() && guards > 0 {
                store.collections_issued += 1;
            }
            return;
        };
        if guards > 0 {
            store.collections_issued += 1;
        }
        for &(pane_id, acquired_ns) in &self.inline[..self.inline_len] {
            let Some(acquired_ns) = acquired_ns else {
                // When: acquired_ns is None (unreadable), the guard's span is an unlocated drop.
                store.spans_dropped_unlocated += 1;
                continue;
            };
            if store.spans.len() < SPAN_CAPACITY {
                store.spans.push(SpanRecordV1 {
                    pane_id,
                    collection_seq,
                    acquired_ns,
                    released_ns,
                });
            } else {
                // When: the store is full, the span is a located drop and its interval a loss.
                store.spans_dropped_located += 1;
                store.losses.insert(acquired_ns, released_ns);
            }
        }
        if self.overflow_count == 0 {
            // When: overflow_count is 0, the collection held at most the inline guards and has no hull.
            return;
        }
        let overflow = u64::from(self.overflow_count);
        match self.overflow_earliest_acquired.filter(|_| !self.overflow_unlocated) {
            Some(earliest) => {
                store.spans_dropped_located += overflow;
                store.losses.insert(earliest, released_ns);
            }
            None => store.spans_dropped_unlocated += overflow,
        }
    }
}

// Lifecycle: dropping a SpanCollection its custody never closed calls close(None), so its guards count
// as unlocated and it is never left open.
impl Drop for SpanCollection {
    fn drop(&mut self) {
        self.close(None);
    }
}

/// The App's guard correlation: the registry of pane logs, the span store and the take sequence.
#[derive(Debug)]
pub(crate) struct GuardCorrelation {
    /// Registered logs; a closed, drained log with nothing pending is pruned at a take.
    logs: RefCell<Vec<Arc<PaneSectionLog>>>,
    /// The UI span store, shared with each counting collection.
    pub(crate) spans: SharedSpanStore,
    next_take_seq: u64,
    takes_exhausted: bool,
}

impl GuardCorrelation {
    /// An empty correlation whose first take, collection and section identities are 1.
    pub(crate) fn new() -> Self {
        Self {
            logs: RefCell::new(Vec::new()),
            spans: Rc::new(RefCell::new(SpanStore::starting_at(1))),
            next_take_seq: 1,
            takes_exhausted: false,
        }
    }

    /// Register a section log for `pane_id` and return its owner, whose drop closes it.
    pub(crate) fn attach(&self, pane_id: u64) -> (Arc<PaneSectionLog>, CorrelationOwner) {
        self.attach_log(Arc::new(PaneSectionLog::new(pane_id)))
    }

    /// Register `log` and return its owner.
    pub(crate) fn attach_log(
        &self,
        log: Arc<PaneSectionLog>,
    ) -> (Arc<PaneSectionLog>, CorrelationOwner) {
        self.logs.borrow_mut().push(Arc::clone(&log));
        (Arc::clone(&log), CorrelationOwner { log })
    }

    /// Start the take sequence at `next`, so a test can reach its exhaustion.
    #[cfg(test)]
    pub(crate) fn set_next_take_seq(&mut self, next: u64) {
        self.next_take_seq = next;
        self.takes_exhausted = next == u64::MAX;
    }

    /// Transfer every completed record with its evidence. Replacements are allocated first, outside every
    /// lock; each log's mutex is then held for one swap; the span store needs no lock.
    // Lock order: logs (a RefCell borrow) -> each log's state mutex, one at a time -> spans (a RefCell borrow).
    pub(crate) fn take(&mut self) -> GuardCorrelationTakeV1 {
        let log_count = self.logs.borrow().len();
        let mut replacements: Vec<LogBuffers> = (0..log_count).map(|_| LogBuffers::new()).collect();
        let span_buffers = (Vec::with_capacity(SPAN_CAPACITY), Vec::with_capacity(LOSS_CAPACITY));
        let mut panes = Vec::with_capacity(log_count);
        let Some(take_seq) = issue(&mut self.next_take_seq, &mut self.takes_exhausted) else {
            // When: issue gives no take_seq (exhausted), nothing is transferred; the replacements drop.
            return GuardCorrelationTakeV1::TakeExhausted;
        };
        let taken_at_ns = epoch_ns(correlation_clock());
        let mut prunable = Vec::with_capacity(log_count);
        for log in self.logs.borrow().iter() {
            let buffers = replacements.pop().unwrap_or_else(LogBuffers::new);
            let (pane, prune) = log.take(buffers);
            panes.push(pane);
            prunable.push(prune);
        }
        let mut prune_flags = prunable.into_iter();
        self.logs.borrow_mut().retain(|_| !prune_flags.next().unwrap_or(false));
        let spans = self.spans.borrow_mut().take(span_buffers.0, span_buffers.1);
        GuardCorrelationTakeV1::Taken(GuardCorrelationV1 {
            clock_epoch: super::frame_counters::clock_epoch(),
            take_seq,
            taken_at_ns,
            panes,
            spans,
        })
    }
}

impl super::App {
    /// Transfer every completed guard-correlation record with its loss, refusal and pending evidence.
    /// Bounded: one short critical section per pane log; the span store is event-loop-owned. It never
    /// takes a parser lock and never waits for a worker's section to finish; it may wait on a log mutex
    /// for a worker's O(1) register/publish/abandon step (publish may also merge the 64-entry loss list).
    #[doc(hidden)]
    pub fn take_guard_correlation_v1(&mut self) -> GuardCorrelationTakeV1 {
        match self.frame_counters.as_mut() {
            Some(counters) => counters.correlation.take(),
            None => GuardCorrelationTakeV1::GateOff,
        }
    }

    /// Give a counting pane its section log: the worker's handle records into it, and the returned
    /// owner, kept with the pane, closes it. `None` when the gate is off or the pane does not count.
    pub(super) fn attach_guard_correlation(
        &self,
        pane_id: u64,
        counters: &mut Option<super::frame_counters::PaneFrameCounters>,
    ) -> Option<CorrelationOwner> {
        let app = self.frame_counters.as_ref()?;
        let pane = counters.as_mut()?;
        let (log, owner) = app.correlation.attach(pane_id);
        pane.sections = Some(log);
        Some(owner)
    }
}

#[cfg(test)]
#[path = "guard_correlation_tests.rs"]
mod guard_correlation_tests;
