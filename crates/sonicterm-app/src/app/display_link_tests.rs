//! Display-link pacing rules against a recording fake source and the fake dispatch clock. Every test
//! drives the production admission and completion adapters, the wait fold's service and deadline
//! collection, `sync_display_links` and the tick handler, for both window roles where both apply.

use super::*;
use crate::app::{
    redraw::{
        redraw_dispatch_tests::{
            at_ms, counters_of, fake_now, paced_owners, publish_output, set_fake_now, test_base,
        },
        DueCause, FrameSettlement, RedrawCause, WindowRedrawState,
    },
    visible_frame::FrameUnavailable,
};
use sonicterm_gpu::core::SurfaceRetryReason;
use std::cell::RefCell;

/// A fake link's shared log.
type LinkLog = std::rc::Rc<RefCell<FakeLinkLog>>;

/// Install a recording fake link on `id` and return its log.
fn install_fake_link(app: &mut App, id: WindowId) -> LinkLog {
    let log = std::rc::Rc::new(RefCell::new(FakeLinkLog::default()));
    app.windows.get_mut(&id).unwrap().display_link.source =
        Some(Box::new(FakeLink(std::rc::Rc::clone(&log))));
    log
}

/// Paced owners with a fake link on the chosen role: the App, the owner, its log and the base instant.
fn linked_owner(child_owner: bool) -> (App, WindowId, LinkLog, Instant) {
    let base = test_base();
    let (mut app, main, child) = paced_owners(base);
    let owner = if child_owner { child } else { main };
    let log = install_fake_link(&mut app, owner);
    (app, owner, log, base)
}

/// This window's redraw state.
fn redraw_of(app: &App, id: WindowId) -> &WindowRedrawState {
    &app.windows[&id].redraw
}

/// The generation the native target would read now.
fn current_generation(app: &App, id: WindowId) -> u64 {
    // Ordering: Acquire mirrors the native target's read of the generation it posts.
    redraw_of(app, id).link_generation.load(Ordering::Acquire)
}

/// Deliver a tick carrying `generation` at the fake instant, as the proxy event would.
fn deliver(app: &mut App, id: WindowId, generation: u64) {
    let target = fake_now();
    app.handle_display_link_tick(id, generation, target);
}

/// Fire the link as the native target does: read the generation now and deliver its tick.
fn fire(app: &mut App, id: WindowId) {
    let generation = current_generation(app, id);
    deliver(app, id, generation);
}

/// Admit at `at` with whatever is pending; returns whether the frame was admitted.
fn admit_at(app: &mut App, id: WindowId, at: Instant) -> bool {
    set_fake_now(at);
    app.admit_window_redraw(id)
}

/// Publish visible output, mark it and admit at `at`; returns whether the frame was admitted.
fn admit_output(app: &mut App, id: WindowId, at: Instant) -> bool {
    publish_output(app, id);
    app.mark_window_redraw(id, RedrawCause::Output);
    admit_at(app, id, at)
}

/// Complete an attempt at `at` with `outcome` through the completion adapter.
fn complete_at(app: &mut App, id: WindowId, at: Instant, outcome: FrameSettlement) {
    set_fake_now(at);
    let snapshot = app.snapshot_window_redraw_at(id, at).unwrap();
    app.complete_window_redraw(id, &snapshot, outcome);
}

/// The wait fold at `at`, in `do_about_to_wait`'s order: service due work, collect deadlines,
/// reconcile the links, keep the deadlines armed. Returns this owner's Frame deadlines.
fn about_to_wait(app: &mut App, id: WindowId, at: Instant) -> Vec<Instant> {
    set_fake_now(at);
    let due = app.refresh_frame_due_work_at(at);
    app.sync_display_links();
    let frames = due
        .iter()
        .filter(|work| work.owner == Some(id) && work.cause == DueCause::Frame)
        .map(|work| work.deadline)
        .collect();
    app.redraw_due = due;
    frames
}

/// Native redraw requests made on this test thread so far.
fn requests() -> u64 {
    crate::app::window_state::window_redraw_requests()
}

/// The three link counters: accepted ticks, tick admissions, fallback admissions.
fn link_counts(app: &App, id: WindowId) -> (u64, u64, u64) {
    let counters = counters_of(app, id);
    (counters.display_link_ticks, counters.display_link_admissions, counters.display_link_fallbacks)
}

/// A busy visible parser on the owner's active pane.
fn busy_parser(app: &App, id: WindowId) -> FrameUnavailable {
    let window = &app.windows[&id];
    let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
    FrameUnavailable::Contended { pane_id, images: false }
}

/// The link runs only while a `Link` admission is pending: never while idle, started by a streaming
/// deferral, stopped once that admission is made, and never started by a timeout or contention
/// retry, which store `Timer`. Both roles.
#[test]
fn the_link_runs_only_while_a_streaming_admission_is_pending() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        about_to_wait(&mut app, owner, base);
        assert!(log.borrow().calls.is_empty(), "an idle window never runs its link");
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        assert!(log.borrow().running, "a pending Link admission runs the link");
        set_fake_now(at_ms(base, 16));
        fire(&mut app, owner);
        assert!(admit_at(&mut app, owner, at_ms(base, 16)));
        assert_eq!(redraw_of(&app, owner).pacing, None, "admission clears the mode");
        about_to_wait(&mut app, owner, at_ms(base, 16));
        assert_eq!(log.borrow().calls, vec![true, false], "the next fold stops the link");
        // A surface timeout stores Timer and runs no link.
        complete_at(
            &mut app,
            owner,
            at_ms(base, 17),
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        );
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        assert!(!admit_output(&mut app, owner, at_ms(base, 18)));
        about_to_wait(&mut app, owner, at_ms(base, 18));
        assert_eq!(log.borrow().calls, vec![true, false], "a timeout retry starts no link");
        assert!(
            admit_at(&mut app, owner, at_ms(base, 27)),
            "admitted one period after the attempt"
        );
        // A collection that meets a busy parser stores Timer and runs no link.
        let busy = busy_parser(&app, owner);
        app.visible_frame_unavailable(owner, busy, false, at_ms(base, 27));
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        assert!(!admit_output(&mut app, owner, at_ms(base, 28)));
        about_to_wait(&mut app, owner, at_ms(base, 28));
        assert!(admit_at(&mut app, owner, at_ms(base, 37)), "admitted at the contention floor");
        about_to_wait(&mut app, owner, at_ms(base, 37));
        assert_eq!(log.borrow().calls, vec![true, false], "a contention retry starts no link");
        assert_eq!(link_counts(&app, owner), (1, 1, 0), "child: {child_owner}");
    }
}

