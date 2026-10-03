//! Pins the frame-counter core: histogram bounds, the line cadence, `lock_parser`'s gate and
//! dispatch scope, the VT section record and the flush slot.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::*;

/// `start` plus `micros` microseconds.
fn after_us(start: Instant, micros: u64) -> Instant {
    start + Duration::from_micros(micros)
}

#[test]
fn histogram_reports_exact_sums_and_bucket_bounds_never_exact_maxima() {
    // A maximum or p95 is only known to lie in a bucket, so it prints as `_le` or `_gt`.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(3_000);
    frames.record_us(5_500);
    assert_eq!(frames.count(), 2);
    assert_eq!(frames.sum_us(), 8_500);
    assert_eq!(frames.max(), Some(Bound::AtMost(7)));
    assert_eq!(frames.p95(), Some(Bound::AtMost(7)));
    frames.record_us(120_000);
    assert_eq!(frames.max(), Some(Bound::Above(100)));
    let fields = frames.line_fields("present_interval");
    assert!(fields.contains("present_interval_sum_us=128500"), "{fields}");
    assert!(fields.contains("present_interval_max_gt_ms=100"), "{fields}");
    assert!(fields.contains("present_interval_p95_gt_ms=100"), "{fields}");
    // A value equal to a bound belongs to that bound's bucket.
    let mut waits = Histogram::new(HistogramUnit::Micros);
    waits.record_us(10);
    assert_eq!(waits.max(), Some(Bound::AtMost(10)));
    assert!(waits.line_fields("ui_parser_wait").contains("ui_parser_wait_max_le_us=10"));
}

#[test]
fn an_empty_interval_prints_no_fields() {
    // Lines omit zero fields, so a histogram with no events contributes nothing.
    let frames = Histogram::new(HistogramUnit::Millis);
    assert_eq!(frames.line_fields("handler"), "");
    assert_eq!((frames.max(), frames.p95()), (None, None));
}

#[test]
fn interval_bounds_come_only_from_buckets_that_grew() {
    // Counters are cumulative and never reset; a reader takes deltas from its own snapshot.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(150_000);
    let earlier = frames.clone();
    frames.record_us(3_000);
    let interval = frames.delta_since(&earlier);
    assert_eq!(interval.count(), 1);
    assert_eq!(interval.sum_us(), 3_000);
    assert_eq!(interval.max(), Some(Bound::AtMost(4)));
}

#[test]
fn atomic_histogram_snapshot_matches_its_records() {
    // VT workers record into shared atomics; a snapshot reads the same counts and sum.
    let shared = AtomicHistogram::new(HistogramUnit::Micros);
    shared.record_us(40);
    shared.record_us(6_000);
    let snapshot = shared.snapshot();
    assert_eq!((snapshot.count(), snapshot.sum_us()), (2, 6_040));
    assert_eq!(snapshot.max(), Some(Bound::Above(5_000)));
}

#[test]
fn a_source_prints_at_most_one_line_a_second_and_one_final_line() {
    // Only authorizing events print; maintenance wakes never do, and nothing follows final=1.
    let start = Instant::now();
    let mut cadence = LineCadence::default();
    assert!(cadence.line_due(start, true));
    assert!(!cadence.line_due(start + Duration::from_millis(400), true));
    assert!(!cadence.line_due(start + Duration::from_secs(2), false), "maintenance wake");
    assert!(cadence.line_due(start + Duration::from_secs(2), true));
    assert!(!cadence.final_line(false), "no pending counts, no final line");
    assert!(!cadence.line_due(start + Duration::from_secs(9), true), "closed source");
    let mut open = LineCadence::default();
    assert!(open.final_line(true));
    assert!(!open.final_line(true), "final=1 is printed once");
}

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

/// A new App under `filter`, so its gate does not depend on any process-wide subscriber.
fn app_under_filter(filter: &str) -> crate::app::App {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::new(filter));
    sonicterm_logging::test_capture::with_default(subscriber, || {
        crate::app::App::new(Theme::default(), Config::default(), Keymap::default())
    })
}

#[test]
fn an_apps_gate_follows_the_frame_counters_target_when_it_is_built() {
    // Debug logging turns the counters on; the default level leaves them off.
    assert!(app_under_filter("warn,frame_counters=debug").frame_counters.is_some());
    assert!(app_under_filter("warn").frame_counters.is_none());
}

