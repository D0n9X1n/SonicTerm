//! Parser deadline, contention and guard-release contracts.

use super::super::visible_frame_tests::*;
use super::*;

/// A counting collection starts its custody and dispatch at the first guard acquired. A frame that renders
/// records one custody and one rendered dispatch; contention after a guard records one custody and one
/// failed dispatch; contention before any guard records nothing; an App that does not count takes neither.
#[test]
fn a_counting_collection_times_custody_from_its_first_guard() {
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        let totals = Arc::new(crate::app::guard_custody::CustodyTotals::default());
        let mut counted = sources(&mut app, window, child).ok().unwrap();
        counted.counted = Some(Arc::clone(&totals));
        let held = counted.try_collect(|| {}).ok().unwrap();
        let (custody, dispatch) = (held.custody, held.dispatch);
        assert!(custody.is_some() && dispatch.is_some(), "child={child}");
        drop(held.guards);
        drop(custody);
        dispatch.unwrap().rendered();
        assert_eq!(custody_counts(&totals), (1, 1, 0), "child={child}: a rendered frame");
        let blocked = Arc::clone(&app.windows[&window].panes[&right].parser);
        let guard = blocked.lock();
        assert!(counted.try_collect(|| {}).is_err());
        assert_eq!(
            custody_counts(&totals),
            (2, 1, 1),
            "child={child}: contention after the first guard"
        );
        drop(guard);
        let first = Arc::clone(&app.windows[&window].panes[&left].parser);
        let guard = first.lock();
        assert!(counted.try_collect(|| {}).is_err());
        assert_eq!(custody_counts(&totals), (2, 1, 1), "child={child}: no guard, nothing recorded");
        drop(guard);
        let uncounted = sources(&mut app, window, child).ok().unwrap();
        let held = uncounted.try_collect(|| {}).ok().unwrap();
        assert!(held.custody.is_none() && held.dispatch.is_none(), "child={child}: not counting");
    }
}

/// The held source closes the custody after its guards, and every early exit of a redraw
/// releases the guards before the custody and dispatch timing close (read as LF on any checkout).
#[test]
fn every_collection_exit_releases_guards_before_closing_the_timing() {
    let collector = concat!(
        include_str!("visible_frame.rs"),
        "\n",
        include_str!("visible_frame_acquire.rs"),
        "\n",
        include_str!("visible_frame_layout.rs"),
        "\n",
        include_str!("visible_frame_receipts.rs")
    )
    .replace("\r\n", "\n");
    let collector: String = collector.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    assert!(
        collector.contains("drop(panes);\ndrop(self.guards);\ndrop(self.custody);"),
        "lend: the custody closes only after every guard is released"
    );
    for (name, source) in [
        (
            "main",
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
        ),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let source = source.replace("\r\n", "\n");
        let compact: String = source.lines().map(str::trim).collect::<Vec<_>>().join("\n");
        // The sync hold, the reconcile failure, the no-renderer exit and the post-render release.
        assert!(
            compact.matches("drop(frame);").count() >= 4,
            "{name}: every exit drops the whole frame"
        );
        assert!(
            compact.contains(
                "guards: std::mem::take(&mut frame.guards),\ncustody: frame.custody.take(),"
            ),
            "{name}: the render source carries the guards, then the custody"
        );
        assert!(
            compact.contains(
                "let dispatch = frame.dispatch.take();\nlet returned_at =\n(render_marker.is_some() || dispatch.is_some()).then(Instant::now);"
            ),
            "{name}: a render closes the rendered population"
        );
    }
}

/// Both collectors hand the App's own custody totals to the sources when the App counts, and none when
/// it does not, so production frames reach the exported totals.
#[test]
fn the_collectors_count_into_the_apps_custody_totals_only_when_counting() {
    for child in [false, true] {
        let (mut app, window, _, _, _) = fixture(child, false);
        assert!(sources(&mut app, window, child).ok().unwrap().counted.is_none(), "child={child}");
        app.frame_counters = Some(crate::app::frame_counters::AppFrameCounters::new());
        let counted = sources(&mut app, window, child).ok().unwrap().counted;
        let owned = &app.frame_counters.as_ref().unwrap().custody;
        assert!(counted.is_some_and(|totals| Arc::ptr_eq(&totals, owned)), "child={child}");
    }
}

