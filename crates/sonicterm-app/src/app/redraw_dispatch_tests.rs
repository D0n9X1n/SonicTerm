//! The dispatch clock and the admission and completion adapters both window roles call, with the
//! fake-clock fixtures the display-link pacing tests share.

use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use std::cell::Cell;

thread_local! {
    /// The fake dispatch clock's current instant; each test thread sets its own.
    static FAKE_NOW: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The fake dispatch clock: the instant this test thread last set.
pub(in crate::app) fn fake_now() -> Instant {
    FAKE_NOW.with(Cell::get).expect("the test sets the fake clock before any pacing read")
}

/// Move the fake dispatch clock to `at`.
pub(in crate::app) fn set_fake_now(at: Instant) {
    FAKE_NOW.with(|cell| cell.set(Some(at)));
}

/// A base instant one second ahead, so instants built from it never precede a window's creation.
pub(in crate::app) fn test_base() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

/// `millis` milliseconds after `base`.
pub(in crate::app) fn at_ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

/// Counting main and child owners on the fake dispatch clock, set to `base`, with a 10 ms period
/// and both pacing clocks at `base`.
pub(in crate::app) fn paced_owners(base: Instant) -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    app.__test_set_dispatch_clock(fake_now);
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    for id in [main, child] {
        let window = app.windows.get_mut(&id).unwrap();
        window.redraw.monitor_period = Duration::from_millis(10);
        window.last_render = base;
        window.stream_clock = base;
    }
    set_fake_now(base);
    (app, main, child)
}

/// Publish one completed output batch on the window's visible active pane.
pub(in crate::app) fn publish_output(app: &App, id: WindowId) {
    let window = &app.windows[&id];
    let pane = window.tab_states[window.tabs.active_index()].active_pane;
    window.panes[&pane].output_generation.fetch_add(1, Ordering::Release);
}

/// This window's frame counters; the paced owners always count.
pub(in crate::app) fn counters_of(
    app: &App,
    id: WindowId,
) -> &super::super::frame_counters::WindowFrameCounters {
    app.windows[&id].redraw.frame_counters.as_deref().expect("counting App")
}

/// The text of the impl-level method `signature` names, up to its closing brace.
fn method_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start..].find("\n    }\n").map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// Admission and completion stamp the fake dispatch clock for either role: a streaming frame is
/// refused before one period and admitted at it, and completion writes both pacing clocks and the
/// present time at the fake instant, never the real one.
#[test]
fn dispatch_clock_stamps_admission_and_completion_for_both_roles() {
    for child_owner in [false, true] {
        let base = test_base();
        let (mut app, main, child) = paced_owners(base);
        let owner = if child_owner { child } else { main };
        publish_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        set_fake_now(at_ms(base, 9));
        assert!(
            !app.admit_window_redraw(owner),
            "9 ms is inside the period (child: {child_owner})"
        );
        set_fake_now(at_ms(base, 10));
        assert!(app.admit_window_redraw(owner), "admitted at one period (child: {child_owner})");
        let snapshot = app.snapshot_window_redraw_at(owner, fake_now()).unwrap();
        set_fake_now(at_ms(base, 12));
        app.complete_window_redraw(owner, &snapshot, FrameSettlement::Presented);
        let window = &app.windows[&owner];
        assert_eq!(window.last_render, at_ms(base, 12), "child: {child_owner}");
        assert_eq!(window.stream_clock, at_ms(base, 12), "child: {child_owner}");
        assert_eq!(window.redraw.last_present, Some(at_ms(base, 12)), "child: {child_owner}");
    }
}

/// A collection that meets a busy parser arms its retry floor from the fake dispatch instant, and
/// the next admission and the armed Frame deadline agree with that floor, for both roles.
#[test]
fn contention_floor_follows_the_dispatch_clock_for_both_roles() {
    for child_owner in [false, true] {
        let base = test_base();
        let (mut app, main, child) = paced_owners(base);
        let owner = if child_owner { child } else { main };
        publish_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        set_fake_now(at_ms(base, 16));
        assert!(app.admit_window_redraw(owner));
        if child_owner {
            // The child reaches the floor through the frame-unavailable adapter at the dispatch instant.
            let window = &app.windows[&owner];
            let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
            let busy =
                crate::app::visible_frame::FrameUnavailable::Contended { pane_id, images: false };
            let now = app.dispatch_now();
            app.visible_frame_unavailable(owner, busy, false, now);
        } else {
            // When: the owner is main, the production main-window contention entry point reads the clock itself.
            app.defer_redraw_on_lock_contention(false);
        }
        let floor = at_ms(base, 26);
        assert_eq!(app.windows[&owner].retry_not_before, Some(floor), "child: {child_owner}");
        let frames: Vec<_> = app
            .frame_due_work_at(fake_now())
            .into_iter()
            .filter(|work| work.owner == Some(owner) && work.cause == DueCause::Frame)
            .collect();
        assert_eq!(frames.len(), 1, "child: {child_owner}");
        assert_eq!(frames[0].deadline, floor, "child: {child_owner}");
        set_fake_now(floor - Duration::from_nanos(1));
        assert!(!app.admit_window_redraw(owner), "refused before the floor (child: {child_owner})");
        set_fake_now(floor);
        assert!(app.admit_window_redraw(owner), "admitted at the floor (child: {child_owner})");
    }
}

