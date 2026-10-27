//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::frame_counters_tests::*;
use super::*;

/// The rules are ordered Timeout > Contention > Sync > Streaming, and no predicate after the
/// winner runs: the sync and streaming checks do Acquire loads, and streaming allocates.
#[test]
fn the_winning_deferral_rule_is_counted_and_later_predicates_never_run() {
    let later = std::cell::Cell::new(0_u32);
    let bump = || {
        later.set(later.get() + 1);
        true
    };
    assert_eq!(defer_rule(|| true, bump, bump, bump), Some(DeferRule::Timeout));
    assert_eq!(defer_rule(|| false, || true, bump, bump), Some(DeferRule::Contention));
    assert_eq!(defer_rule(|| false, || false, || true, bump), Some(DeferRule::Sync));
    assert_eq!(later.get(), 0, "no predicate ran after the winner");
    assert_eq!(defer_rule(|| false, || false, || false, || true), Some(DeferRule::Streaming));
    assert_eq!(defer_rule(|| false, || false, || false, || false), None);
}

/// `defer_sync` counts the Sync rule and joins the window record by name between
/// `defer_contention` and `defer_streaming`; a window that never held reports it as zero.
#[test]
fn defer_sync_joins_the_window_record_between_contention_and_streaming() {
    assert_eq!(WindowFrameCounters::default().record(None, 0).count("defer_sync"), Some(0));
    let mut counters = WindowFrameCounters {
        defer_contention: 1,
        defer_streaming: 2,
        ..WindowFrameCounters::default()
    };
    counters.note_defer(DeferRule::Sync);
    counters.note_defer(DeferRule::Sync);
    let record = counters.record(None, 0);
    assert_eq!(record.count("defer_sync"), Some(2));
    let fields = record.line_fields();
    let order: Vec<usize> = ["defer_contention=1", "defer_sync=2", "defer_streaming=2"]
        .iter()
        .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
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
fn a_redraw_takes_each_pending_flush_once_into_flush_to_redraw() {
    // Oldest pending flush to the first redraw that takes it; a second redraw finds nothing,
    // and an empty slot is no observation.
    let slots = [AtomicU64::new(1_000_000), AtomicU64::new(0)];
    let mut counters = WindowFrameCounters::default();
    for now_ns in [5_000_000, 9_000_000] {
        for slot in &slots {
            counters.take_flush(slot, now_ns);
        }
    }
    let flushes = &counters.flush_to_redraw;
    assert_eq!((flushes.count(), flushes.sum_us()), (1, 4_000));
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
    app.note_window_handler(main, Instant::now() - Duration::from_millis(5), true);
    app.request_owner_redraw(main, crate::app::redraw::RedrawCause::Output);
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting window");
    assert_eq!(
        (counters.user_request_redraw, counters.redraw_requested, counters.handler.count()),
        (1, 1, 1)
    );
    // The synthetic window has no native window, so no request reached winit and none counts.
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(snapshot.windows[0].1.count("native_request_redraw"), Some(0));
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
    app.retire_window_counters(main, &mut removed);
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
    let methods = handler_methods(concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    ));
    assert_eq!(methods.len(), 6, "the handler methods changed; review this test");
    for (name, method) in &methods {
        let prints = method.contains("emit_frame_lines(");
        assert_eq!(prints, matches!(name.as_str(), "user_event" | "window_event"), "{name}");
        assert_eq!(method.contains("finish_frame_lines()"), name == "exiting", "{name}");
    }
}

#[test]
fn registration_turns_on_each_counting_windows_renderer() {
    // Every renderer is attached before its window registers, so registration sets its flag.
    let body = source_span(
        concat!(
            include_str!("mod.rs"),
            "\n",
            include_str!("application_handler.rs"),
            "\n",
            include_str!("user_event.rs")
        ),
        "fn insert_window_registered(",
        "\n    }\n",
    )
    .expect("registration");
    assert!(body.contains("renderer.set_frame_counting(true)"), "{body}");
}