/// Each suppression writer the link must not outlive.
#[derive(Debug, Clone, Copy)]
enum Suppression {
    NativeOcclusion,
    BackendOcclusion,
    Stopped,
    Park,
    Hide,
    Degrade,
}

/// Run the production writer for `which` on window `id` at `at`.
fn suppress(app: &mut App, id: WindowId, which: Suppression, at: Instant) {
    set_fake_now(at);
    match which {
        Suppression::NativeOcclusion => app.handle_window_occlusion(id, true),
        Suppression::BackendOcclusion | Suppression::Stopped => {
            let outcome = match which {
                Suppression::Stopped => FrameSettlement::Stopped(7),
                _ => FrameSettlement::SurfaceRetry(SurfaceRetryReason::Occluded),
            };
            let redraw = &mut app.windows.get_mut(&id).unwrap().redraw;
            let snapshot = redraw.snapshot();
            redraw.settle(snapshot, outcome, at);
        }
        Suppression::Park => {
            let redraw = &mut app.windows.get_mut(&id).unwrap().redraw;
            let snapshot = redraw.snapshot();
            redraw.park(snapshot);
        }
        Suppression::Hide => app.hide_main_window(),
        Suppression::Degrade => app.set_software_render_degrade(true),
    }
}

/// Undo `which` the way the app does when the window becomes visible and usable again.
fn restore(app: &mut App, id: WindowId, which: Suppression) {
    match which {
        Suppression::NativeOcclusion | Suppression::BackendOcclusion => {
            app.handle_window_occlusion(id, false);
        }
        Suppression::Stopped => app.windows.get_mut(&id).unwrap().redraw.stopped_generation = None,
        Suppression::Park => app.mark_window_redraw(id, RedrawCause::Topology),
        Suppression::Hide => app.windows.get_mut(&id).unwrap().hidden = false,
        Suppression::Degrade => app.set_software_render_degrade(false),
    }
}

/// Every suppression writer invalidates link pacing at the writer: the generation moves, the live
/// interval, an unused permit and a stored `Link` are dropped, the next fold stops the link, and a
/// new streaming deferral after visibility returns restarts it at a newer generation.
#[test]
fn every_suppression_writer_stops_and_invalidates_the_link() {
    for which in [
        Suppression::NativeOcclusion,
        Suppression::BackendOcclusion,
        Suppression::Stopped,
        Suppression::Park,
        Suppression::Hide,
        Suppression::Degrade,
    ] {
        for child_owner in [false, true] {
            if matches!(which, Suppression::Hide) && child_owner {
                // When: the writer is hide, only the main window can be hidden.
                continue;
            }
            let label = format!("{which:?} child={child_owner}");
            let (mut app, owner, log, base) = linked_owner(child_owner);
            assert!(!admit_output(&mut app, owner, at_ms(base, 1)), "{label}");
            about_to_wait(&mut app, owner, at_ms(base, 1));
            set_fake_now(at_ms(base, 2));
            fire(&mut app, owner);
            let started = redraw_of(&app, owner).link_live.expect("the link runs");
            assert_eq!(redraw_of(&app, owner).link_permit, Some(started), "{label}");
            let before = current_generation(&app, owner);
            suppress(&mut app, owner, which, at_ms(base, 3));
            assert!(current_generation(&app, owner) > before, "{label}: generation bumped");
            let redraw = redraw_of(&app, owner);
            assert_eq!(
                (redraw.link_live, redraw.link_permit, redraw.pacing),
                (None, None, None),
                "{label}"
            );
            about_to_wait(&mut app, owner, at_ms(base, 3));
            assert!(!log.borrow().running, "{label}: stopped at the next fold");
            restore(&mut app, owner, which);
            assert!(!admit_output(&mut app, owner, at_ms(base, 4)), "{label}");
            assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "{label}");
            about_to_wait(&mut app, owner, at_ms(base, 4));
            let restarted = redraw_of(&app, owner).link_live.expect("restarted");
            assert!(restarted > started, "{label}: the restart bumps the generation");
            assert_eq!(log.borrow().calls, vec![true, false, true], "{label}");
        }
    }
}

/// The writers no headless fixture reaches still invalidate at the writer: `begin_window_redraw`'s
/// device-stop branch calls `invalidate_link_pacing`, and every software-degrade writer goes through
/// the invalidating setter instead of assigning the flag.
#[test]
fn device_stop_and_degrade_writers_invalidate_link_pacing() {
    let redraw = include_str!("redraw.rs").replace("\r\n", "\n");
    let begin = &redraw[redraw.find("pub(super) fn begin_window_redraw(").unwrap()..];
    let stop = begin.find("if !renderer.device_accepts_gpu_work() {").unwrap();
    let stop_branch = &begin[stop..stop + begin[stop..].find("return false;").unwrap()];
    assert!(stop_branch.contains("invalidate_link_pacing()"), "{stop_branch}");
    for (name, source) in [
        ("config_apply.rs", include_str!("config_apply.rs")),
        ("event_loop.rs", include_str!("event_loop.rs")),
        ("gpu_recovery.rs", include_str!("gpu_recovery.rs")),
    ] {
        assert!(source.contains("set_software_render_degrade("), "{name} uses the setter");
        assert!(!source.contains("self.software_render_degrade ="), "{name} assigns the flag");
    }
}

