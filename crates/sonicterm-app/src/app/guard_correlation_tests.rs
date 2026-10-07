//! Unit tests for the raw guard-correlation records.

use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::Duration;

use super::*;
use crate::app::frame_counters::{clock_epoch, epoch_in, AppFrameCounters};
use crate::app::guard_custody::{start_at, CustodyTotals, GuardCustody};
use crate::lib_tests::CountAllocations;

/// The run clock's epoch, set before a test reads any correlation time.
fn epoch() -> Instant {
    clock_epoch()
}

/// An instant `offset_ns` after the epoch.
fn at(offset_ns: u64) -> Instant {
    epoch() + Duration::from_nanos(offset_ns)
}

/// An instant one second before the epoch: it never converts.
fn before_epoch() -> Instant {
    epoch().checked_sub(Duration::from_secs(1)).expect("an instant before the epoch")
}

/// Run `body` with the correlation clock replaced by `clock` on this thread.
fn with_clock<Value>(
    clock: impl FnMut() -> Instant + 'static,
    body: impl FnOnce() -> Value,
) -> Value {
    CLOCK_OVERRIDE.with(|held| *held.borrow_mut() = Some(Box::new(clock)));
    let value = body();
    CLOCK_OVERRIDE.with(|held| *held.borrow_mut() = None);
    value
}

/// A clock that advances `step_ns` per read from `start_ns` after the epoch.
fn stepping(start_ns: u64, step_ns: u64) -> impl FnMut() -> Instant {
    let mut next_ns = start_ns;
    move || {
        let read = at(next_ns);
        next_ns += step_ns;
        read
    }
}

/// The protocol's issued-identity identity for one pane transfer (§5.1).
fn accounted(pane: &PaneSectionsV1) -> bool {
    let resolved = pane.records.len() as u64
        + pane.abandoned.len() as u64
        + pane.abandoned_overflow
        + pane.abandoned_unlocated
        + pane.issued_dropped_located
        + pane.issued_dropped_unlocated
        + u64::from(pane.pending.is_some());
    let issued = pane.next_section_seq - pane.prev_next_section_seq
        + u64::from(pane.carried_pending.is_some());
    resolved == issued
}

/// Every guard of a span batch is a record or a counted drop.
fn spans_accounted(batch: &SpanBatchV1) -> bool {
    batch.spans_issued
        == batch.spans.len() as u64 + batch.spans_dropped_located + batch.spans_dropped_unlocated
}

/// One log's take, through fresh buffers.
fn take_log(log: &PaneSectionLog) -> PaneSectionsV1 {
    log.take(LogBuffers::new()).0
}

/// The span store's take.
fn take_spans(store: &SharedSpanStore) -> SpanBatchV1 {
    store.borrow_mut().take(Vec::with_capacity(SPAN_CAPACITY), Vec::with_capacity(LOSS_CAPACITY))
}

/// Open a collection on `store`, note `guards` guards on pane 7 and close it at `released_ns`.
fn collect(store: &SharedSpanStore, guards: usize, released_ns: Option<u64>) {
    let mut collection = SpanCollection::open(store);
    for _ in 0..guards {
        collection.note_guard(7);
    }
    collection.close(released_ns);
}

/// The sentinel rule: `next == u64::MAX` exactly when the flag is set, and nothing is issued at it.
#[test]
fn issue_reaches_and_holds_the_exhausted_sentinel() {
    let (mut next, mut exhausted) = (u64::MAX - 1, false);
    assert_eq!(issue(&mut next, &mut exhausted), Some(u64::MAX - 1));
    assert_eq!((next, exhausted), (u64::MAX, true), "reaching the sentinel sets the flag at once");
    assert_eq!(issue(&mut next, &mut exhausted), None);
    assert_eq!((next, exhausted), (u64::MAX, true), "a refusal keeps next and the flag");
}