/// A panic inside a counting collection, after every parser guard is taken, unwinds through the guards before
/// the custody records, so `ui_guard_custody_ns` never ends while a guard is still held.
#[test]
fn an_unwinding_collection_records_custody_only_after_every_guard_is_released() {
    use crate::app::guard_custody::{CustodyTotals, CUSTODY_DROP_PROBE};
    use std::cell::RefCell;
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        let parsers: Vec<_> = [left, right]
            .iter()
            .map(|id| Arc::clone(&app.windows[&window].panes[id].parser))
            .collect();
        // Each entry is whether every visible parser could be locked when a custody recorded.
        let guards_free_at_record = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = std::rc::Rc::clone(&guards_free_at_record);
        CUSTODY_DROP_PROBE.with(|probe| {
            *probe.borrow_mut() = Some(Box::new(move || {
                let free = parsers.iter().all(|parser| parser.try_lock().is_some());
                observed.borrow_mut().push(free);
            }));
        });
        let totals = Arc::new(CustodyTotals::default());
        let mut counted = sources(&mut app, window, child).ok().unwrap();
        counted.counted = Some(Arc::clone(&totals));
        // Holding this borrow makes the image loop's test-only `borrow_mut` panic after every guard is held.
        let visits = counted.image_visits.borrow();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = counted.try_collect(|| {});
        }));
        drop(visits);
        CUSTODY_DROP_PROBE.with(|probe| probe.borrow_mut().take());
        assert!(unwound.is_err(), "child={child}: the collection must unwind");
        assert_eq!(*guards_free_at_record.borrow(), [true], "child={child}");
        assert_eq!(custody_counts(&totals), (1, 0, 1), "child={child}: recorded once, as failed");
    }
}

/// Both redraw adapters keep the collected frame as one owning binding and move only its snapshot out, so
/// an unwind between collection and render drops every guard before the custody and dispatch clock.
#[test]
fn the_redraw_adapters_keep_the_collected_frame_whole() {
    for (name, source) in [
        (
            "main",
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
        ),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let source = source.replace("\r\n", "\n");
        let compact: String = source.lines().map(str::trim).collect::<Vec<_>>().join("\n");
        assert!(
            !compact.contains("let super::visible_frame::HeldVisibleFrame {"),
            "{name}: never destructured"
        );
        assert!(compact.contains("let mut frame = {"), "{name}: one owning binding");
        assert!(
            compact.contains("let frame_snapshot = frame.snapshot.take();"),
            "{name}: the snapshot is taken"
        );
        // Each timing field leaves the frame only by `take` (once, at render); an early exit drops the whole
        // frame, so no field is ever a separate local that could drop out of order.
        for field in ["frame.custody", "frame.dispatch"] {
            let uses = compact.matches(field).count();
            assert_eq!(
                uses,
                compact.matches(&format!("{field}.take()")).count(),
                "{name}: {field}"
            );
        }
        // The guards move out only into the render source; every other use borrows them.
        for moved in
            ["drop(frame.guards)", "= frame.guards;", "guards: frame.guards,", "drop(frame.images)"]
        {
            assert!(!compact.contains(moved), "{name}: {moved}");
        }
        assert_eq!(compact.matches("frame.custody.take()").count(), 1, "{name}: custody");
        assert_eq!(compact.matches("frame.dispatch.take()").count(), 1, "{name}: dispatch");
        assert_eq!(
            compact.matches("std::mem::take(&mut frame.guards)").count(),
            1,
            "{name}: guards"
        );
    }
}