/// How the pending streaming admission is delivered in the whole-sequence test.
#[derive(Debug, Clone, Copy)]
enum Delivery {
    /// A tick arrives at 16 ms.
    Tick,
    /// No tick arrives; due service at the 20 ms ceiling requests the frame.
    NoTick,
    /// An `Expose` redraw at 19.999 ms still defers; the ceiling admits.
    Expose,
}

/// The whole sequence through start, due service and redraw delivery, period 10 ms: a streaming
/// deferral at 1 arms the 20 ms ceiling, not 10; a non-timer fold at 10 services nothing; nothing is
/// admitted before 16 or 20; a tick admission or a fallback is counted exactly once; and completion
/// stamps the fake instant. Both roles.
#[test]
fn a_streaming_deferral_admits_on_a_tick_or_at_the_two_period_ceiling() {
    for child_owner in [false, true] {
        for delivery in [Delivery::Tick, Delivery::NoTick, Delivery::Expose] {
            let label = format!("{delivery:?} child={child_owner}");
            let (mut app, owner, log, base) = linked_owner(child_owner);
            let deferred = if matches!(delivery, Delivery::Expose) {
                app.mark_window_redraw(owner, RedrawCause::Expose);
                admit_at(&mut app, owner, at_ms(base, 1))
            } else {
                admit_output(&mut app, owner, at_ms(base, 1))
            };
            assert!(!deferred, "{label}");
            assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "{label}");
            assert_eq!(
                about_to_wait(&mut app, owner, at_ms(base, 1)),
                vec![at_ms(base, 20)],
                "{label}"
            );
            assert!(log.borrow().running, "{label}");
            let before = requests();
            assert_eq!(
                about_to_wait(&mut app, owner, at_ms(base, 10)),
                vec![at_ms(base, 20)],
                "{label}"
            );
            assert!(redraw_of(&app, owner).deferred, "{label}: nothing is serviced at 10");
            assert_eq!(requests(), before, "{label}");
            assert!(
                !admit_at(&mut app, owner, at_ms(base, 15)),
                "{label}: nothing admits before 16"
            );
            let admitted_at = match delivery {
                Delivery::Tick => {
                    set_fake_now(at_ms(base, 16));
                    fire(&mut app, owner);
                    assert_eq!(requests(), before + 1, "{label}: the tick requests one frame");
                    assert!(admit_at(&mut app, owner, at_ms(base, 16)), "{label}");
                    assert_eq!(link_counts(&app, owner), (1, 1, 0), "{label}");
                    at_ms(base, 16)
                }
                Delivery::NoTick => {
                    assert!(about_to_wait(&mut app, owner, at_ms(base, 20)).is_empty(), "{label}");
                    assert_eq!(requests(), before + 1, "{label}: due service requests one frame");
                    assert!(!redraw_of(&app, owner).deferred, "{label}");
                    assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "{label}");
                    assert!(log.borrow().running, "{label}: due service does not stop the link");
                    assert!(admit_at(&mut app, owner, at_ms(base, 20)), "{label}");
                    assert_eq!(link_counts(&app, owner), (0, 0, 1), "{label}");
                    at_ms(base, 20)
                }
                Delivery::Expose => {
                    app.mark_window_redraw(owner, RedrawCause::Expose);
                    let edge = at_ms(base, 20) - Duration::from_micros(1);
                    assert!(!admit_at(&mut app, owner, edge), "{label}: 19.999 ms defers");
                    assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "{label}");
                    assert!(admit_at(&mut app, owner, at_ms(base, 20)), "{label}");
                    assert_eq!(link_counts(&app, owner), (0, 0, 1), "{label}");
                    at_ms(base, 20)
                }
            };
            assert_eq!(redraw_of(&app, owner).pacing, None, "{label}");
            complete_at(&mut app, owner, admitted_at, FrameSettlement::Presented);
            let window = &app.windows[&owner];
            assert_eq!(window.last_render, admitted_at, "{label}");
            assert_eq!(window.stream_clock, admitted_at, "{label}");
            assert_eq!(window.redraw.last_present, Some(admitted_at), "{label}");
            about_to_wait(&mut app, owner, admitted_at + Duration::from_millis(1));
            assert!(!log.borrow().running, "{label}: stopped after the admission");
        }
    }
}

/// A permit stored before native occlusion is not spent after un-occlusion in the same period: the
/// next streaming redraw defers, the link restarts at a newer generation, and only a tick of that
/// generation admits. Completion stamps the fake instant. Both roles.
#[test]
fn a_permit_from_before_occlusion_is_not_used_after_it() {
    for child_owner in [false, true] {
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        let first = redraw_of(&app, owner).link_live.expect("the link runs");
        set_fake_now(at_ms(base, 3));
        fire(&mut app, owner);
        assert_eq!(redraw_of(&app, owner).link_permit, Some(first));
        set_fake_now(at_ms(base, 4));
        app.handle_window_occlusion(owner, true);
        assert_eq!(redraw_of(&app, owner).link_permit, None, "occlusion clears the permit");
        set_fake_now(at_ms(base, 5));
        app.handle_window_occlusion(owner, false);
        assert!(!admit_output(&mut app, owner, at_ms(base, 6)), "old readiness is not used");
        about_to_wait(&mut app, owner, at_ms(base, 6));
        let second = redraw_of(&app, owner).link_live.expect("restarted");
        assert!(second > first, "child: {child_owner}");
        assert!(!admit_at(&mut app, owner, at_ms(base, 8)), "no tick of the new generation yet");
        set_fake_now(at_ms(base, 12));
        fire(&mut app, owner);
        assert!(admit_at(&mut app, owner, at_ms(base, 12)));
        assert_eq!(link_counts(&app, owner), (2, 1, 0), "child: {child_owner}");
        complete_at(&mut app, owner, at_ms(base, 12), FrameSettlement::Presented);
        assert_eq!(app.windows[&owner].last_render, at_ms(base, 12));
        assert_eq!(app.windows[&owner].stream_clock, at_ms(base, 12));
    }
}