/// T1: a section registered before a take and published after it is pending at take k, carried and
/// recorded at take k+1, and each take's identity holds.
#[test]
fn a_section_published_after_a_take_is_pending_then_carried() {
    epoch();
    let log = PaneSectionLog::new(3);
    let registration = log.register().expect("an open log issues");
    let first = take_log(&log);
    assert_eq!(first.pending.map(|pending| pending.section_seq), Some(1));
    assert!(first.records.is_empty() && accounted(&first));
    registration.publish(at(10), at(20));
    let second = take_log(&log);
    assert_eq!(second.carried_pending, Some(1));
    assert_eq!(
        second.records,
        vec![SectionRecordV1 { section_seq: 1, before_lock_ns: 10, locked_at_ns: 20 }]
    );
    assert_eq!(
        (second.prev_next_section_seq, second.next_section_seq),
        (2, 2),
        "an unchanged next"
    );
    assert!(second.pending.is_none() && accounted(&second));
}

/// T2: interleavings against consecutive takes: the first identity, a carry, pending across three
/// takes, abandonment overflow and refusals; each issued identity has exactly one disposition.
#[test]
fn every_issued_identity_has_exactly_one_disposition() {
    epoch();
    let log = PaneSectionLog::new(1);
    log.register().expect("first").publish(at(1), at(2));
    let lingering = log.register().expect("second");
    for _ in 0..3 {
        let pane = take_log(&log);
        assert_eq!(pane.pending.map(|pending| pending.section_seq), Some(2));
        assert!(accounted(&pane), "{pane:?}");
    }
    drop(lingering);
    for _ in 0..ABANDONED_CAPACITY {
        drop(log.register().expect("abandoned"));
    }
    let pane = take_log(&log);
    assert_eq!((pane.abandoned.len(), pane.abandoned_overflow), (ABANDONED_CAPACITY, 1));
    assert_eq!(pane.abandoned_overflow + pane.issued_dropped_located, pane.loss_events);
    assert!(!pane.losses.is_empty() && accounted(&pane));
    log.close();
    assert!(log.register().is_none());
    let pane = take_log(&log);
    assert_eq!(
        (pane.refused_closed, pane.prev_next_section_seq, pane.next_section_seq),
        (1, 67, 67)
    );
    assert!(accounted(&pane), "a refusal issues no identity");
}

/// T3: one record past capacity is a located drop with its interval as a loss; the first record stays.
#[test]
fn a_record_past_capacity_is_a_located_drop() {
    epoch();
    let log = PaneSectionLog::new(1);
    for index in 0..=SECTION_CAPACITY as u64 {
        log.register().expect("open").publish(at(index * 10), at(index * 10 + 5));
    }
    let pane = take_log(&log);
    assert_eq!(
        (pane.records.len(), pane.issued_dropped_located, pane.loss_events),
        (SECTION_CAPACITY, 1, 1)
    );
    assert_eq!(
        pane.records[0],
        SectionRecordV1 { section_seq: 1, before_lock_ns: 0, locked_at_ns: 5 }
    );
    let last = SECTION_CAPACITY as u64 * 10;
    assert_eq!(pane.losses, vec![LossIntervalV1 { from_ns: last, to_ns: last + 5 }]);
    assert!(accounted(&pane));
}

/// A full loss list merges its nearest pair, so it never grows and every interval stays covered.
#[test]
fn a_full_loss_list_merges_its_nearest_pair() {
    let mut losses = LossList::new();
    for index in 0..=LOSS_CAPACITY as u64 {
        losses.insert(index * 100, index * 100 + 10);
    }
    let (intervals, events, merged) = losses.take(Vec::new());
    assert_eq!((intervals.len(), events, merged), (LOSS_CAPACITY, LOSS_CAPACITY as u64 + 1, true));
    assert_eq!(
        intervals[0],
        LossIntervalV1 { from_ns: 0, to_ns: 110 },
        "the first pair is the nearest"
    );
}