/// A frame held the way the adapters hold it (snapshot moved out, guards borrowed mutably) that unwinds
/// before render drops its guards before the custody records: the field order is the drop order.
#[test]
fn an_unwind_after_collection_releases_the_held_frames_guards_first() {
    use crate::app::guard_custody::{CustodyTotals, CUSTODY_DROP_PROBE};
    use std::cell::RefCell;
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        let parsers: Vec<_> = [left, right]
            .iter()
            .map(|id| Arc::clone(&app.windows[&window].panes[id].parser))
            .collect();
        let guards_free_at_record = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = std::rc::Rc::clone(&guards_free_at_record);
        CUSTODY_DROP_PROBE.with(|probe| {
            *probe.borrow_mut() = Some(Box::new(move || {
                let free = parsers.iter().all(|parser| parser.try_lock().is_some());
                observed.borrow_mut().push(free);
            }));
        });
        let totals = Arc::new(CustodyTotals::default());
        let mut counted = sources(&mut app, window, child).ok().unwrap();
        counted.counted = Some(Arc::clone(&totals));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut frame = counted.try_collect(|| Some(())).ok().unwrap();
            let _snapshot = frame.snapshot.take();
            // The adapters borrow the guards mutably (receipts, viewports) while the frame still owns them.
            let held = &mut frame.guards;
            assert_eq!(held.len(), 2);
            panic!("unwind between collection and render");
        }));
        CUSTODY_DROP_PROBE.with(|probe| probe.borrow_mut().take());
        assert!(unwound.is_err(), "child={child}");
        assert_eq!(*guards_free_at_record.borrow(), [true], "child={child}");
        assert_eq!(custody_counts(&totals), (1, 0, 1), "child={child}: recorded once, as failed");
    }
}

/// C′ through the collector: a counting collection records one span per guard, and the custody's single
/// release read ends the scalar and every span at the same instant; contention after the first guard
/// still closes that guard's span after it is released.
#[test]
fn a_counting_collection_records_spans_ending_at_the_custody_release() {
    use crate::app::guard_correlation::{GuardCorrelation, GuardCorrelationTakeV1, CLOCK_OVERRIDE};
    let epoch = crate::app::frame_counters::clock_epoch();
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        let totals = Arc::new(crate::app::guard_custody::CustodyTotals::default());
        let mut correlation = GuardCorrelation::new();
        let mut counted = sources(&mut app, window, child).ok().unwrap();
        counted.counted = Some(Arc::clone(&totals));
        counted.spans = Some(std::rc::Rc::clone(&correlation.spans));
        let mut tick_ns = 0_u64;
        CLOCK_OVERRIDE.with(|clock| {
            *clock.borrow_mut() = Some(Box::new(move || {
                tick_ns += 1_000;
                epoch + Duration::from_nanos(tick_ns)
            }));
        });
        let held = counted.try_collect(|| {}).ok().unwrap();
        drop(held.guards);
        drop(held.custody);
        held.dispatch.unwrap().rendered();
        let blocked = Arc::clone(&app.windows[&window].panes[&right].parser);
        let guard = blocked.lock();
        assert!(counted.try_collect(|| {}).is_err());
        drop(guard);
        CLOCK_OVERRIDE.with(|clock| *clock.borrow_mut() = None);
        let GuardCorrelationTakeV1::Taken(taken) = correlation.take() else {
            panic!("child={child}: taken")
        };
        let spans = taken.spans.spans;
        let summary: Vec<(u64, u64, u64, u64)> = spans
            .iter()
            .map(|span| (span.pane_id, span.collection_seq, span.acquired_ns, span.released_ns))
            .collect();
        // Reads: two acquisitions and the release of the first collection, then one acquisition and the
        // release of the contended one.
        let mut panes = [left, right];
        panes.sort_unstable();
        assert_eq!(summary.len(), 3, "child={child}: {summary:?}");
        assert!(
            summary[..2].iter().all(|span| span.1 == 1 && span.3 == 3_000),
            "child={child}: {summary:?}"
        );
        assert_eq!(summary[2].1, 2, "child={child}: the contended collection");
        assert_eq!(
            (summary[2].2, summary[2].3),
            (4_000, 5_000),
            "child={child}: closed after its release"
        );
        let mut seen: Vec<u64> = summary[..2].iter().map(|span| span.0).collect();
        seen.sort_unstable();
        assert_eq!(seen, panes, "child={child}: one span per guarded pane");
        assert_eq!(
            (taken.spans.spans_issued, taken.spans.collections_issued),
            (3, 2),
            "child={child}"
        );
        assert_eq!(taken.spans.open_collections, 0, "child={child}");
        assert_eq!(custody_counts(&totals), (2, 1, 1), "child={child}");
    }
}