/// A queued tick from an earlier interval is rejected when it is delivered: no count, no permit and
/// no redraw request. So is a tick carrying the invalidated generation while no interval is live.
#[test]
fn a_tick_from_an_earlier_interval_is_rejected() {
    for child_owner in [false, true] {
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        let queued = current_generation(&app, owner);
        set_fake_now(at_ms(base, 2));
        app.handle_window_occlusion(owner, true);
        set_fake_now(at_ms(base, 3));
        app.handle_window_occlusion(owner, false);
        assert!(!admit_output(&mut app, owner, at_ms(base, 4)));
        about_to_wait(&mut app, owner, at_ms(base, 4));
        assert!(redraw_of(&app, owner).link_live.expect("restarted") > queued);
        let before = requests();
        set_fake_now(at_ms(base, 5));
        deliver(&mut app, owner, queued);
        assert_eq!(link_counts(&app, owner).0, 0, "child: {child_owner}");
        assert_eq!(redraw_of(&app, owner).link_permit, None);
        assert_eq!(requests(), before, "a stale tick requests no frame");
        assert!(!redraw_of(&app, owner).request_in_flight);
        // Invalidate again; with `Link` stored but no live interval, the invalidated generation is refused.
        set_fake_now(at_ms(base, 6));
        app.handle_window_occlusion(owner, true);
        set_fake_now(at_ms(base, 7));
        app.handle_window_occlusion(owner, false);
        let invalidated = current_generation(&app, owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 8)));
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link));
        assert_eq!(redraw_of(&app, owner).link_live, None);
        deliver(&mut app, owner, invalidated);
        assert_eq!(link_counts(&app, owner).0, 0, "child: {child_owner}");
        assert_eq!(redraw_of(&app, owner).link_permit, None);
        assert_eq!(requests(), before);
    }
}

/// A stored mode is kept until admission: a `Timer` stored under software degradation still uses the
/// timer rule after degradation ends, and starts no link; a stored `Link` survives due service.
#[test]
fn the_stored_mode_is_kept_until_admission() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        app.set_software_render_degrade(true);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        app.set_software_render_degrade(false);
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer), "Timer is not upgraded");
        assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 1)), vec![at_ms(base, 10)]);
        assert!(log.borrow().calls.is_empty(), "no link for a Timer admission");
        assert!(admit_at(&mut app, owner, at_ms(base, 12)), "the timer rule admits at 12");
        about_to_wait(&mut app, owner, at_ms(base, 12));
        assert!(log.borrow().calls.is_empty());
        assert_eq!(link_counts(&app, owner), (0, 0, 0));
        // The next deferral stores Link, which survives a contention-free due service.
        assert!(!admit_output(&mut app, owner, at_ms(base, 14)));
        assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 14)), vec![at_ms(base, 20)]);
        about_to_wait(&mut app, owner, at_ms(base, 20));
        assert!(!redraw_of(&app, owner).deferred, "due service cleared deferred");
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "child: {child_owner}");
        assert!(log.borrow().running);
    }
}

/// An accepted tick that a contention or timeout refusal blocks is counted as a tick, not an
/// admission; a valid permit keeps the Frame deadline at the refusal's floor; two ticks before one
/// admission count two ticks and one admission. Both roles.
#[test]
fn refused_and_repeated_ticks_are_counted_as_ticks() {
    for child_owner in [false, true] {
        // A tick, then a contention refusal.
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        set_fake_now(at_ms(base, 5));
        fire(&mut app, owner);
        let busy = busy_parser(&app, owner);
        app.visible_frame_unavailable(owner, busy, false, at_ms(base, 5));
        assert!(!admit_at(&mut app, owner, at_ms(base, 6)));
        assert_eq!(link_counts(&app, owner), (1, 0, 0), "contention refuses the tick");
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link));
        assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 6)), vec![at_ms(base, 15)]);
        assert!(admit_at(&mut app, owner, at_ms(base, 15)), "the permit admits at the floor");
        assert_eq!(link_counts(&app, owner), (1, 1, 0));
        // A tick, then a surface timeout.
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        set_fake_now(at_ms(base, 5));
        fire(&mut app, owner);
        complete_at(
            &mut app,
            owner,
            at_ms(base, 5),
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        );
        assert!(!admit_at(&mut app, owner, at_ms(base, 6)));
        assert_eq!(link_counts(&app, owner), (1, 0, 0), "the timeout refuses the tick");
        // Two ticks before one admission.
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        let before = requests();
        set_fake_now(at_ms(base, 3));
        fire(&mut app, owner);
        set_fake_now(at_ms(base, 4));
        fire(&mut app, owner);
        assert_eq!(requests(), before + 1, "one frame request is in flight");
        assert!(admit_at(&mut app, owner, at_ms(base, 4)));
        assert_eq!(link_counts(&app, owner), (2, 1, 0), "child: {child_owner}");
    }
}