/// T5: closing keeps records and evidence, a late publish lands, a new registration is refused with its
/// time, the worker's Arc never closes the log, and the log is pruned only once drained with nothing pending.
#[test]
fn closure_keeps_evidence_and_prunes_only_once_drained() {
    epoch();
    let mut correlation = GuardCorrelation::new();
    let (worker_log, owner) = correlation.attach(5);
    drop(Arc::clone(&worker_log));
    assert!(worker_log.register().is_some(), "dropping a worker handle does not close the log");
    let late = worker_log.register().expect("open before the owner drops");
    drop(owner);
    late.publish(at(1), at(2));
    assert!(worker_log.register().is_none(), "a closed log refuses");
    let lingering = Arc::new(PaneSectionLog::new(6));
    let (_, lingering_owner) = correlation.attach_log(Arc::clone(&lingering));
    let pending = lingering.register().expect("open");
    drop(lingering_owner);
    let GuardCorrelationTakeV1::Taken(first) = correlation.take() else {
        panic!("the take is not exhausted")
    };
    let closed =
        first.panes.iter().find(|pane| pane.pane_id == 5).expect("the closed log is transferred");
    assert!(closed.closed && closed.records.len() == 1 && closed.refused_closed == 1);
    assert!(closed.first_refusal_ns.is_some() && accounted(closed));
    let GuardCorrelationTakeV1::Taken(second) = correlation.take() else {
        panic!("the take is not exhausted")
    };
    let ids: Vec<u64> = second.panes.iter().map(|pane| pane.pane_id).collect();
    assert_eq!(ids, vec![6], "the drained log is pruned; the one still pending is kept");
    assert_eq!(Arc::strong_count(&worker_log), 1, "the worker's Arc still holds the pruned log");
    drop(pending);
}

/// T7 (threaded): a take returns while another thread holds a parser lock; it takes none.
#[test]
fn a_take_never_waits_for_a_parser_lock() {
    epoch();
    let parser = Arc::new(Mutex::new(0_u8));
    let held = Arc::clone(&parser);
    let (locked, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let holder = std::thread::spawn(move || {
        let _guard = held.lock();
        locked.0.send(()).unwrap();
        release.1.recv().unwrap();
    });
    locked.1.recv().unwrap();
    let mut correlation = GuardCorrelation::new();
    let (_log, _owner) = correlation.attach(1);
    assert!(matches!(correlation.take(), GuardCorrelationTakeV1::Taken(_)));
    assert!(parser.try_lock().is_none(), "the parser was held throughout");
    release.0.send(()).unwrap();
    holder.join().unwrap();
}

/// T8: section, collection and take sequences issue their last identity with the flag set at once,
/// validate in a take straight after, then refuse with `next` unchanged across later takes.
#[test]
fn sequences_exhaust_at_the_sentinel() {
    epoch();
    let log = PaneSectionLog::starting_at(1, u64::MAX - 1);
    log.register().expect("the last identity").publish(at(1), at(2));
    let first = take_log(&log);
    assert_eq!(first.records[0].section_seq, u64::MAX - 1);
    assert!(first.identities_exhausted && first.next_section_seq == u64::MAX && accounted(&first));
    assert!(log.register().is_none());
    for _ in 0..2 {
        let later = take_log(&log);
        assert!(
            later.identities_exhausted
                && later.prev_next_section_seq == u64::MAX
                && accounted(&later)
        );
    }
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(u64::MAX - 1)));
    collect(&store, 1, Some(5));
    let first = take_spans(&store);
    assert_eq!(
        (first.spans[0].collection_seq, first.next_collection_seq),
        (u64::MAX - 1, u64::MAX)
    );
    assert!(first.identities_exhausted);
    collect(&store, 2, Some(5));
    let refused = take_spans(&store);
    assert_eq!((refused.collections_refused_exhausted, refused.spans_dropped_unlocated), (1, 2));
    assert!(refused.identities_exhausted && refused.spans.is_empty() && spans_accounted(&refused));
    let mut correlation = GuardCorrelation::new();
    correlation.set_next_take_seq(u64::MAX - 1);
    assert!(
        matches!(correlation.take(), GuardCorrelationTakeV1::Taken(taken) if taken.take_seq == u64::MAX - 1)
    );
    assert!(matches!(correlation.take(), GuardCorrelationTakeV1::TakeExhausted));
}

/// T9: an open collection at a take is counted; a full store turns later spans into located drops with a
/// loss; a span omitted from a collection breaks the span identity.
#[test]
fn spans_count_open_collections_and_store_overflow() {
    epoch();
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    let open = SpanCollection::open(&store);
    assert_eq!(take_spans(&store).open_collections, 1);
    drop(open);
    for _ in 0..SPAN_CAPACITY / INLINE_GUARDS {
        collect(&store, INLINE_GUARDS, Some(9));
    }
    collect(&store, 1, Some(9));
    let batch = take_spans(&store);
    assert_eq!(
        (batch.spans.len(), batch.spans_dropped_located, batch.loss_events),
        (SPAN_CAPACITY, 1, 1)
    );
    assert!(spans_accounted(&batch) && batch.open_collections == 0);
    collect(&store, 2, Some(9));
    let mut omitted = take_spans(&store);
    omitted.spans.pop();
    assert!(!spans_accounted(&omitted), "a span omitted from a multi-pane collection is caught");
}