#[test]
fn every_window_removal_retires_its_counters() {
    // A removed window's totals move to closed_windows before it drops.
    for (name, source) in [
        ("child_window.rs", to_lf(include_str!("child_window.rs"))),
        ("child_tabs.rs", to_lf(include_str!("child_tabs.rs"))),
        ("session.rs", to_lf(include_str!("session.rs"))),
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
    let body = source_span(source, "pub(super) fn emit_frame_lines_at(", "\n    }\n")
        .expect("emit_frame_lines_at");
    let window_ready = body.find("counters.line.ready(now)").expect("window readiness check");
    assert!(window_ready < body.find("counters.record(").expect("window record"));
    let app_ready = body.find("app.line.ready(now)").expect("app readiness check");
    assert!(app_ready < body.find("app.record()").expect("app record"));
}

#[test]
fn refused_redraws_after_the_line_interval_build_no_record() {
    // A RedrawRequested arrives every frame; once the second has passed, a redraw with no new
    // attempt or flush is refused before anything is built, however many arrive.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let start = Instant::now();
    let counters = |app: &App| -> u64 {
        app.windows[&main].redraw.frame_counters.as_deref().expect("counting").records_built
    };
    app.windows.get_mut(&main).unwrap().redraw.frame_counters.as_deref_mut().unwrap().attempts += 1;
    app.emit_frame_lines_at(Some((main, true)), start);
    assert_eq!(counters(&app), 1, "the attempt authorized one line");
    for second in 2..6 {
        app.emit_frame_lines_at(Some((main, true)), start + Duration::from_secs(second));
    }
    assert_eq!(counters(&app), 1, "refused redraws after expiry built no record");
}

#[test]
fn a_first_interval_with_no_activity_authorizes_no_redraw_line() {
    // A field that was never printed and is still zero has not grown.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    app.emit_frame_lines_at(Some((main, true)), Instant::now() + Duration::from_secs(2));
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting");
    assert_eq!(counters.records_built, 0, "a zero-activity redraw built a record");
}

#[test]
fn the_gate_seal_is_plain_event_loop_state() {
    // Pane creation runs with the gate off too, so sealing must not be an atomic store.
    let app = app_under_filter("warn");
    let seal = std::any::type_name_of_val(&app.frame_counters_sealed);
    assert_eq!(seal, std::any::type_name::<std::cell::Cell<bool>>());
}

#[test]
fn with_the_gate_off_the_production_hooks_write_nothing() {
    // The hooks the event loop calls on every dispatch read no clock, attach no counters and
    // write no tally when the App's gate is off; only the seal is set.
    let mut app = app_under_filter("warn");
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    {
        let _dispatch = app.frame_dispatch_scope();
        assert!(app.frame_clock_start().is_none(), "no clock read");
        app.note_frame_wake(&winit::event::StartCause::Poll);
        app.note_frame_user_wake();
        app.note_frame_dispatch(DispatchKind::AboutToWait, None);
        app.note_redraw_requested(main);
        app.note_user_request_redraw(main);
        assert!(!app.begin_window_handler(main));
        app.note_window_handler(main, Instant::now(), false);
        let parser = Mutex::new(0_u8);
        drop(lock_parser(&parser));
        app.emit_frame_lines(Some((main, true)));
    }
    assert_eq!(current_tally(), (0, 0), "no tally written");
    assert!(app.pane_frame_counters().is_none());
    assert!(app.windows[&main].redraw.frame_counters.is_none());
    assert!(app.windows[&main].panes.values().all(|pane| pane.frame_counters.is_none()));
    assert!(app.frame_counters_snapshot().is_none());
    assert_eq!(app.force_frame_counters_on(), Err(FrameCountersTooLate), "sealed");
}

#[test]
fn a_tab_shows_its_leaves_or_only_its_zoomed_pane() {
    // The walk visits in place, allocating nothing, and zoom hides every other leaf.
    use sonicterm_cfg::keymap::Direction;
    use sonicterm_ui::pane::PaneTree;
    let shown = |tree: &PaneTree| {
        let mut visited = Vec::new();
        for_each_shown_pane(tree, &mut |pane_id| visited.push(pane_id));
        visited
    };
    let mut tree = PaneTree::leaf(1);
    assert_eq!(shown(&tree), vec![1]);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.split(2, Direction::Down, 3));
    assert_eq!(shown(&tree), vec![1, 2, 3]);
    assert!(tree.toggle_zoom(2));
    assert_eq!(shown(&tree), vec![2]);
}