/// The production connection: with the App's own counters, the main and child collectors' sources carry the
/// App's span store, so a collection's guards reach `App::take_guard_correlation_v1` as one span per pane,
/// with no store installed by hand.
#[test]
fn production_sources_record_spans_into_the_apps_take() {
    use crate::app::guard_correlation::GuardCorrelationTakeV1;
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        app.frame_counters = Some(crate::app::frame_counters::AppFrameCounters::new());
        {
            // The production sources, collected and released as a redraw does; they end with this block.
            let collected = sources(&mut app, window, child).ok().unwrap();
            let held = collected.try_collect(|| {}).ok().unwrap();
            drop(held.guards);
            drop(held.custody);
            held.dispatch.unwrap().rendered();
        }
        let GuardCorrelationTakeV1::Taken(taken) = app.take_guard_correlation_v1() else {
            panic!("child={child}: a counting App takes");
        };
        let mut pane_ids: Vec<u64> = taken.spans.spans.iter().map(|span| span.pane_id).collect();
        pane_ids.sort_unstable();
        let mut expected = vec![left, right];
        expected.sort_unstable();
        assert_eq!(pane_ids, expected, "child={child}: one span per guarded pane");
        let first = taken.spans.spans[0];
        assert!(
            taken.spans.spans.iter().all(|span| span.collection_seq == 1
                && span.released_ns == first.released_ns
                && span.acquired_ns <= span.released_ns),
            "child={child}: {:?}",
            taken.spans.spans
        );
        assert_eq!(
            (taken.take_seq, taken.spans.spans_issued, taken.spans.collections_issued),
            (1, 2, 1),
            "child={child}"
        );
        assert_eq!(taken.spans.open_collections, 0, "child={child}");
    }
}

/// The gate-off counterpart: an App without counters gives its sources no span store, and building, collecting
/// and taking read no correlation clock and allocate no recording buffer; the take answers GateOff.
#[test]
fn gate_off_sources_take_no_correlation_clock_or_recorder() {
    use crate::app::guard_correlation::{
        GuardCorrelationTakeV1, CLOCK_READS, RECORDER_ALLOCATIONS,
    };
    for child in [false, true] {
        let reads_before = CLOCK_READS.with(std::cell::Cell::get);
        let recorders_before = RECORDER_ALLOCATIONS.with(std::cell::Cell::get);
        let (mut app, window, _, _, _) = fixture(child, false);
        let collected = sources(&mut app, window, child).ok().unwrap();
        assert!(collected.spans.is_none(), "child={child}: no span store without counters");
        let held = collected.try_collect(|| {}).ok().unwrap();
        drop(held);
        drop(collected);
        assert!(
            matches!(app.take_guard_correlation_v1(), GuardCorrelationTakeV1::GateOff),
            "child={child}"
        );
        assert_eq!(
            CLOCK_READS.with(std::cell::Cell::get),
            reads_before,
            "child={child}: a clock was read"
        );
        assert_eq!(
            RECORDER_ALLOCATIONS.with(std::cell::Cell::get),
            recorders_before,
            "child={child}: a recording buffer was allocated"
        );
    }
}

/// A later pane is offered only what remains of the one absolute deadline, never a fresh budget.
#[test]
fn every_visible_parser_waits_against_one_absolute_collection_deadline() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = sources(&mut app, window, false).ok().unwrap();
    let budget = Duration::from_micros(2_000);
    let acquire = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::BusyThenFree { wait: Duration::from_micros(1_500) }),
        (right, ScriptedOutcome::BusyThenFree { wait: Duration::from_micros(100) }),
    ]);
    let start = acquire.now();
    let Ok(held) = sources.try_collect_with(&acquire, budget, || {}) else {
        panic!("both panes were freed within the shared deadline");
    };
    assert_eq!(held.guards.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(), vec![left, right]);
    let calls = acquire.calls.borrow().clone();
    // Both waits carry the same deadline; the second starts 1.5 ms in, so it has 0.5 ms left.
    assert_eq!(
        calls,
        vec![
            AcquireCall::Probe(left),
            AcquireCall::Wait { pane_id: left, deadline: start + budget, at: start },
            AcquireCall::Probe(right),
            AcquireCall::Wait {
                pane_id: right,
                deadline: start + budget,
                at: start + Duration::from_micros(1_500),
            },
        ]
    );
}

