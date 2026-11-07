//! Pins a collection's note: its parser wait, and for a miss the disjoint kind (parser, budget
//! expired before the probe, images) and the visible-pane index it happened on, as the window's
//! counters receive them.

use super::visible_frame_tests::*;
use super::*;

/// The fixture's sources as a counting App collects them: only then is a note measured.
fn counting(app: &mut App, window: WindowId) -> VisibleFrameSources {
    let mut counted = sources(app, window, false).ok().unwrap();
    counted.counted = Some(Arc::new(crate::app::guard_custody::CustodyTotals::default()));
    counted
}

/// A collection that takes both parsers notes its wait and no miss.
#[test]
fn a_held_collection_notes_its_parser_wait_and_no_miss() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = counting(&mut app, window);
    let acquire = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::BusyThenFree { wait: Duration::from_micros(300) }),
        (right, ScriptedOutcome::Free),
    ]);
    assert!(sources.try_collect_with(&acquire, Duration::from_micros(2_000), || {}).is_ok());
    assert_eq!(sources.take_note(), Some(CollectionNote { parser_wait_us: 300, miss: None }));
    assert_eq!(sources.take_note(), None, "a note is taken once");
}

/// A pane probed and still busy at the deadline is a parser miss at its own index.
#[test]
fn a_busy_parser_is_a_parser_miss_at_its_pane_index() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = counting(&mut app, window);
    let budget = Duration::from_micros(2_000);
    let acquire = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::Free),
        (right, ScriptedOutcome::BusyThroughout { wait: budget }),
    ]);
    assert!(sources.try_collect_with(&acquire, budget, || {}).is_err());
    let note = sources.take_note().expect("a miss is noted");
    assert_eq!(note.miss, Some((1, MissKind::Parser)));
    assert_eq!(note.parser_wait_us, 2_000);
}

/// A pane reached only after time ran out between panes, as preemption does, is a budget
/// expiry at its own index; a zero budget expires before the first pane.
#[test]
fn a_pane_the_deadline_left_no_time_for_is_a_budget_expiry_at_its_index() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = counting(&mut app, window);
    // Each clock read advances 1 us. Reads: start 1 (deadline 4), left's checks 2 and 3, so the
    // left pane is taken; the right pane's first read is 4, the deadline, so it is never offered time.
    let preempted =
        ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
    preempted.tick_per_read.set(Duration::from_micros(1));
    assert!(sources.try_collect_with(&preempted, Duration::from_micros(3), || {}).is_err());
    assert_eq!(sources.take_note().unwrap().miss, Some((1, MissKind::BudgetExpired)));
    assert!(
        !preempted.calls.borrow().contains(&AcquireCall::Probe(right)),
        "an expired pane is never probed"
    );
    let expired =
        ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
    assert!(sources.try_collect_with(&expired, Duration::ZERO, || {}).is_err());
    assert_eq!(sources.take_note().unwrap().miss, Some((0, MissKind::BudgetExpired)));
    // A separate classifier read would disagree here: with a 1 us tick and a 1 us budget the start
    // reads 1 (deadline 2), and the acquisition's own first read is 2, so the first pane is never
    // probed; an earlier extra read at 2 would have moved that to 3 yet still blamed the parser.
    let ticking =
        ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
    ticking.tick_per_read.set(Duration::from_micros(1));
    assert!(sources.try_collect_with(&ticking, Duration::from_micros(1), || {}).is_err());
    assert_eq!(sources.take_note().unwrap().miss, Some((0, MissKind::BudgetExpired)));
    assert!(ticking.calls.borrow().is_empty(), "the expired pane was never probed");
}

/// A guard that arrives at the deadline is dropped as the pane's parser miss, not an expiry: the
/// pane was offered time.
#[test]
fn a_late_guard_is_a_parser_miss_not_an_expiry() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = counting(&mut app, window);
    let budget = Duration::from_micros(2_000);
    let late = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::FreeLate { late: budget }),
        (right, ScriptedOutcome::Free),
    ]);
    assert!(sources.try_collect_with(&late, budget, || {}).is_err());
    assert_eq!(sources.take_note().unwrap().miss, Some((0, MissKind::Parser)));
}