/// The four input/output combinations, each with and without a valid permit, classify through the
/// shared streaming predicate: the three streaming ones count a tick admission with a permit and a
/// fallback at the ceiling without one; pure typing admits at once and moves neither counter.
#[test]
fn link_admissions_classify_the_four_input_output_combinations() {
    for child_owner in [false, true] {
        for (input, output) in [(false, false), (false, true), (true, true), (true, false)] {
            for permit in [false, true] {
                let label =
                    format!("child={child_owner} input={input} output={output} permit={permit}");
                let (mut app, owner, _log, base) = linked_owner(child_owner);
                app.mark_window_redraw(owner, RedrawCause::Expose);
                assert!(!admit_at(&mut app, owner, at_ms(base, 1)), "{label}: a Link deferral");
                about_to_wait(&mut app, owner, at_ms(base, 1));
                if permit {
                    set_fake_now(at_ms(base, 5));
                    fire(&mut app, owner);
                }
                if input {
                    app.mark_window_redraw(owner, RedrawCause::Input);
                }
                if output {
                    publish_output(&app, owner);
                    app.mark_window_redraw(owner, RedrawCause::Output);
                }
                let streaming = output || !input;
                let first = admit_at(&mut app, owner, at_ms(base, 5));
                let ticks = u64::from(permit);
                if !streaming {
                    assert!(first, "{label}: pure typing is admitted at once");
                    assert_eq!(link_counts(&app, owner), (ticks, 0, 0), "{label}");
                } else if permit {
                    assert!(first, "{label}");
                    assert_eq!(link_counts(&app, owner), (ticks, 1, 0), "{label}");
                } else {
                    assert!(!first, "{label}");
                    assert!(!admit_at(&mut app, owner, at_ms(base, 19)), "{label}");
                    assert!(admit_at(&mut app, owner, at_ms(base, 20)), "{label}");
                    assert_eq!(link_counts(&app, owner), (0, 0, 1), "{label}");
                }
            }
        }
    }
}

/// Over a fixed-seed random sequence of output, input, ticks, admissions, completions, folds and
/// contention, every admission made in `Link` mode for streaming work is classified exactly once,
/// no other admission is classified, and attempts never fall below the classified admissions.
#[test]
fn every_link_mode_admission_is_classified_exactly_once() {
    for child_owner in [false, true] {
        let (mut app, owner, _log, base) = linked_owner(child_owner);
        let mut seed: u64 = 0x5eed_1550;
        let mut elapsed_us: u64 = 0;
        for step in 0..3000 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            elapsed_us += (seed >> 20) % 4_000;
            let now = base + Duration::from_micros(elapsed_us);
            set_fake_now(now);
            match (seed >> 33) % 9 {
                0 => {
                    publish_output(&app, owner);
                    app.mark_window_redraw(owner, RedrawCause::Output);
                }
                1 => app.mark_window_redraw(owner, RedrawCause::Input),
                2 => app.mark_window_redraw(owner, RedrawCause::Expose),
                3 => fire(&mut app, owner),
                4 | 5 => {
                    let window = &app.windows[&owner];
                    let link = window.redraw.pacing == Some(PacingMode::Link);
                    let streaming =
                        window.visible_output_advanced() || !window.redraw.input_pending();
                    let (_, admissions, fallbacks) = link_counts(&app, owner);
                    if app.admit_window_redraw(owner) {
                        let (_, admissions_after, fallbacks_after) = link_counts(&app, owner);
                        let classified =
                            admissions_after + fallbacks_after - admissions - fallbacks;
                        assert_eq!(classified, u64::from(link && streaming), "step {step}");
                        let outcome = match (seed >> 40) % 3 {
                            0 => FrameSettlement::Presented,
                            1 => FrameSettlement::Settled,
                            _ => FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
                        };
                        let snapshot = app.snapshot_window_redraw_at(owner, now).unwrap();
                        app.complete_window_redraw(owner, &snapshot, outcome);
                    }
                }
                6 => {
                    about_to_wait(&mut app, owner, now);
                }
                7 => {
                    let busy = busy_parser(&app, owner);
                    app.visible_frame_unavailable(owner, busy, false, now);
                }
                _ => {}
            }
            let counters = counters_of(&app, owner);
            assert!(
                counters.attempts
                    >= counters.display_link_admissions + counters.display_link_fallbacks,
                "step {step}"
            );
        }
        let (_, admissions, fallbacks) = link_counts(&app, owner);
        assert!(
            admissions > 0 && fallbacks > 0,
            "both classes exercised: {admissions} {fallbacks}"
        );
    }
}