/// A parser still busy at the deadline is a miss: every guard already taken is released, and no
/// acquisition is attempted for a later pane once the deadline is observed to have passed.
#[test]
fn a_parser_busy_through_the_deadline_is_contended_with_every_guard_released() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = sources(&mut app, window, false).ok().unwrap();
    let budget = Duration::from_micros(2_000);
    // The first pane uses the whole budget, so the second is never probed.
    let exhausted = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::BusyThroughout { wait: budget }),
        (right, ScriptedOutcome::Free),
    ]);
    let outcome = sources.try_collect_with(&exhausted, budget, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: left, images: false }));
    assert!(!exhausted.calls.borrow().contains(&AcquireCall::Probe(right)), "no call after expiry");
    // The first pane is free and taken; the second stays busy past the deadline, so the first guard is released.
    let second_busy = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::Free),
        (right, ScriptedOutcome::BusyThroughout { wait: budget }),
    ]);
    let outcome = sources.try_collect_with(&second_busy, budget, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: right, images: false }));
    let state = &app.windows[&window];
    assert!(state.panes[&left].parser.try_lock().is_some(), "the earlier guard was released");
    assert!(state.panes[&right].parser.try_lock().is_some());
    assert!(sources.image_visits.borrow().is_empty(), "every parser is taken before any image");
}

/// A guard that arrives after the deadline, on either acquisition path, is dropped and reported as
/// contention rather than lengthening the collection.
#[test]
fn a_guard_obtained_after_the_deadline_is_released_and_counts_as_a_miss() {
    let (mut app, window, left, right, _inactive) = fixture(false, false);
    let sources = sources(&mut app, window, false).ok().unwrap();
    let budget = Duration::from_micros(2_000);
    // The timed wait returns the guard, but only after the clock passed the deadline.
    let late = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::Free),
        (right, ScriptedOutcome::BusyThenFree { wait: Duration::from_micros(2_500) }),
    ]);
    let outcome = sources.try_collect_with(&late, budget, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: right, images: false }));
    let state = &app.windows[&window];
    assert!(state.panes[&left].parser.try_lock().is_some(), "the earlier guard was released");
    assert!(state.panes[&right].parser.try_lock().is_some(), "the late guard was released");
    // The nonwaiting probe succeeds, but only after the deadline: that guard is released too.
    let late_probe = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::Free),
        (right, ScriptedOutcome::FreeLate { late: Duration::from_micros(2_500) }),
    ]);
    let outcome = sources.try_collect_with(&late_probe, budget, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: right, images: false }));
    let state = &app.windows[&window];
    assert!(state.panes[&left].parser.try_lock().is_some(), "the earlier guard was released");
    assert!(state.panes[&right].parser.try_lock().is_some(), "the late probe's guard was released");
    assert!(sources.image_visits.borrow().is_empty(), "no image is visited after a late guard");
    // A zero budget expires before the first probe: a free parser is still not taken.
    let expired =
        ScriptedAcquire::new(&[(left, ScriptedOutcome::Free), (right, ScriptedOutcome::Free)]);
    let outcome = sources.try_collect_with(&expired, Duration::ZERO, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: left, images: false }));
    assert!(expired.calls.borrow().is_empty(), "no acquisition is attempted after expiry");
    // The deadline passes while the hook after a failed probe runs: the timed wait is never entered.
    let during_hook = ScriptedAcquire::new(&[
        (left, ScriptedOutcome::BusyThenFree { wait: Duration::ZERO }),
        (right, ScriptedOutcome::Free),
    ]);
    during_hook.hook_advance.set(budget);
    let outcome = sources.try_collect_with(&during_hook, budget, || {});
    assert_eq!(outcome.err(), Some(FrameUnavailable::Contended { pane_id: left, images: false }));
    assert_eq!(*during_hook.calls.borrow(), vec![AcquireCall::Probe(left)], "no wait after expiry");
}