#[test]
fn forcing_the_gate_succeeds_only_before_the_first_window_or_pane() {
    // Once a window or pane exists, something may already have read the gate.
    let mut early = app_under_filter("warn");
    assert_eq!(early.force_frame_counters_on(), Ok(()));
    assert!(early.frame_counters.is_some());
    let mut after_window = app_under_filter("warn");
    after_window.__test_synthetic_main();
    assert_eq!(after_window.force_frame_counters_on(), Err(FrameCountersTooLate));
    assert!(after_window.frame_counters.is_none());
    let mut after_pane = app_under_filter("warn");
    assert!(after_pane.pane_frame_counters().is_none());
    assert_eq!(after_pane.force_frame_counters_on(), Err(FrameCountersTooLate));
}

#[test]
fn two_apps_keep_independent_gates_and_each_pane_shares_its_apps_statistics() {
    // A pane's worker records into its own App's statistics through its own flush slot.
    let mut counting = app_under_filter("warn");
    counting.force_frame_counters_on().unwrap();
    let silent = &mut app_under_filter("warn");
    let first = counting.pane_frame_counters().expect("gate on");
    let second = counting.pane_frame_counters().expect("gate on");
    let app_vt = &counting.frame_counters.as_ref().unwrap().vt;
    assert!(Arc::ptr_eq(&first.vt, app_vt) && Arc::ptr_eq(&second.vt, app_vt));
    assert!(!Arc::ptr_eq(&first.pending_flush, &second.pending_flush));
    assert!(silent.pane_frame_counters().is_none());
}

#[test]
fn both_pane_creators_attach_counters_before_starting_the_worker() {
    // The worker clones its handles from the pane, so they must be attached first.
    for (name, source) in [
        ("spawn_pane.rs", include_str!("spawn_pane.rs")),
        ("child_tabs.rs", include_str!("child_tabs.rs")),
    ] {
        let attach = source
            .find("frame_counters = self.pane_frame_counters();")
            .unwrap_or_else(|| panic!("{name} attaches no counters"));
        assert!(source[attach..].contains("spawn_pane_workers("), "{name} starts its worker first");
    }
}

#[test]
fn a_counting_apps_dispatch_scope_counts_each_parser_lock() {
    // The App opens a scope at each dispatch; its event-loop parser locks drain into its totals.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let parser = Mutex::new(0_u32);
    {
        let _dispatch = app.frame_dispatch_scope();
        drop(lock_parser(&parser));
    }
    let totals = &app.frame_counters.as_ref().unwrap().dispatch;
    assert_eq!(totals.locks.load(Ordering::Relaxed), 1);
    assert_eq!(totals.wait.snapshot().count(), 1);
}

#[test]
fn every_application_handler_method_opens_a_dispatch_scope() {
    // A dispatch without a scope would lock parsers uncounted.
    let module = include_str!("mod.rs");
    let start = module.find("impl ApplicationHandler<UserEvent> for App {").expect("handler impl");
    let body = &module[start..start + module[start..].find("\n}\n").expect("impl end")];
    let methods = body.matches("\n    fn ").count();
    assert_eq!(methods, 6, "the handler methods changed; review this test");
    assert_eq!(body.matches("let _dispatch = self.frame_dispatch_scope();").count(), methods);
}

/// Every non-test source under `dir` (recursively), as `(path, text)`.
fn app_sources(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("app sources") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            app_sources(&path, found);
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") && !name.starts_with("test_hooks_")
        {
            found.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
        }
    }
}

#[test]
fn event_loop_pane_parser_locks_go_through_lock_parser() {
    // An uncounted lock would hide event-loop waits; only the VT worker keeps a plain lock().
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let mut sources = Vec::new();
    app_sources(&root, &mut sources);
    let mut plain = Vec::new();
    for (path, text) in &sources {
        if path.ends_with("spawn_pane.rs") || path.ends_with("frame_counters.rs") {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let locks_parser = code.contains("parser.lock()") || code.contains("parser_arc.lock()");
            if locks_parser && !code.starts_with("//") {
                plain.push(format!("{path}:{}", index + 1));
            }
        }
    }
    assert!(plain.is_empty(), "pane parser locked without lock_parser: {plain:#?}");
}