/// With a link installed, the rules outside hardware streaming are unchanged and run no link: pure
/// input admits at once, a surface timeout waits exactly one period from the attempt, the contention
/// floor holds, and the software path keeps 25,000 µs and 83,333 µs while composing.
#[test]
fn link_installed_windows_keep_every_other_rule() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        app.mark_window_redraw(owner, RedrawCause::Input);
        assert!(admit_at(&mut app, owner, at_ms(base, 1)), "pure input admits immediately");
        complete_at(
            &mut app,
            owner,
            at_ms(base, 1),
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        );
        publish_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 1)), vec![at_ms(base, 11)]);
        assert!(!admit_at(&mut app, owner, at_ms(base, 11) - Duration::from_nanos(1)));
        assert!(admit_at(&mut app, owner, at_ms(base, 11)), "one period from the attempt");
        // The loop folds after the admitting handler, which services the deadline armed at 1.
        about_to_wait(&mut app, owner, at_ms(base, 11));
        let busy = busy_parser(&app, owner);
        app.visible_frame_unavailable(owner, busy, false, at_ms(base, 11));
        assert!(!admit_output(&mut app, owner, at_ms(base, 12)));
        assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 12)), vec![at_ms(base, 21)]);
        assert!(!admit_at(&mut app, owner, at_ms(base, 21) - Duration::from_nanos(1)));
        assert!(admit_at(&mut app, owner, at_ms(base, 21)), "admitted at the contention floor");
        about_to_wait(&mut app, owner, at_ms(base, 21));
        assert!(log.borrow().calls.is_empty(), "child: {child_owner}");
        assert_eq!(link_counts(&app, owner), (0, 0, 0));
        for composing in [false, true] {
            let (mut app, owner, log, base) = linked_owner(child_owner);
            app.set_software_render_degrade(true);
            let period = Duration::from_micros(if composing { 83_333 } else { 25_000 });
            if composing {
                app.windows.get_mut(&owner).unwrap().ime.handle_preedit("中", None);
            }
            assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
            assert_eq!(about_to_wait(&mut app, owner, at_ms(base, 1)), vec![base + period]);
            assert!(!admit_at(&mut app, owner, base + period - Duration::from_micros(1)));
            assert!(admit_at(&mut app, owner, base + period), "composing={composing}");
            about_to_wait(&mut app, owner, base + period);
            assert!(log.borrow().calls.is_empty(), "the software path runs no link");
            assert_eq!(link_counts(&app, owner), (0, 0, 0));
        }
    }
}

/// A surface timeout completed at 0 with streaming output pending stays timer-owned: its deadline is
/// 10, due service requests it at 10 and it is admitted at 10, with no link and no link counts.
#[test]
fn a_timeout_retry_stays_timer_owned_with_a_link_installed() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        publish_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        complete_at(
            &mut app,
            owner,
            base,
            FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        );
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        assert_eq!(about_to_wait(&mut app, owner, base), vec![at_ms(base, 10)]);
        let before = requests();
        assert!(about_to_wait(&mut app, owner, at_ms(base, 10)).is_empty());
        assert_eq!(requests(), before + 1, "due service requests the retry");
        assert!(admit_at(&mut app, owner, at_ms(base, 10)), "admitted at exactly one period");
        about_to_wait(&mut app, owner, at_ms(base, 10));
        assert!(log.borrow().calls.is_empty(), "child: {child_owner}");
        assert_eq!(link_counts(&app, owner), (0, 0, 0));
    }
}

/// A contention retry armed at 0 stays timer-owned: admitted at the contention floor, no link, no
/// link counts.
#[test]
fn a_contention_retry_stays_timer_owned_with_a_link_installed() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        publish_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        let busy = busy_parser(&app, owner);
        set_fake_now(base);
        app.visible_frame_unavailable(owner, busy, false, base);
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        assert_eq!(about_to_wait(&mut app, owner, base), vec![at_ms(base, 10)]);
        assert!(about_to_wait(&mut app, owner, at_ms(base, 10)).is_empty());
        assert!(admit_at(&mut app, owner, at_ms(base, 10)), "admitted at the contention floor");
        about_to_wait(&mut app, owner, at_ms(base, 10));
        assert!(log.borrow().calls.is_empty(), "child: {child_owner}");
        assert_eq!(link_counts(&app, owner), (0, 0, 0));
    }
}

/// A synchronized-output hold stops the link, and no tick can authorize the held frame; the stored
/// `Link` survives as data. Released before the 20 ms ceiling, the admission defers and the link
/// restarts at a newer generation; released after it, the admission is a fallback with no restart.
/// The hold is the window's winning `Sync` deferral, set the way admission sets it.
#[test]
fn a_sync_hold_stops_the_link_and_keeps_the_mode() {
    for child_owner in [false, true] {
        for late in [false, true] {
            let label = format!("child={child_owner} late={late}");
            let (mut app, owner, log, base) = linked_owner(child_owner);
            assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
            about_to_wait(&mut app, owner, at_ms(base, 1));
            let held = redraw_of(&app, owner).link_live.expect("the link runs");
            let redraw = &mut app.windows.get_mut(&owner).unwrap().redraw;
            redraw.deferred_rule = Some(crate::app::frame_counters::DeferRule::Sync);
            assert!(redraw.sync_hold(), "{label}: a deferred window under Sync is held");
            set_fake_now(at_ms(base, 2));
            app.sync_display_links();
            assert_eq!(log.borrow().calls, vec![true, false], "{label}: the hold stops the link");
            let before = requests();
            let tick_times: &[u64] = if late { &[16, 32, 48] } else { &[16] };
            for &millis in tick_times {
                set_fake_now(at_ms(base, millis));
                deliver(&mut app, owner, held);
            }
            assert_eq!(requests(), before, "{label}: no held tick requests a frame");
            assert_eq!(redraw_of(&app, owner).link_permit, None, "{label}");
            assert_eq!(link_counts(&app, owner).0, 0, "{label}");
            assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Link), "{label}");
            if late {
                assert!(admit_at(&mut app, owner, at_ms(base, 50)), "{label}: past the ceiling");
                assert!(!redraw_of(&app, owner).sync_hold(), "{label}");
                assert_eq!(link_counts(&app, owner), (0, 0, 1), "{label}");
                assert_eq!(redraw_of(&app, owner).pacing, None, "{label}");
                about_to_wait(&mut app, owner, at_ms(base, 50));
                assert_eq!(log.borrow().calls, vec![true, false], "{label}: no restart");
            } else {
                assert!(!admit_at(&mut app, owner, at_ms(base, 17)), "{label}: before the ceiling");
                assert!(!redraw_of(&app, owner).sync_hold(), "{label}");
                about_to_wait(&mut app, owner, at_ms(base, 17));
                let restarted = redraw_of(&app, owner).link_live.expect("restarted");
                assert!(restarted > held, "{label}");
                set_fake_now(at_ms(base, 18));
                fire(&mut app, owner);
                assert!(admit_at(&mut app, owner, at_ms(base, 18)), "{label}");
                assert_eq!(link_counts(&app, owner), (1, 1, 0), "{label}");
            }
        }
    }
}