/// The deterministic red/green pin: a probe that is known to fail, because another thread owns the
/// visible parser, is followed by the timed wait, which takes the parser once it is released. Returning
/// the failed probe instead would yield `Contended` here whatever the scheduling. Both roles are covered.
#[test]
fn a_failed_probe_falls_through_to_the_timed_wait_that_takes_the_released_parser() {
    for child in [false, true] {
        let (mut app, window, _left, right, _inactive) = fixture(child, false);
        let parser = Arc::clone(&app.windows[&window].panes[&right].parser);
        let sources = sources(&mut app, window, child).ok().unwrap();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let guard = parser.lock();
            held_tx.send(()).unwrap();
            // Released only on the hook's signal, which comes after the collector's probe failed.
            let _ = release_rx.recv();
            drop(guard);
        });
        held_rx.recv().unwrap();
        let acquire = ReleaseAfterFailedProbe {
            release: release_tx.clone(),
            failed_probes: Default::default(),
        };
        // The five-second budget is a watchdog for a stalled host, not a timing assertion.
        let outcome = sources.try_collect_with(&acquire, Duration::from_secs(5), || {});
        let collected = outcome.is_ok();
        let why = outcome.err();
        // Always release the holder, even when the collector returned before its hook ran.
        let _ = release_tx.send(());
        holder.join().unwrap();
        assert_eq!(acquire.failed_probes.get(), 1, "the held parser's probe failed exactly once");
        assert!(collected, "the timed wait takes the released parser, got {why:?}");
    }
}

/// The production collector takes every parser through the deadline-checked timed helper, and the VT
/// worker releases each section's parser fairly. These pins support the behaviour tests above.
#[test]
fn collection_waits_with_a_deadline_and_the_worker_releases_fairly() {
    let strip = |source: &str| -> String {
        source
            .replace("\r\n", "\n")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let collector = strip(concat!(
        include_str!("visible_frame.rs"),
        "\n",
        include_str!("visible_frame_acquire.rs"),
        "\n",
        include_str!("visible_frame_layout.rs"),
        "\n",
        include_str!("visible_frame_receipts.rs")
    ));
    let production = collector.split("#[path = \"visible_frame_tests.rs\"]").next().unwrap();
    assert!(production.contains("parser.try_lock_until(deadline)"));
    assert!(production.contains("acquire_parser(acquire, entry.id, &entry.parser, deadline)"));
    assert!(production.contains(
        "self.try_collect_with(&TimedParserAcquire, COLLECTION_LOCK_BUDGET, before_first_lock)"
    ));
    assert!(!production.contains("entry.parser.try_lock()"), "no parser is only probed");
    assert_eq!(production.matches("let deadline = acquire.now() + budget;").count(), 1);
    let worker = strip(concat!(
        include_str!("spawn_pane.rs"),
        "\n",
        include_str!("spawn_pane_commands.rs"),
        "\n",
        include_str!("spawn_pane_output.rs"),
        "\n",
        include_str!("spawn_pane_vt.rs")
    ));
    assert_eq!(worker.matches("MutexGuard::unlock_fair(parser);").count(), 1);
}

/// Integration over the real mutex: a collector parks on a held parser and takes it when the holder
/// releases on its own timer. The holder never waits for the collector, so this cannot hang.
#[test]
fn a_parked_collection_takes_a_parser_its_holder_releases() {
    let (mut app, window, _left, right, _inactive) = fixture(false, false);
    let parser = Arc::clone(&app.windows[&window].panes[&right].parser);
    let sources = sources(&mut app, window, false).ok().unwrap();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let guard = parser.lock();
        held_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        drop(guard);
    });
    held_rx.recv().unwrap();
    // The five-second budget is a watchdog for a stalled host, not a timing assertion.
    let collected =
        sources.try_collect_with(&TimedParserAcquire, Duration::from_secs(5), || {}).is_ok();
    holder.join().unwrap();
    assert!(collected, "the parked collection takes the parser once it is released");
}

/// The production budget expires on a parser held past it, and the holder releases on a watchdog if the
/// collector never returns, so a regression to an unbounded lock fails by assertion rather than hanging.
#[test]
fn a_parser_held_past_the_production_budget_is_contended_without_hanging() {
    let (mut app, window, _left, right, _inactive) = fixture(false, false);
    let parser = Arc::clone(&app.windows[&window].panes[&right].parser);
    let sources = sources(&mut app, window, false).ok().unwrap();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = parser.lock();
        held_tx.send(()).unwrap();
        // Released when the collector returns, or after the watchdog if it is stuck in an unbounded lock.
        let returned = returned_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        drop(guard);
        returned
    });
    held_rx.recv().unwrap();
    let outcome = sources.try_collect(|| {});
    let why = outcome.err();
    let _ = returned_tx.send(());
    assert!(holder.join().unwrap(), "the collector returned while the parser was still held");
    assert_eq!(why, Some(FrameUnavailable::Contended { pane_id: right, images: false }));
}
