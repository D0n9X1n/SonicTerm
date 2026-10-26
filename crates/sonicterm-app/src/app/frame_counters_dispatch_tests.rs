//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::frame_counters_tests::*;
use super::*;

#[test]
fn lock_parser_outside_a_counting_dispatch_is_exactly_lock() {
    // Gate off, or no dispatch at all: no clock read and no tally write.
    let parser = Mutex::new(7_u32);
    let mut clock_reads = 0_u32;
    let mut clock = || {
        clock_reads += 1;
        Instant::now()
    };
    assert_eq!(*lock_counted(&parser, &mut clock), 7);
    {
        let _scope = DispatchScope::enter(None);
        assert_eq!(*lock_counted(&parser, &mut clock), 7);
    }
    assert_eq!(clock_reads, 0);
    assert_eq!(current_tally(), (0, 0));
}

#[test]
fn a_counting_dispatch_times_each_lock_and_drains_into_its_app() {
    // Each event-loop lock of a pane parser adds one wait to the App that opened the dispatch.
    let parser = Mutex::new(0_u32);
    let totals = Arc::new(DispatchTotals::default());
    let start = Instant::now();
    let mut ticks =
        [after_us(start, 0), after_us(start, 3), after_us(start, 10), after_us(start, 30)]
            .into_iter();
    let mut clock = || ticks.next().expect("four clock reads");
    {
        let _scope = DispatchScope::enter(Some(Arc::clone(&totals)));
        drop(lock_counted(&parser, &mut clock));
        drop(lock_counted(&parser, &mut clock));
        assert_eq!(totals.locks.load(Ordering::Relaxed), 0, "drained only at dispatch end");
    }
    let waits = totals.wait.snapshot();
    assert_eq!((totals.locks.load(Ordering::Relaxed), waits.count(), waits.sum_us()), (2, 2, 23));
    assert_eq!(current_tally(), (0, 0), "the thread-local is empty after the drain");
}

#[test]
fn nested_and_unwound_scopes_restore_the_outer_dispatch() {
    // A scope is a guard: an inner App's waits go to it, and a panic still drains and restores.
    let parser = Mutex::new(0_u32);
    let outer = Arc::new(DispatchTotals::default());
    let inner = Arc::new(DispatchTotals::default());
    {
        let _outer_scope = DispatchScope::enter(Some(Arc::clone(&outer)));
        {
            let _inner_scope = DispatchScope::enter(Some(Arc::clone(&inner)));
            drop(lock_parser(&parser));
        }
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _gate_off_scope = DispatchScope::enter(None);
            drop(lock_parser(&parser));
            panic!("handler unwinds");
        }));
        assert!(unwound.is_err());
        drop(lock_parser(&parser));
    }
    assert_eq!(inner.locks.load(Ordering::Relaxed), 1);
    assert_eq!(outer.locks.load(Ordering::Relaxed), 1, "the gate-off scope counted nothing");
}

#[test]
fn vt_section_derives_wait_parse_and_hold_from_its_four_instants() {
    // Wait is lock minus before, parse is parsed minus locked, hold is released minus locked.
    let stats = VtFrameStats::default();
    let start = Instant::now();
    let times = VtSectionTimes {
        before_lock: start,
        locked_at: after_us(start, 20),
        parsed_at: after_us(start, 520),
        released_at: after_us(start, 530),
    };
    stats.record_section(&times, 4_096);
    let wait = stats.parser_lock_wait.snapshot();
    let parse = stats.parse.snapshot();
    let hold = stats.parser_lock_hold.snapshot();
    assert_eq!((wait.sum_us(), parse.sum_us(), hold.sum_us()), (20, 500, 510));
    assert_eq!(stats.parse_bytes.load(Ordering::Relaxed), 4_096);
}

/// The first publication into an empty slot is not coalesced; one that finds a flush still
/// pending is, and says so, so the echo watch can record it.
#[test]
fn publish_flush_reports_whether_it_coalesced() {
    let stats = VtFrameStats::default();
    let slot = AtomicU64::new(0);
    assert!(!publish_flush(&slot, 1_000, &stats), "an empty slot stores the time");
    assert!(publish_flush(&slot, 2_000, &stats), "a pending flush coalesces the next");
    assert!(consume_flush(&slot, 3_000).is_some());
    assert!(!publish_flush(&slot, 4_000, &stats), "consumed, so the next is fresh");
}

#[test]
fn coalesced_flushes_give_one_observation_and_count_the_coalescing() {
    // The oldest pending flush is kept; a later one before the redraw only counts as coalesced.
    let stats = VtFrameStats::default();
    let slot = AtomicU64::new(0);
    publish_flush(&slot, 1_000, &stats);
    publish_flush(&slot, 2_000, &stats);
    assert_eq!(consume_flush(&slot, 5_000), Some(4_000));
    assert_eq!(consume_flush(&slot, 6_000), None, "consumed once");
    assert_eq!(stats.flushes.load(Ordering::Relaxed), 2);
    assert_eq!(stats.flushes_coalesced.load(Ordering::Relaxed), 1);
}