/// Pausing a native link whose generation was invalidated leaves the replacement state: a `Timer`
/// stored after software degradation began survives the pause, still decides the admission after
/// degradation ends, and no link starts for it.
#[test]
fn finishing_an_old_native_pause_keeps_a_replacement_timer() {
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        app.set_software_render_degrade(true);
        assert_eq!(redraw_of(&app, owner).pacing, None);
        assert!(!admit_output(&mut app, owner, at_ms(base, 2)));
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer));
        about_to_wait(&mut app, owner, at_ms(base, 2));
        assert_eq!(log.borrow().calls, vec![true, false], "the old interval is paused");
        assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer), "the pause keeps Timer");
        app.set_software_render_degrade(false);
        assert!(admit_at(&mut app, owner, at_ms(base, 12)), "the stored Timer admits at 12");
        about_to_wait(&mut app, owner, at_ms(base, 12));
        assert_eq!(log.borrow().calls, vec![true, false], "child: {child_owner}");
    }
}

/// A `Timer` stored by a surface timeout at 0 survives link invalidation by software degradation
/// turning on and off, or by occlusion and un-occlusion, and is admitted at 10 with no link and no
/// link counts.
#[test]
fn a_stored_timer_survives_link_invalidation() {
    for child_owner in [false, true] {
        for through_occlusion in [false, true] {
            let label = format!("child={child_owner} occlusion={through_occlusion}");
            let (mut app, owner, log, base) = linked_owner(child_owner);
            publish_output(&app, owner);
            app.mark_window_redraw(owner, RedrawCause::Output);
            complete_at(
                &mut app,
                owner,
                base,
                FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
            );
            set_fake_now(at_ms(base, 2));
            if through_occlusion {
                app.handle_window_occlusion(owner, true);
                app.handle_window_occlusion(owner, false);
            } else {
                app.set_software_render_degrade(true);
                app.set_software_render_degrade(false);
            }
            assert_eq!(redraw_of(&app, owner).pacing, Some(PacingMode::Timer), "{label}");
            assert!(admit_at(&mut app, owner, at_ms(base, 10)), "{label}: admitted at 10");
            about_to_wait(&mut app, owner, at_ms(base, 10));
            assert!(log.borrow().calls.is_empty(), "{label}");
            assert_eq!(link_counts(&app, owner), (0, 0, 0), "{label}");
        }
    }
}

/// The link's preferred rate follows the window's monitor period: installation sets the current
/// period, a refresh from 60 to 120 Hz sets the new one, and a refresh that keeps the rate, or finds
/// it unavailable, sets nothing. Both roles.
#[test]
fn the_preferred_period_follows_the_window_monitor_period() {
    let hz_60 = Duration::from_micros(16_666);
    let hz_120 = Duration::from_micros(8_333);
    for child_owner in [false, true] {
        let base = test_base();
        let (mut app, main, child) = paced_owners(base);
        let owner = if child_owner { child } else { main };
        let window = app.windows.get_mut(&owner).unwrap();
        window.redraw.monitor_rate_override = Some(Some(60_000));
        window.refresh_monitor_period();
        let log = std::rc::Rc::new(RefCell::new(FakeLinkLog::default()));
        app.install_display_link(owner, Box::new(FakeLink(std::rc::Rc::clone(&log))));
        assert_eq!(log.borrow().periods, vec![hz_60], "installation (child: {child_owner})");
        let window = app.windows.get_mut(&owner).unwrap();
        window.redraw.monitor_rate_override = Some(Some(120_000));
        window.refresh_monitor_period();
        assert_eq!(log.borrow().periods, vec![hz_60, hz_120], "60 to 120 Hz");
        window.refresh_monitor_period();
        window.redraw.monitor_rate_override = Some(None);
        window.refresh_monitor_period();
        assert_eq!(log.borrow().periods, vec![hz_60, hz_120], "an unchanged rate sets nothing");
        assert!(log.borrow().calls.is_empty(), "installation does not start the link");
    }
}

/// `source` with comments blanked and CRLF normalized, so a scan matches only real code.
fn code_of(source: &str) -> String {
    crate::app::source_scan_support::code_views(source).0
}

/// The body of the item `signature` opens, up to the closing brace at the signature's indentation.
fn body_of<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let indent = &source[line_start..start];
    let close = format!("\n{indent}}}\n");
    let end = source[start..].find(&close).map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// Whether `body` assigns `field` (`field =`, not `field ==`).
fn assigns(body: &str, field: &str) -> bool {
    body.match_indices(&format!("{field} =")).any(|(offset, matched)| {
        body.as_bytes().get(offset + matched.len()) != Some(&b'=')
            && !body[..offset].ends_with(|byte: char| byte.is_alphanumeric() || byte == '_')
    })
}