/// With every parser held, a busy image store is an image miss at its pane's index.
#[test]
fn a_busy_image_store_is_an_image_miss_at_its_pane_index() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = counting(&mut app, window);
    let images = std::sync::Arc::clone(&app.registry.windows[&window].panes[&right].inline_images);
    let held = images.lock();
    let acquire =
        ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
    assert!(sources.try_collect_with(&acquire, Duration::from_micros(2_000), || {}).is_err());
    drop(held);
    assert_eq!(sources.take_note().unwrap().miss, Some((1, MissKind::Images)));
}

/// A window's counters take each kind into its own count and the index into its slot, so the
/// three kinds sum to the failed collections, the per-1,000 denominator being the attempts.
#[test]
fn window_counters_attribute_each_kind_and_index() {
    let mut counters = crate::app::frame_counters::WindowFrameCounters::default();
    let misses = [
        Some((0, MissKind::Parser)),
        Some((1, MissKind::BudgetExpired)),
        Some((3, MissKind::BudgetExpired)),
        Some((5, MissKind::Images)),
        None,
    ];
    for (wait_us, miss) in [10, 20, 30, 40, 50].into_iter().zip(misses) {
        counters.note_collection(wait_us, miss);
    }
    let kinds = (
        counters.contention_parser,
        counters.contention_budget_expired,
        counters.contention_images,
    );
    assert_eq!(kinds, (1, 2, 1));
    assert_eq!(counters.miss_pane, [1, 1, 0, 2], "index 3 and 5 share the last slot");
    assert_eq!(counters.parser_lock_wait.count(), 5, "every collection records its wait");
    assert_eq!(counters.parser_lock_wait.sum_us(), 150);
    let record = counters.record(None, 0);
    for (name, value) in [
        ("contention_budget_expired", 2),
        ("miss_pane_0", 1),
        ("miss_pane_1", 1),
        ("miss_pane_2", 0),
        ("miss_pane_3_plus", 2),
    ] {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
}

/// Both redraw paths, main and child, take each collection's note into the window's counters right
/// after collecting, success or miss; a path that skipped it would leave its window's attribution
/// and collection count silently empty (read as LF on any checkout).
#[test]
fn both_redraw_paths_record_every_collection_note() {
    for (name, source) in [
        ("main", include_str!("window_event.rs")),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let compact: String =
            source.replace("\r\n", "\n").lines().map(str::trim).collect::<Vec<_>>().join("\n");
        let collect =
            "let collected = sources.try_collect(|| self.snapshot_window_redraw(win_id));";
        let record = "self.note_collection(win_id, sources.take_note());";
        assert_eq!(compact.matches(collect).count(), 1, "{name}: one collection");
        assert!(compact.contains(&format!("{collect}\n{record}")), "{name}: the note follows it");
    }
}

/// A collection in an App that does not count reads the clock only for its deadline: no extra
/// read for a parser wait and no note, hit or miss.
#[test]
fn a_counters_off_collection_measures_nothing_extra() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let plain = sources(&mut app, window, false).ok().unwrap();
    let counted = counting(&mut app, window);
    let reads = |sources: &VisibleFrameSources| {
        let acquire =
            ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
        acquire.tick_per_read.set(Duration::from_nanos(1));
        let start = acquire.now();
        assert!(sources.try_collect_with(&acquire, Duration::from_millis(2), || {}).is_ok());
        acquire.now().duration_since(start).as_nanos() - 1
    };
    let (plain_reads, counted_reads) = (reads(&plain), reads(&counted));
    assert_eq!(plain.take_note(), None, "no note without counters");
    assert!(counted.take_note().is_some());
    // The deadline needs one read to start, then before and after each pane's probe; a counting
    // collection adds exactly its one parser-wait read.
    assert_eq!(plain_reads, 1 + 2 * 2);
    assert_eq!(counted_reads, plain_reads + 1);
}