#[test]
fn the_winning_deferral_rule_is_counted_and_later_predicates_never_run() {
    // The streaming check allocates and does Acquire loads, so it must not run after a winner.
    let later = std::cell::Cell::new(0_u32);
    let bump = || {
        later.set(later.get() + 1);
        true
    };
    assert_eq!(defer_rule(|| true, bump, bump), Some(DeferRule::Timeout));
    assert_eq!(defer_rule(|| false, || true, bump), Some(DeferRule::Contention));
    assert_eq!(later.get(), 0, "no predicate ran after the winner");
    assert_eq!(defer_rule(|| false, || false, || true), Some(DeferRule::Streaming));
    assert_eq!(defer_rule(|| false, || false, || false), None);
}

#[test]
fn settlements_count_each_outcome_and_the_present_interval() {
    // Every FrameSettlement has its own counter; only presented frames time an interval.
    use crate::app::redraw::{FrameSettlement, RedrawCause};
    use sonicterm_gpu::core::SurfaceRetryReason;
    let start = Instant::now();
    let mut counters = WindowFrameCounters::default();
    for outcome in [
        FrameSettlement::Presented,
        FrameSettlement::Cached,
        FrameSettlement::Settled,
        FrameSettlement::Retry(RedrawCause::Input),
        FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        FrameSettlement::Stopped(3),
        FrameSettlement::Failed,
    ] {
        counters.record_settlement(outcome, start);
    }
    counters.record_settlement(FrameSettlement::Presented, after_us(start, 16_000));
    let outcomes = (
        counters.presented,
        counters.cached,
        counters.settled,
        counters.retry,
        counters.surface_retry,
        counters.stopped,
        counters.failed,
    );
    assert_eq!(outcomes, (2, 1, 1, 1, 1, 1, 1));
    let interval = &counters.present_interval;
    assert_eq!((interval.count(), interval.sum_us()), (1, 16_000));
    counters.note_defer(DeferRule::Streaming);
    counters.note_contention(false);
    assert_eq!((counters.defer_streaming, counters.contention_parser), (1, 1));
}

#[test]
fn a_counting_apps_windows_count_outcomes_and_contention_on_their_paths() {
    // Only a counting App's windows carry counters; the shared paths feed them.
    use crate::app::redraw::FrameSettlement;
    use crate::app::visible_frame::FrameUnavailable;
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let now = Instant::now();
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Presented, now);
    let contended = FrameUnavailable::Contended { pane_id: 1, images: true };
    app.visible_frame_unavailable(main, contended, false, now);
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting window");
    let counted = (
        counters.presented,
        counters.contention_images,
        counters.contention_parser,
        counters.contention_retry_armed,
    );
    assert_eq!(counted, (1, 1, 0, 1));
    let mut silent = app_under_filter("warn");
    silent.__test_synthetic_main();
    let silent_main = silent.main_window_id.unwrap();
    assert!(silent.windows[&silent_main].redraw.frame_counters.is_none());
}

#[test]
fn wakes_and_dispatch_stalls_count_by_cause_and_kind() {
    // Each StartCause has its own wake counter; each dispatch kind its own duration histogram.
    use winit::event::StartCause;
    let start = Instant::now();
    let mut counters = AppFrameCounters::new();
    for cause in [
        StartCause::Init,
        StartCause::Poll,
        StartCause::WaitCancelled { start, requested_resume: None },
        StartCause::ResumeTimeReached { start, requested_resume: start },
        StartCause::ResumeTimeReached { start, requested_resume: start },
    ] {
        counters.note_wake(&cause);
    }
    let wakes = (
        counters.wake_init,
        counters.wake_poll,
        counters.wake_wait_cancelled,
        counters.wake_resume_time,
    );
    assert_eq!(wakes, (1, 1, 1, 2));
    counters.record_dispatch(DispatchKind::AboutToWait, 2_000);
    counters.record_dispatch(DispatchKind::UserEvent, 30_000);
    counters.record_dispatch(DispatchKind::NewEvents, 500);
    let sums = (
        counters.about_to_wait.sum_us(),
        counters.user_event.sum_us(),
        counters.new_events.sum_us(),
    );
    assert_eq!(sums, (2_000, 30_000, 500));
}