#[test]
fn renderer_work_counters_join_the_window_record_with_assembly_in_us_buckets() {
    // The renderer work counters, glyph atlas growth counts included, reach the window's line,
    // snapshot and closed totals; assembly is a microsecond histogram and growth-to-present a
    // millisecond one, each with the App's own bounds and an exact sum.
    use sonicterm_gpu::frame_stats::{FrameStats, ASSEMBLY_BOUNDS_US};
    assert_eq!(ASSEMBLY_BOUNDS_US, MICROS_BOUNDS);
    let mut stats = FrameStats::ZERO;
    stats.full_frames = 2;
    stats.row_cache_invalidate_visits = 40;
    stats.row_cache_invalidate_us = 900;
    stats.recolor_glyphs_visited = 120;
    stats.font_fallback_applies = 3;
    stats.assembly_buckets[2] = 1;
    stats.assembly_buckets[6] = 1;
    stats.assembly_sum_us = 6_080;
    stats.glyph_atlas_growths = 2;
    stats.atlas_growth_abandoned = 1;
    stats.atlas_growth_to_present_buckets[1] = 1;
    stats.atlas_growth_to_present_buckets[9] = 1;
    stats.atlas_growth_to_present_sum_us = 130_000;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in [
        ("full_frames", 2),
        ("row_cache_invalidate_visits", 40),
        ("row_cache_invalidate_us", 900),
        ("recolor_glyphs_visited", 120),
        ("font_fallback_applies", 3),
        ("glyph_atlas_growths", 2),
        ("atlas_growth_abandoned", 1),
    ] {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
    // Growth-to-present time is a millisecond histogram on the App's frame bounds.
    use sonicterm_gpu::frame_stats::GROWTH_TO_PRESENT_BOUNDS_MS;
    assert_eq!(GROWTH_TO_PRESENT_BOUNDS_MS, MILLIS_BOUNDS);
    let growth = record.histogram_buckets("atlas_growth_to_present").expect("growth histogram");
    assert_eq!((growth.unit, growth.bounds, growth.sum_us), ("ms", &MILLIS_BOUNDS[..], 130_000));
    assert_eq!(growth.counts, &[0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
    let assembly = record.histogram_buckets("assembly").expect("assembly histogram");
    assert_eq!(
        (assembly.unit, assembly.bounds, assembly.sum_us),
        ("us", &MICROS_BOUNDS[..], 6_080)
    );
    assert_eq!(assembly.counts, &[0, 0, 1, 0, 0, 0, 1]);
    let fields = record.line_fields();
    assert!(
        fields.contains("full_frames=2") && fields.contains("assembly_us=[0,0,1,0,0,0,1]"),
        "{fields}"
    );
}

#[test]
fn attempt_and_preparation_counters_leave_the_renderer_each_under_its_own_name() {
    // Every attempt, apply-attempt and preparation field reaches the window record with its own
    // value, so a field wired to the wrong source, or left out, shows as a mismatch here.
    use sonicterm_gpu::frame_stats::{AttemptStats, FrameStats};
    let attempt = |base: u64| AttemptStats {
        attempts: base,
        presented: base + 1,
        attempt_ns: base + 2,
        shape_ns: base + 3,
        raster_ns: base + 4,
        shape_requests: base + 5,
        raster_calls: base + 6,
        raster_tiles: base + 7,
    };
    let mut stats = FrameStats::ZERO;
    stats.shape_ns = 1;
    stats.raster_ns = 2;
    stats.raster_calls = 3;
    stats.raster_tiles = 4;
    stats.font_generation_applies = 5;
    stats.font_prepare_ns = 6;
    stats.font_generation_prepare_ns = 7;
    stats.attempts = attempt(10);
    stats.apply_attempts = attempt(20);
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    let mut expected = vec![
        ("shape_ns", 1),
        ("raster_ns", 2),
        ("raster_calls", 3),
        ("raster_tiles", 4),
        ("font_generation_applies", 5),
        ("font_prepare_ns", 6),
        ("font_generation_prepare_ns", 7),
    ];
    for (prefix, base) in [("render_", 10), ("apply_", 20)] {
        for (offset, suffix) in [
            "attempts",
            "attempts_presented",
            "attempt_ns",
            "attempt_shape_ns",
            "attempt_raster_ns",
            "attempt_shape_requests",
            "attempt_raster_calls",
            "attempt_raster_tiles",
        ]
        .into_iter()
        .enumerate()
        {
            let name: &'static str = Box::leak(format!("{prefix}{suffix}").into_boxed_str());
            expected.push((name, base + offset as u64));
        }
    }
    for (name, value) in expected {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
}

#[test]
fn stream_clock_exempt_joins_the_window_record_after_defer_streaming() {
    // The harness and perf-compare read the new window count by name, in the record's order.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    let counters = WindowFrameCounters {
        defer_streaming: 2,
        stream_clock_exempt: 4,
        contention_retry_armed: 1,
        ..WindowFrameCounters::default()
    };
    let record = counters.record(None, 0);
    assert_eq!(record.count("stream_clock_exempt"), Some(4));
    let fields = record.line_fields();
    let defer = fields.find("defer_streaming=").expect("defer_streaming field");
    let exempt = fields.find("stream_clock_exempt=4").expect("stream_clock_exempt field");
    let armed = fields.find("contention_retry_armed=").expect("contention_retry_armed field");
    assert!(defer < exempt && exempt < armed, "{fields}");
}

/// The three display-link counts join the window record by name, after `stream_clock_exempt` and
/// before `contention_retry_armed`, so the harness and perf-compare read them in contract order. A
/// line omits zero counts, so every neighbour is non-zero to show the order.
#[test]
fn display_link_counts_join_the_window_record_after_stream_clock_exempt() {
    let counters = WindowFrameCounters {
        stream_clock_exempt: 1,
        display_link_ticks: 5,
        display_link_admissions: 3,
        display_link_fallbacks: 2,
        contention_retry_armed: 1,
        ..WindowFrameCounters::default()
    };
    let record = counters.record(None, 0);
    assert_eq!(record.count("display_link_ticks"), Some(5));
    assert_eq!(record.count("display_link_admissions"), Some(3));
    assert_eq!(record.count("display_link_fallbacks"), Some(2));
    let fields = record.line_fields();
    let order: Vec<usize> = [
        "stream_clock_exempt=",
        "display_link_ticks=5",
        "display_link_admissions=3",
        "display_link_fallbacks=2",
        "contention_retry_armed=",
    ]
    .iter()
    .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
    // A window that never ran a link still reports all three, as zero.
    let idle = WindowFrameCounters::default().record(None, 0);
    for name in ["display_link_ticks", "display_link_admissions", "display_link_fallbacks"] {
        assert_eq!(idle.count(name), Some(0), "{name}");
    }
}

#[test]
fn damage_waste_joins_the_renderer_record_after_damaged_frames() {
    // The harness and perf-compare read the union-rect waste count by name, beside its denominator.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    use sonicterm_gpu::frame_stats::FrameStats;
    let mut stats = FrameStats::ZERO;
    stats.damaged_frames = 3;
    stats.damage_waste_permille_sum = 37;
    stats.software_frames = 1;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    assert_eq!(record.count("damage_waste_permille_sum"), Some(37));
    let fields = record.line_fields();
    let damaged = fields.find("damaged_frames=3").expect("damaged_frames field");
    let waste =
        fields.find("damage_waste_permille_sum=37").expect("damage_waste_permille_sum field");
    let software = fields.find("software_frames=1").expect("software_frames field");
    assert!(damaged < waste && waste < software, "{fields}");
}

/// `sync_timeouts` joins the App record by name, after `flushes_suppressed` and before
/// `ui_parser_locks`, so the harness and perf-compare read it in contract order; an App whose
/// workers never timed out a synchronized update still reports it, as zero.
#[test]
fn sync_timeouts_joins_the_app_record_after_flushes_suppressed() {
    let counters = AppFrameCounters::new();
    assert_eq!(counters.record().count("sync_timeouts"), Some(0), "supported zero");
    counters.vt.flushes_suppressed.fetch_add(2, Ordering::Relaxed);
    counters.vt.sync_timeouts.fetch_add(3, Ordering::Relaxed);
    counters.dispatch.locks.fetch_add(1, Ordering::Relaxed);
    let record = counters.record();
    assert_eq!(record.count("sync_timeouts"), Some(3));
    let fields = record.line_fields();
    let order: Vec<usize> = ["flushes_suppressed=2", "sync_timeouts=3", "ui_parser_locks=1"]
        .iter()
        .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
}

#[test]
fn partial_counters_join_the_renderer_record_after_full_frames() {
    // The harness and perf-compare read the partial-assembly counters by name, after full_frames.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    use sonicterm_gpu::frame_stats::FrameStats;
    let mut stats = FrameStats::ZERO;
    stats.full_frames = 2;
    stats.partial_frames = 5;
    stats.partial_fallbacks = 1;
    stats.row_cells_hashed = 400;
    stats.row_cache_invalidate_visits = 3;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in
        [("partial_frames", 5), ("partial_fallbacks", 1), ("row_cells_hashed", 400)]
    {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
    let fields = record.line_fields();
    let order: Vec<usize> = [
        "full_frames=2",
        "partial_frames=5",
        "partial_fallbacks=1",
        "row_cells_hashed=400",
        "row_cache_invalidate_visits=3",
    ]
    .iter()
    .map(|field| fields.find(field).unwrap_or_else(|| panic!("{field}: {fields}")))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
}

#[test]
fn title_and_chrome_run_counters_leave_the_renderer_under_their_own_names() {
    // The four reuse and prepare counters reach the window record each from its own field, so a
    // swapped or missing wire shows as a mismatch here.
    let mut stats = sonicterm_gpu::frame_stats::FrameStats::ZERO;
    stats.tab_title_reuses = 11;
    stats.tab_title_prepares = 12;
    stats.chrome_run_reuses = 13;
    stats.chrome_run_prepares = 14;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in [
        ("tab_title_reuses", 11),
        ("tab_title_prepares", 12),
        ("chrome_run_reuses", 13),
        ("chrome_run_prepares", 14),
    ] {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
}

/// Each section also adds its lock wait to a nanosecond total and counts once, so a phase reads the
/// workers' summed wait and the sections it covers as differences of two snapshots.
#[test]
fn a_section_adds_its_wait_to_the_nanosecond_total_and_counts_once() {
    let stats = VtFrameStats::default();
    let start = Instant::now();
    for wait_us in [20, 5] {
        let locked_at = after_us(start, wait_us);
        let times = VtSectionTimes {
            before_lock: start,
            locked_at,
            parsed_at: locked_at,
            released_at: locked_at,
        };
        stats.record_section(&times, 1);
    }
    assert_eq!(stats.parser_lock_wait_ns.load(Ordering::Relaxed), 25_000);
    assert_eq!(stats.parser_sections.load(Ordering::Relaxed), 2);
}

/// The App record carries the worker-wait totals and the guard-custody and frame-dispatch totals by
/// name, each a supported zero before any frame, so a head always reports them.
#[test]
fn the_app_record_names_the_guard_and_wait_totals() {
    let counters = AppFrameCounters::new();
    let names = [
        "parser_lock_wait_ns",
        "parser_sections",
        "ui_guard_custody_ns",
        "ui_guard_custodies",
        "frame_dispatch_ns",
        "frame_dispatches",
        "frame_dispatch_failed_ns",
        "frame_dispatches_failed",
    ];
    for name in names {
        assert_eq!(counters.record().count(name), Some(0), "{name}");
    }
    counters.custody.dispatches_failed.fetch_add(4, Ordering::Relaxed);
    counters.vt.parser_sections.fetch_add(7, Ordering::Relaxed);
    let record = counters.record();
    assert_eq!(
        (record.count("frame_dispatches_failed"), record.count("parser_sections")),
        (Some(4), Some(7))
    );
}

/// D′: both activation paths initialize the run clock's epoch through the counters' constructor. A fresh,
/// injected cell is checked without initializing it: an App built with the gate off leaves it unset, the
/// tracing-enabled App sets it while it is built, and forcing counters on sets it at that call.
#[test]
fn both_activation_paths_set_the_epoch_cell_at_construction() {
    let check = |activate: &dyn Fn() -> bool| {
        let cell: &'static OnceLock<Instant> = Box::leak(Box::new(OnceLock::new()));
        CONSTRUCTOR_EPOCH_CELL.with(|held| held.set(Some(cell)));
        let counting = activate();
        CONSTRUCTOR_EPOCH_CELL.with(|held| held.set(None));
        (counting, cell.get().is_some())
    };
    assert_eq!(check(&|| app_under_filter("warn").frame_counters.is_some()), (false, false));
    assert_eq!(
        check(&|| app_under_filter("warn,frame_counters=debug").frame_counters.is_some()),
        (true, true)
    );
    let forced = check(&|| {
        let mut app = app_under_filter("warn");
        let before =
            CONSTRUCTOR_EPOCH_CELL.with(|held| held.get().is_some_and(|cell| cell.get().is_some()));
        app.force_frame_counters_on().expect("no window yet");
        !before && app.frame_counters.is_some()
    });
    assert_eq!(forced, (true, true), "forcing counters on sets the cell at that call, not before");
}