#[test]
fn an_untargeted_flush_stores_nothing() {
    // With no redraw target there is no window to consume a timestamp.
    let stats = VtFrameStats::default();
    note_untargeted_flush(&stats);
    assert_eq!(stats.flushes.load(Ordering::Relaxed), 1);
    assert_eq!(stats.flushes_untargeted.load(Ordering::Relaxed), 1);
}

#[test]
fn racing_publication_and_consumption_neither_lose_nor_double_count() {
    // Every publish is either observed once by a swap or counted as coalesced, never both.
    let stats = Arc::new(VtFrameStats::default());
    let slot = Arc::new(AtomicU64::new(0));
    let publishes = 20_000_u64;
    let producer = {
        let (stats, slot) = (Arc::clone(&stats), Arc::clone(&slot));
        std::thread::spawn(move || {
            for index in 1..=publishes {
                publish_flush(&slot, index, &stats);
            }
        })
    };
    let mut observed = 0_u64;
    while !producer.is_finished() {
        observed += u64::from(consume_flush(&slot, u64::MAX).is_some());
    }
    producer.join().unwrap();
    observed += u64::from(consume_flush(&slot, u64::MAX).is_some());
    assert_eq!(observed + stats.flushes_coalesced.load(Ordering::Relaxed), publishes);
}

#[test]
fn native_redraw_requests_count_per_window_inside_a_counting_dispatch() {
    // Requests reach winit from raw window handles across the App, so each is tallied by window
    // id in the open dispatch and attributed when the dispatch drains; none counts outside one.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let child = app.__test_seed_child_window(&["only"]);
    let main = app.main_window_id.expect("synthetic main");
    note_native_request(main);
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
        note_native_request(main);
        note_native_request(child);
    }
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    let count = |id| {
        let record =
            snapshot.windows.iter().find(|(window, _)| *window == id).map(|entry| &entry.1);
        record.and_then(|record| record.count("native_request_redraw"))
    };
    assert_eq!((count(main), count(child)), (Some(2), Some(1)));
    // A window closed by the dispatch that requested its redraw keeps that request too.
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(child);
        assert!(app.close_child_window(child));
    }
    let closed = app.frame_counters_snapshot().expect("counting app").closed_windows;
    assert_eq!(closed.count("native_request_redraw"), Some(2));
    let after = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(after.windows.len(), 1, "nothing stale is attributed to the closed child");
}

#[test]
fn with_the_gate_off_a_native_request_writes_nothing() {
    // A non-counting App's dispatch opens no tally, so the request only reaches winit.
    let mut app = app_under_filter("warn");
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
        assert_eq!(pending_native_requests(), 0);
    }
    assert!(app.frame_counters_snapshot().is_none());
}

#[test]
fn every_app_redraw_request_goes_through_the_counting_helper() {
    // A direct winit request_redraw() outside request_native_redraw would go uncounted.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    app_sources(&root, &root, &mut sources);
    let found = direct_redraw_requests(&sources);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].starts_with("app/frame_counters.rs:"), "{found:#?}");
    let source = include_str!("frame_counters.rs");
    let body = source_span(source, "pub(crate) fn request_native_redraw(", "\n}\n")
        .expect("counting helper");
    assert!(body.contains("note_native_request(window.id())"), "{body}");
    assert!(body.contains("window.request_redraw()"), "{body}");
}

#[test]
fn repeated_requesting_dispatches_reuse_the_tally_storage() {
    // After warm-up a dispatch opens with request storage already in place, so its first
    // native request allocates nothing, cycle after cycle.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    for _ in 0..3 {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
    }
    for _ in 0..100 {
        let _dispatch = app.frame_dispatch_scope();
        let before = pending_native_capacity();
        assert!(before >= 1, "the dispatch opened with no storage, so its request allocates");
        note_native_request(main);
        assert_eq!(pending_native_capacity(), before, "the request grew the storage");
    }
}

#[test]
fn requests_for_unregistered_windows_share_one_bounded_counter() {
    // Ids that never register keep no entry: their requests land in one app-wide count, so
    // storage stays fixed however many distinct ids are abandoned, in one dispatch or many.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let abandoned = |index: u64| winit::window::WindowId::from(u64::MAX / 2 + index);
    for round in 0..50 {
        let _dispatch = app.frame_dispatch_scope();
        for offset in 0..4 {
            note_native_request(abandoned(round * 4 + offset));
        }
        note_native_request(main);
    }
    {
        let _dispatch = app.frame_dispatch_scope();
        for index in 1_000..1_100 {
            note_native_request(abandoned(index));
        }
        assert!(
            pending_native_capacity() <= NATIVE_TALLY_CAPACITY,
            "one dispatch grew past the cap"
        );
    }
    let dispatch = &app.frame_counters.as_ref().expect("counting").dispatch;
    assert_eq!(dispatch.registered_entries(), 1, "only the registered main keeps an entry");
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(snapshot.app.count("native_request_redraw_unregistered"), Some(300));
    let main_record = &snapshot.windows.iter().find(|(id, _)| *id == main).expect("main").1;
    assert_eq!(main_record.count("native_request_redraw"), Some(50));
}