/// T9b: beyond 16 guards one conservative hull covers the overflow; a failed overflow read makes it
/// unlocated; a partial failure and an unwind close through the custody the same way.
#[test]
fn guards_past_sixteen_become_one_hull() {
    epoch();
    for guards in [17_usize, 20] {
        let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
        with_clock(stepping(100, 10), || collect(&store, guards, Some(10_000)));
        let batch = take_spans(&store);
        assert_eq!(batch.spans.len(), INLINE_GUARDS);
        assert_eq!(batch.spans_dropped_located, guards as u64 - INLINE_GUARDS as u64);
        assert_eq!(batch.losses, vec![LossIntervalV1 { from_ns: 100 + 16 * 10, to_ns: 10_000 }]);
        assert!(spans_accounted(&batch));
    }
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    let mut reads = 0_u64;
    with_clock(
        move || {
            reads += 1;
            if reads == 18 {
                before_epoch()
            } else {
                at(reads * 10)
            }
        },
        || collect(&store, 18, Some(10_000)),
    );
    let batch = take_spans(&store);
    assert_eq!((batch.spans.len(), batch.spans_dropped_unlocated, batch.loss_events), (16, 2, 0));
    for unwind in [false, true] {
        let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
        let totals = Arc::new(CustodyTotals::default());
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let (custody, _dispatch) = start_at(&totals, epoch());
            let mut custody = custody.with_spans(Some(SpanCollection::open(&store)));
            for _ in 0..18 {
                custody.note_guard(4);
            }
            if unwind {
                panic!("the collection unwinds after 18 guards");
            }
        }));
        assert_eq!(outcome.is_err(), unwind);
        let batch = take_spans(&store);
        assert_eq!(
            (batch.spans.len(), batch.spans_dropped_located, batch.open_collections),
            (16, 2, 0)
        );
        assert_eq!(
            totals.custodies.load(Ordering::Relaxed),
            1,
            "the scalar custody is recorded once"
        );
    }
}

/// T10: each timestamp failure is counted unlocated and never yields a number.
#[test]
fn unreadable_times_are_counted_unlocated() {
    epoch();
    let log = PaneSectionLog::new(1);
    let registration = with_clock(before_epoch, || log.register()).expect("issued");
    for _ in 0..2 {
        let pane = take_log(&log);
        assert_eq!(pane.pending.and_then(|pending| pending.registered_ns), None, "carried unknown");
    }
    drop(registration);
    let pane = take_log(&log);
    assert_eq!((pane.abandoned_unlocated, pane.abandoned.len()), (1, 0));
    assert!(accounted(&pane) && pane.losses.is_empty());
    let registration = log.register().expect("issued");
    with_clock(before_epoch, || drop(registration));
    log.register().expect("issued").publish(before_epoch(), at(1));
    let pane = take_log(&log);
    assert_eq!((pane.abandoned_unlocated, pane.issued_dropped_unlocated), (1, 1));
    assert!(pane.records.is_empty() && pane.losses.is_empty() && accounted(&pane));
    log.close();
    with_clock(before_epoch, || assert!(log.register().is_none()));
    let pane = take_log(&log);
    assert_eq!((pane.refused_unlocated, pane.first_refusal_ns), (1, None));
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    with_clock(before_epoch, || collect(&store, 2, Some(9)));
    collect(&store, 2, None);
    let batch = take_spans(&store);
    assert_eq!((batch.spans_dropped_unlocated, batch.losses.len()), (4, 0));
    let mut correlation = GuardCorrelation::new();
    let taken = with_clock(before_epoch, || correlation.take());
    assert!(matches!(taken, GuardCorrelationTakeV1::Taken(taken) if taken.taken_at_ns.is_none()));
}