#[test]
fn probes_are_timed_once_per_call_and_an_empty_batch_is_not_counted() {
    // A single and a batch probe count once each; an empty batch makes no snapshot and no count.
    let totals = Arc::new(DispatchTotals::default());
    let start = Instant::now();
    let mut ticks =
        [after_us(start, 0), after_us(start, 40), after_us(start, 100), after_us(start, 300)]
            .into_iter();
    let mut clock = || ticks.next().expect("two reads per counted probe");
    let mut probes = 0_u32;
    {
        let _scope = DispatchScope::enter(Some(Arc::clone(&totals)));
        time_probe_with(1, &mut clock, || probes += 1);
        time_probe_with(3, &mut clock, || probes += 1);
        time_probe_with(0, &mut clock, || probes += 1);
    }
    assert_eq!(probes, 3, "every probe still runs");
    let counted =
        (totals.probe_calls.load(Ordering::Relaxed), totals.probe_panes.load(Ordering::Relaxed));
    assert_eq!(counted, (2, 4));
    assert_eq!(totals.probe.snapshot().sum_us(), 240);
}

#[test]
fn a_redraw_takes_each_pending_flush_once_into_flush_to_redraw() {
    // Oldest pending flush to the first redraw that takes it; a second redraw finds nothing.
    let slots = [AtomicU64::new(1_000_000), AtomicU64::new(0)];
    let mut counters = WindowFrameCounters::default();
    counters.note_redraw(slots.iter(), 5_000_000);
    counters.note_redraw(slots.iter(), 9_000_000);
    let flushes = &counters.flush_to_redraw;
    assert_eq!((counters.redraw_requested, flushes.count(), flushes.sum_us()), (2, 1, 4_000));
}

#[test]
fn a_counting_apps_window_counts_requests_redraws_and_handler_time() {
    // The App's hooks reach the window's counters; with the gate off no clock is read.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    app.note_user_request_redraw(main);
    app.note_redraw_requested(main);
    app.note_window_handler(main, Instant::now() - Duration::from_millis(5));
    app.request_owner_redraw(main, crate::app::redraw::RedrawCause::Output);
    let window = &app.windows[&main];
    let counters = window.redraw.frame_counters.as_deref().expect("counting window");
    assert_eq!(
        (counters.user_request_redraw, counters.redraw_requested, counters.handler.count()),
        (1, 1, 1)
    );
    assert_eq!(counters.native_request_redraw, u64::from(window.redraw.request_in_flight));
    assert!(app_under_filter("warn").frame_clock_start().is_none());
}

#[test]
fn a_source_line_prints_deltas_at_most_once_a_second_then_one_final_line() {
    // Lines carry span_ms and only nonzero deltas; maintenance wakes never print, and nothing
    // follows final=1.
    let start = Instant::now();
    let mut line = LineState::new(start);
    let mut record = CounterRecord::default();
    record.push_count("presented", 3);
    record.push_count("failed", 0);
    let first =
        line.line("main", &record, start + Duration::from_millis(200), true).expect("first");
    assert!(first.starts_with("window=main span_ms=200 "), "{first}");
    assert!(first.contains("presented=3") && !first.contains("failed"), "{first}");
    assert!(line.line("main", &record, start + Duration::from_millis(700), true).is_none());
    let mut later = CounterRecord::default();
    later.push_count("presented", 5);
    later.push_count("failed", 0);
    let three_s = start + Duration::from_secs(3);
    assert!(line.line("main", &later, three_s, false).is_none(), "a maintenance wake");
    let second = line.line("main", &later, three_s, true).expect("second");
    assert!(second.contains("presented=2"), "{second}");
    assert!(line.final_line("main", &later, three_s).is_none(), "nothing pending");
    let mut closing = LineState::new(start);
    let closed =
        closing.final_line("child-2", &later, start + Duration::from_millis(50)).expect("final");
    assert!(closed.starts_with("window=child-2 final=1 span_ms=50 "), "{closed}");
    assert!(closing.line("child-2", &later, three_s, true).is_none(), "nothing after final=1");
}

#[test]
fn counter_records_merge_by_name_and_take_deltas() {
    // Closed windows merge into one fixed-size record; lines print deltas from the last one.
    let mut waits = Histogram::new(HistogramUnit::Millis);
    waits.record_us(3_000);
    let mut first = CounterRecord::default();
    first.push_count("presented", 2);
    first.push_histogram("handler", waits.clone());
    let mut second = CounterRecord::default();
    second.push_count("presented", 5);
    second.push_count("failed", 1);
    second.push_histogram("handler", waits);
    let mut closed = CounterRecord::default();
    closed.merge(&first);
    closed.merge(&second);
    assert_eq!((closed.count("presented"), closed.count("failed")), (Some(7), Some(1)));
    let handler = (closed.histogram_count("handler"), closed.histogram_sum_us("handler"));
    assert_eq!(handler, (Some(2), Some(6_000)));
    let delta = second.delta_since(&first);
    assert_eq!((delta.count("presented"), delta.histogram_count("handler")), (Some(3), Some(0)));
}