/// One completion case: the outcome, the software policy, whether the snapshot carries new input,
/// whether a contention floor is armed, and whether the streaming clock must stay put.
struct CompletionCase {
    outcome: FrameSettlement,
    software: bool,
    new_input: bool,
    retry_armed: bool,
    exempt: bool,
}

/// The settlement counter an outcome raises.
fn settlement_count(app: &App, id: WindowId, outcome: FrameSettlement) -> u64 {
    let counters = counters_of(app, id);
    match outcome {
        FrameSettlement::Presented => counters.presented,
        FrameSettlement::Cached => counters.cached,
        FrameSettlement::Settled => counters.settled,
        FrameSettlement::Retry(_) => counters.retry,
        FrameSettlement::SurfaceRetry(_) => counters.surface_retry,
        FrameSettlement::Stopped(_) => counters.stopped,
        FrameSettlement::Failed => counters.failed,
    }
}

/// The sum of every settlement counter, so a test sees a second settlement of any kind.
fn settlement_total(app: &App, id: WindowId) -> u64 {
    let counters = counters_of(app, id);
    counters.presented
        + counters.cached
        + counters.settled
        + counters.retry
        + counters.surface_retry
        + counters.stopped
        + counters.failed
}

/// Completion through the adapter keeps the hardware new-input exemption, settles exactly once,
/// stamps the fake instant, and settles captured output only for drawing outcomes, for both roles.
#[test]
fn completion_through_the_adapters_keeps_the_input_exemption_and_settles_once() {
    let hardware_input = |outcome, exempt| CompletionCase {
        outcome,
        software: false,
        new_input: true,
        retry_armed: false,
        exempt,
    };
    let mut cases = vec![
        hardware_input(FrameSettlement::Settled, true),
        CompletionCase { software: true, ..hardware_input(FrameSettlement::Settled, false) },
        hardware_input(FrameSettlement::Cached, false),
        CompletionCase { new_input: false, ..hardware_input(FrameSettlement::Settled, false) },
        CompletionCase { retry_armed: true, ..hardware_input(FrameSettlement::Settled, false) },
        hardware_input(FrameSettlement::Presented, false),
        hardware_input(FrameSettlement::Retry(RedrawCause::AtlasRetry), false),
        hardware_input(FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout), false),
        hardware_input(FrameSettlement::Failed, false),
    ];
    cases.push(hardware_input(FrameSettlement::Stopped(3), false));
    for child_owner in [false, true] {
        for case in &cases {
            let base = test_base();
            let (mut app, main, child) = paced_owners(base);
            let owner = if child_owner { child } else { main };
            let label = format!(
                "child={child_owner} {:?} software={} input={} retry={}",
                case.outcome, case.software, case.new_input, case.retry_armed
            );
            app.software_render_degrade = case.software;
            if case.retry_armed {
                app.windows.get_mut(&owner).unwrap().retry_not_before = Some(at_ms(base, 100));
            }
            let cause = if case.new_input { RedrawCause::Input } else { RedrawCause::Expose };
            app.mark_window_redraw(owner, cause);
            publish_output(&app, owner);
            set_fake_now(at_ms(base, 5));
            let snapshot = app.snapshot_window_redraw_at(owner, fake_now()).unwrap();
            let outcome_before = settlement_count(&app, owner, case.outcome);
            let total_before = settlement_total(&app, owner);
            set_fake_now(at_ms(base, 7));
            app.complete_window_redraw(owner, &snapshot, case.outcome);
            let window = &app.windows[&owner];
            assert_eq!(window.last_render, at_ms(base, 7), "{label}");
            let expected_stream = if case.exempt { base } else { at_ms(base, 7) };
            assert_eq!(window.stream_clock, expected_stream, "{label}");
            assert_eq!(
                counters_of(&app, owner).stream_clock_exempt,
                u64::from(case.exempt),
                "{label}"
            );
            assert_eq!(settlement_count(&app, owner, case.outcome), outcome_before + 1, "{label}");
            assert_eq!(settlement_total(&app, owner), total_before + 1, "{label}");
            assert!(
                !window.redraw.captures_new_input(snapshot.causes),
                "{label}: input observed once"
            );
            let draws = matches!(
                case.outcome,
                FrameSettlement::Presented | FrameSettlement::Cached | FrameSettlement::Settled
            );
            assert_eq!(window.visible_output_advanced(), !draws, "{label}: output settlement");
        }
    }
}

/// The completion adapter adds no second clock or settlement writer: it reaches `complete_attempt`
/// only through `finish_window_redraw`, exactly once, and settles nothing itself.
#[test]
fn the_completion_adapter_has_one_settlement_path() {
    let source = include_str!("redraw.rs").replace("\r\n", "\n");
    let adapter = method_body(&source, "pub(super) fn complete_window_redraw(");
    assert_eq!(adapter.matches("finish_window_redraw(").count(), 1, "{adapter}");
    for forbidden in [".settle(", "last_render =", "stream_clock =", "complete_attempt("] {
        assert!(!adapter.contains(forbidden), "the adapter must not use {forbidden} itself");
    }
    let finish = method_body(&source, "pub(super) fn finish_window_redraw(");
    assert_eq!(finish.matches("complete_attempt(").count(), 1, "{finish}");
    assert!(!finish.contains(".settle("), "{finish}");
}