/// C′: the custody reads the release once; the scalar and the span end at that same instant.
#[test]
fn one_release_read_ends_the_scalar_and_the_spans() {
    epoch();
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    let totals = Arc::new(CustodyTotals::default());
    let (custody, dispatch) = start_at(&totals, at(1_000_000));
    drop(dispatch);
    let mut custody = custody.with_spans(Some(SpanCollection::open(&store)));
    with_clock(|| at(2_000_000), || custody.note_guard(9));
    let reads_before = CLOCK_READS.with(Cell::get);
    with_clock(|| at(5_000_000), || drop(custody));
    assert_eq!(CLOCK_READS.with(Cell::get) - reads_before, 1, "exactly one release read");
    assert_eq!(totals.custody_ns.load(Ordering::Relaxed), 4_000_000);
    assert_eq!(
        take_spans(&store).spans[0].released_ns,
        5_000_000,
        "the same instant as the scalar's end"
    );
    let (custody, dispatch) = start_at(&totals, before_epoch() - Duration::from_secs(1));
    drop(dispatch);
    let mut custody = custody.with_spans(Some(SpanCollection::open(&store)));
    custody.note_guard(9);
    with_clock(before_epoch, || drop(custody));
    let custody_ns = totals.custody_ns.load(Ordering::Relaxed);
    assert_eq!(
        custody_ns,
        4_000_000 + 1_000_000_000,
        "the scalar records its interval as without spans"
    );
    assert_eq!(
        take_spans(&store).spans_dropped_unlocated,
        1,
        "an unrepresentable release is unlocated"
    );
}

/// D′: the epoch is initialized once, concurrent first callers agree, and it is never reset. That the
/// counters' constructor sets it is pinned in frame_counters_tests through an injected cell.
#[test]
fn the_epoch_is_set_once_and_shared() {
    let cell: &'static OnceLock<Instant> = Box::leak(Box::new(OnceLock::new()));
    let firsts: Vec<Instant> = (0..8)
        .map(|_| std::thread::spawn(move || epoch_in(cell)))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(firsts.iter().all(|first| *first == firsts[0]), "concurrent first access agrees");
    assert_eq!(epoch_in(cell), firsts[0], "never reset");
    let first = AppFrameCounters::new();
    let epoch_after_first = clock_epoch();
    let second = AppFrameCounters::new();
    assert_eq!(clock_epoch(), epoch_after_first, "a second App reuses the epoch");
    drop((first, second));
    assert!(epoch_ns(Instant::now()).is_some(), "a gate-on read converts as located");
}