/// The native link is macOS-only and never a CVDisplayLink; its tick target only reads the shared
/// generation and posts through the proxy, never naming the App; registration installs the link
/// after reading the monitor period; the wait fold reconciles links between collecting deadlines
/// and arming the wake; every suppression writer invalidates link pacing at the writer; due
/// service and the reconcile never write the pacing mode; and neither handler passes the real
/// clock to admission. CRLF-normalized, comments blanked.
#[test]
fn display_link_sources_keep_their_contracts() {
    let link = code_of(include_str!("display_link.rs"));
    assert!(link.contains("#[cfg(target_os = \"macos\")]\npub(super) mod native {"), "native gate");
    assert!(!link.contains("CVDisplayLink"), "only NSView.displayLink is used");
    let native = body_of(&link, "pub(super) mod native {");
    assert!(native.contains("available!(macos = 14.0)"), "the timer path stays below macOS 14");
    let fired = body_of(native, "fn display_link_fired(");
    assert!(fired.contains("send_event("), "{fired}");
    assert!(fired.contains("generation.load(Ordering::Acquire)"), "{fired}");
    assert!(!fired.contains("App"), "the tick target never touches the App: {fired}");

    let registry = code_of(include_str!("mod.rs"));
    let insert = body_of(&registry, "pub(super) fn insert_window_registered(");
    let refresh = insert.find("refresh_monitor_period()").expect("the period is read");
    let install = insert.find("install_native_display_link(id)").expect("the link is installed");
    assert!(refresh < install, "the link takes the period just read: {insert}");

    let event_loop = code_of(include_str!("event_loop.rs"));
    let wait = body_of(&event_loop, "pub(super) fn do_about_to_wait(");
    let collect = wait.find("refresh_frame_due_work_at(").expect("deadlines are collected");
    let sync = wait.find("sync_display_links(").expect("links are reconciled");
    let arm = wait.find("set_control_flow(").expect("the wake is armed");
    assert!(collect < sync && sync < arm, "{wait}");

    let redraw = code_of(include_str!("redraw.rs"));
    let occlusion = body_of(&redraw, "fn observe_native_occlusion(");
    let settle = body_of(&redraw, "pub(super) fn settle(");
    let stopped = body_of(settle, "FrameSettlement::Stopped(generation) => {");
    let backend = body_of(settle, "SurfaceRetryReason::Occluded => {");
    let park = body_of(&redraw, "pub(super) fn park(");
    let child_tabs = code_of(include_str!("child_tabs.rs"));
    let hide = body_of(&child_tabs, "pub(super) fn hide_main_window(");
    let degrade = body_of(&link, "pub(super) fn set_software_render_degrade(");
    for (writer, body) in [
        ("native occlusion", occlusion),
        ("backend occlusion", backend),
        ("device stop", stopped),
        ("park", park),
        ("hide", hide),
        ("software degrade", degrade),
    ] {
        assert!(body.contains("invalidate_link_pacing()"), "{writer}: {body}");
    }

    let service = body_of(&redraw, "pub(super) fn service_redraw_due(");
    let reconcile = body_of(&link, "pub(super) fn sync_display_link(");
    let reconcile_all = body_of(&link, "pub(super) fn sync_display_links(");
    for (name, body) in
        [("service", service), ("reconcile", reconcile), ("reconcile all", reconcile_all)]
    {
        assert!(!assigns(body, "pacing"), "{name} must not write the pacing mode: {body}");
        assert!(!body.contains("invalidate_link_pacing("), "{name} must not cancel link pacing");
    }

    for (name, source) in [
        ("window_event.rs", code_of(include_str!("window_event.rs"))),
        ("child_window.rs", code_of(include_str!("child_window.rs"))),
    ] {
        for (offset, _) in source.match_indices("begin_window_redraw(") {
            let call = &source[offset..offset + source[offset..].find(')').unwrap()];
            assert!(!call.contains("Instant::now"), "{name}: {call}");
        }
    }
}

/// The scan helper's assignment check tells a write from a comparison or a longer name.
#[test]
fn the_assignment_scan_tells_a_write_from_a_comparison() {
    assert!(assigns("self.redraw.pacing = None;", "pacing"));
    assert!(!assigns("if self.redraw.pacing == Some(mode) {", "pacing"));
    assert!(!assigns("self.link_pacing = None;", "pacing"));
}

/// An early wake does not end a synchronized-output hold: after a held admission, a cursor repaint
/// clears `deferred` before admission re-evaluates, yet the stored `Link` stays paused, and a tick
/// delivered before the next redraw is rejected and not counted.
#[test]
fn an_early_wake_keeps_a_held_window_off_the_link() {
    use crate::app::spawn_pane::{pack_sync_deadline, sync_word_of};
    for child_owner in [false, true] {
        let (mut app, owner, log, base) = linked_owner(child_owner);
        assert!(!admit_output(&mut app, owner, at_ms(base, 1)));
        about_to_wait(&mut app, owner, at_ms(base, 1));
        let held = redraw_of(&app, owner).link_live.expect("the link runs");
        // A window that has presented once is not forced as a first frame.
        app.windows.get_mut(&owner).unwrap().redraw.last_present = Some(base);
        {
            let window = &app.windows[&owner];
            let pane = &window.panes[&window.tab_states[window.tabs.active_index()].active_pane];
            let state = sonicterm_vt::vt::SyncState { set: true, epoch: 1, resets: 0 };
            let deadline = base + Duration::from_secs(10);
            pane.sync_deadline_word.store(pack_sync_deadline(1, deadline), Ordering::Relaxed);
            pane.sync_word.store(sync_word_of(state), Ordering::Release);
        }
        assert!(!admit_at(&mut app, owner, at_ms(base, 2)), "child={child_owner}: held");
        app.sync_display_links();
        assert_eq!(log.borrow().calls, vec![true, false], "child={child_owner}: paused");
        app.repaint_owner(owner, &[RedrawCause::Cursor]);
        set_fake_now(at_ms(base, 3));
        app.sync_display_links();
        assert_eq!(log.borrow().calls, vec![true, false], "child={child_owner}: still paused");
        deliver(&mut app, owner, held);
        let generation = current_generation(&app, owner);
        deliver(&mut app, owner, generation);
        assert_eq!(redraw_of(&app, owner).link_permit, None, "child={child_owner}");
        assert_eq!(link_counts(&app, owner).0, 0, "child={child_owner}: no tick counted");
    }
}