#[test]
fn a_closed_windows_totals_move_to_closed_windows_and_late_vt_counts_still_appear() {
    // The snapshot's sum never drops when a window closes, and VT counts are app-wide.
    use crate::app::redraw::FrameSettlement;
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Presented, Instant::now());
    let pane = app.pane_frame_counters().expect("gate on");
    let open = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(open.windows.len(), 1);
    assert_eq!(open.windows[0].1.count("presented"), Some(1));
    let mut removed = app.windows.remove(&main).unwrap();
    app.retire_window_counters(&mut removed);
    pane.vt.note_batch();
    let closed = app.frame_counters_snapshot().expect("counting app");
    assert!(closed.windows.is_empty());
    assert_eq!(closed.closed_windows.count("presented"), Some(1));
    assert_eq!(closed.app.count("batches"), Some(1), "a late increment from a closed pane");
    assert!(app_under_filter("warn").frame_counters_snapshot().is_none());
}

#[test]
fn only_authorizing_dispatches_print_lines_and_exit_prints_the_final_ones() {
    // Maintenance wakes count but never print, and no timer is armed for a line.
    let module = include_str!("mod.rs");
    let start = module.find("impl ApplicationHandler<UserEvent> for App {").expect("handler impl");
    let body = &module[start..start + module[start..].find("\n}\n").expect("impl end")];
    for method in body.split("\n    fn ").skip(1) {
        let name = &method[..method.find('(').expect("signature")];
        let prints = method.contains("emit_frame_lines(");
        assert_eq!(prints, matches!(name, "user_event" | "window_event"), "{name}");
        assert_eq!(method.contains("finish_frame_lines()"), name == "exiting", "{name}");
    }
}

#[test]
fn registration_turns_on_each_counting_windows_renderer() {
    // Every renderer is attached before its window registers, so registration sets its flag.
    let module = include_str!("mod.rs");
    let start = module.find("fn insert_window_registered(").expect("registration");
    let body = &module[start..start + module[start..].find("\n    }\n").expect("end")];
    assert!(body.contains("renderer.set_frame_counting(true)"), "{body}");
}

#[test]
fn every_window_removal_retires_its_counters() {
    // A removed window's totals move to closed_windows before it drops.
    for (name, source) in [
        ("child_window.rs", include_str!("child_window.rs")),
        ("child_tabs.rs", include_str!("child_tabs.rs")),
        ("session.rs", include_str!("session.rs")),
    ] {
        let removals = source.matches("self.windows.remove(").count();
        assert_eq!(source.matches("self.retire_window_counters(").count(), removals, "{name}");
    }
}

#[test]
fn readiness_is_known_before_any_record_is_built() {
    // Building a record allocates, so a dispatch checks readiness first and builds nothing for
    // a frame that cannot print a line.
    let start = Instant::now();
    let mut cadence = LineCadence::default();
    assert!(cadence.ready(start));
    assert!(cadence.line_due(start, true));
    assert!(!cadence.ready(start + Duration::from_millis(500)), "printed within a second");
    assert!(cadence.ready(start + Duration::from_secs(2)));
    cadence.final_line(false);
    assert!(!cadence.ready(start + Duration::from_secs(9)), "closed");
}

#[test]
fn line_records_are_built_only_when_a_line_could_print() {
    // A RedrawRequested arrives every frame; the record is built at most once a second per source.
    let source = include_str!("frame_counters.rs");
    let start = source.find("pub(super) fn emit_frame_lines(").expect("emit_frame_lines");
    let body = &source[start..start + source[start..].find("\n    }\n").expect("end")];
    let window_ready = body.find("counters.line.ready(now)").expect("window readiness check");
    assert!(window_ready < body.find("counters.record(").expect("window record"));
    let app_ready = body.find("app.line.ready(now)").expect("app readiness check");
    assert!(app_ready < body.find("app.record()").expect("app record"));
}