/// T11: section recording to and past capacity, loss merging, abandonment, custody for 1, 16, 17 and 20
/// guards, the span store to and past capacity with overflow hulls and loss merging, recording after a take
/// and a worker thread's sections allocate nothing; inside a log's locked swap nothing is allocated; a whole
/// take allocates exactly its replacements and its transfer vectors.
#[test]
fn recording_never_allocates() {
    epoch();
    let log = Arc::new(PaneSectionLog::new(1));
    let store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    let totals = Arc::new(CustodyTotals::default());
    // Warm the thread locals (their destructor registration may allocate) before counting.
    drop(log.register());
    collect(&store, 1, Some(1));
    drop(take_log(&log));
    let counter = CountAllocations::start();
    for index in 0..SECTION_CAPACITY as u64 + LOSS_CAPACITY as u64 * 2 {
        log.register().expect("open").publish(at(index * 1_000), at(index * 1_000 + 1));
    }
    for _ in 0..=ABANDONED_CAPACITY {
        drop(log.register());
    }
    for guards in [1_usize, 16, 17, 20] {
        let (custody, dispatch) = start_at(&totals, epoch());
        let mut custody = custody.with_spans(Some(SpanCollection::open(&store)));
        for _ in 0..guards {
            custody.note_guard(2);
        }
        drop((custody, dispatch));
    }
    assert_eq!(counter.count(), 0, "recording allocated");
    drop(counter);
    // The span store to capacity and past it: inline spans, located drops, overflow hulls and loss merging.
    let span_store: SharedSpanStore = Rc::new(RefCell::new(SpanStore::starting_at(1)));
    collect(&span_store, 1, Some(1));
    drop(take_spans(&span_store));
    let counter = CountAllocations::start();
    for _ in 0..SPAN_CAPACITY / INLINE_GUARDS + LOSS_CAPACITY * 2 {
        collect(&span_store, INLINE_GUARDS, Some(u64::MAX / 2));
    }
    for guards in [17_usize, 20] {
        collect(&span_store, guards, Some(u64::MAX / 2));
    }
    assert_eq!(counter.count(), 0, "span recording to and past capacity allocated");
    drop(counter);
    let batch = take_spans(&span_store);
    assert_eq!(batch.spans.len(), SPAN_CAPACITY);
    assert!(
        batch.losses_merged && batch.loss_events > LOSS_CAPACITY as u64 && spans_accounted(&batch)
    );
    let counter = CountAllocations::start();
    collect(&span_store, 3, Some(u64::MAX / 2));
    assert_eq!(counter.count(), 0, "span recording after a take allocated");
    drop(counter);
    // The locked swap: the probe enables counting just after the log mutex is taken and reads it just
    // before the take returns, so only the code under the mutex is measured.
    let window: Rc<RefCell<(Option<CountAllocations>, Option<u64>)>> =
        Rc::new(RefCell::new((None, None)));
    let probe_window = Rc::clone(&window);
    TAKE_LOCKED_PROBE.with(|probe| {
        *probe.borrow_mut() = Some(Box::new(move || {
            let mut held = probe_window.borrow_mut();
            match held.0.take() {
                Some(counter) => held.1 = Some(counter.count()),
                None => held.0 = Some(CountAllocations::start()),
            }
        }));
    });
    let (pane, _) = log.take(LogBuffers::new());
    TAKE_LOCKED_PROBE.with(|probe| *probe.borrow_mut() = None);
    assert_eq!(window.borrow().1, Some(0), "the locked swap allocated");
    assert!(
        pane.losses_merged && pane.losses.len() == LOSS_CAPACITY && pane.abandoned_overflow == 1
    );
    let counter = CountAllocations::start();
    log.register().expect("open").publish(at(1), at(2));
    assert_eq!(counter.count(), 0, "recording after a take allocated");
    drop(counter);
    let worker_log = Arc::clone(&log);
    let worker_count = std::thread::spawn(move || {
        drop(worker_log.register());
        let counter = CountAllocations::start();
        worker_log.register().expect("open").publish(at(3), at(4));
        drop(worker_log.register());
        counter.count()
    })
    .join()
    .expect("the worker thread");
    assert_eq!(worker_count, 0, "a worker thread's sections allocated");
    let mut correlation = GuardCorrelation::new();
    let (_first, _first_owner) = correlation.attach(1);
    let (_second, _second_owner) = correlation.attach(2);
    drop(correlation.take());
    let counter = CountAllocations::start();
    let taken = correlation.take();
    let take_allocations = counter.count();
    drop(counter);
    drop(taken);
    // Per log three replacement buffers, plus the replacement list, the two span buffers, the panes and
    // the prune flags.
    assert_eq!(take_allocations, 3 * 2 + 1 + 2 + 1 + 1, "a take allocates only its replacements");
}

/// T12: the record layouts and the metadata sizes, asserted against the protocol's estimates.
#[test]
fn record_and_metadata_sizes() {
    use std::mem::size_of;
    let sizes = (
        size_of::<SectionRecordV1>(),
        size_of::<SpanRecordV1>(),
        size_of::<PendingSectionV1>(),
        size_of::<AbandonedSectionV1>(),
        size_of::<LossIntervalV1>(),
    );
    assert_eq!(sizes, (24, 32, 24, 24, 16));
    // The complete wrappers: the log with its pane id inside its Arc (16 B header), and the store with its
    // RefCell borrow state inside its Rc (16 B header).
    let log_meta = size_of::<PaneSectionLog>() + 16;
    let store_meta = size_of::<RefCell<SpanStore>>() + 16;
    let collection = size_of::<SpanCollection>();
    let custody = size_of::<GuardCustody>();
    println!(
        "guard-correlation metadata: log {log_meta} B, store {store_meta} B, collection {collection} B, \
         custody {custody} B"
    );
    assert!(log_meta <= 256 && store_meta <= 192, "log {log_meta}, store {store_meta}");
    // The inline guards dominate: 16 × 24 B, with the store handle, the identity and the overflow summary.
    assert!(collection <= 16 * 24 + 64, "collection {collection}");
    assert!(custody <= collection + 32, "custody {custody}");
}
