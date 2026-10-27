//! Owner-local visibility and idle image-atlas release contracts.

use super::super::redraw_tests::*;
use super::*;

/// A due frame and a due release together: the frame implies `deferred`, so the release is skipped and
/// the frame is requested, in either order.
#[test]
pub(super) fn a_coinciding_frame_deadline_skips_the_release() {
    let now = Instant::now();
    for frame_first in [true, false] {
        let (mut app, main) = idle_release_owner(now);
        app.windows.get_mut(&main).unwrap().redraw.deferred = true;
        let frame = DueWork { owner: Some(main), cause: DueCause::Frame, deadline: now };
        let release =
            DueWork { owner: Some(main), cause: DueCause::ImageAtlasRelease, deadline: now };
        app.redraw_due = if frame_first { vec![frame, release] } else { vec![release, frame] };
        app.service_redraw_due(now);
        assert_eq!(releases(&app, main), 0, "frame_first={frame_first}");
        assert!(app.windows[&main].redraw.request_in_flight, "the frame is requested");
    }
}

/// A firing whose deadline is gone (media returned, or the atlas was already released) or later than
/// now does nothing.
#[test]
pub(super) fn a_stale_release_firing_does_nothing() {
    let now = Instant::now();
    for deadline in [None, Some(now + Duration::from_secs(1))] {
        let (mut app, main) = idle_release_owner(now);
        app.windows.get_mut(&main).unwrap().test_image_atlas_release.as_mut().unwrap().deadline =
            deadline;
        app.redraw_due =
            vec![DueWork { owner: Some(main), cause: DueCause::ImageAtlasRelease, deadline: now }];
        app.service_redraw_due(now);
        assert_eq!(releases(&app, main), 0);
    }
}

/// The pane's decoded media is a separate allocation from the renderer's atlas: a serviced release leaves
/// the pane's `InlineMediaRetained` charge exactly as it was, both before and after the next charge.
#[test]
pub(super) fn an_image_atlas_release_leaves_the_pane_media_charge_unchanged() {
    let now = Instant::now();
    let (mut app, main) = idle_release_owner(now);
    let pane_id = *app.windows[&main].panes.keys().next().unwrap();
    let image = sonicterm_render_model::InlineImage {
        id: 1,
        row: 0,
        col: 0,
        width: 32,
        height: 32,
        bgra: Arc::from(vec![0; 32 * 32 * 4]),
    };
    assert!(app.__test_set_pane_inline_images(main, pane_id, vec![image]));
    app.reconcile_pane_owners();
    app.__test_charge_pane_owners();
    let media = |app: &App| {
        app.__test_pane_charges(main, pane_id).unwrap()
            [&sonicterm_types::ResourceClass::InlineMediaRetained]
    };
    let charged = media(&app);
    assert!(charged.bytes >= 32 * 32 * 4, "precondition: the decoded image is charged");
    assert_eq!(app.__test_collect_and_service_redraw_due(now), 1, "the release is collected");
    assert_eq!(releases(&app, main), 1, "and serviced");
    assert_eq!(media(&app), charged, "the release itself moves no pane charge");
    app.__test_charge_pane_owners();
    assert_eq!(media(&app), charged, "nor does the next charge");
}

/// The gated sampling seam keeps production's interval: a pass inside it does nothing, and the first pass
/// at or past the interval runs.
#[test]
pub(super) fn the_gated_retention_seam_keeps_the_sampling_interval() {
    let (mut app, _main) = idle_release_owner(Instant::now());
    let start = Instant::now();
    assert!(app.__test_sample_pane_retention_at(start), "the first pass always runs");
    let interval = super::super::super::retention::RETENTION_SAMPLE_INTERVAL;
    assert!(!app.__test_sample_pane_retention_at(start + interval - Duration::from_millis(1)));
    assert!(app.__test_sample_pane_retention_at(start + interval));
}

/// Output in a background tab of a visible window requests no frame and marks no cause, and the
/// token is acknowledged; a command badge that changes in that tab still requests exactly one.
#[test]
pub(super) fn background_tab_output_requests_no_frame_but_a_badge_change_requests_one() {
    use crate::app::output_event::OutputEvent;
    use std::sync::atomic::Ordering;
    let (mut app, _, child) = owners();
    let pane = app.windows[&child].tab_states[1].active_pane;
    let state = &app.windows[&child].panes[&pane];
    state.output_generation.fetch_add(1, Ordering::Release);
    state.output_outstanding.store(true, Ordering::Release);
    let causes = app.windows[&child].redraw.snapshot();
    let before = crate::app::window_state::window_redraw_requests();

    app.service_output_event(OutputEvent::Pane { window_id: child, pane_id: pane }, Instant::now());

    assert_eq!(crate::app::window_state::window_redraw_requests(), before, "no native request");
    assert_eq!(app.windows[&child].redraw.snapshot(), causes, "no cause marked");
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert!(!app.windows[&child].panes[&pane].output_outstanding.load(Ordering::Acquire));

    let now = Instant::now();
    let state = &app.windows[&child].panes[&pane];
    state.command_events.lock().push(crate::app::PaneCommandEvent {
        event: sonicterm_vt::vt::CommandEvent::CmdEnd(Some(0)),
        at: now,
        duration: None,
    });
    state.output_outstanding.store(true, Ordering::Release);
    let output = app.windows[&child].redraw.cause_generation(RedrawCause::Output);

    app.service_output_event(OutputEvent::Pane { window_id: child, pane_id: pane }, now);

    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1, "one request");
    assert!(app.windows[&child].redraw.request_in_flight);
    assert!(
        app.windows[&child].redraw.cause_generation(RedrawCause::Chrome)
            > causes.0[RedrawCause::Chrome as usize]
    );
    assert_eq!(app.windows[&child].redraw.cause_generation(RedrawCause::Output), output);
}

/// An explicit request for a window whose output is settled (as the harness sends after clearing
/// its retained frame) still makes exactly one native request with an `Output` cause.
#[test]
pub(super) fn explicit_request_with_settled_output_requests_one_output_frame() {
    use crate::app::output_event::OutputEvent;
    let (mut app, _, child) = owners();
    let pane = app.windows[&child].tab_states[0].active_pane;
    let state = &app.windows[&child].panes[&pane];
    assert_eq!(
        state.output_generation.load(std::sync::atomic::Ordering::Acquire),
        state.observed_output_generation
    );
    if let Some(renderer) = app.windows.get_mut(&child).unwrap().renderer.as_mut() {
        renderer.invalidate_retained_frame();
    }
    let output = app.windows[&child].redraw.cause_generation(RedrawCause::Output);
    let before = crate::app::window_state::window_redraw_requests();

    app.service_output_event(OutputEvent::Explicit(child), Instant::now());

    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1);
    assert!(app.windows[&child].redraw.cause_generation(RedrawCause::Output) > output);
}

/// A hardware keypress frame that presented nothing must not delay the shell's echo by one period.
#[test]
pub(super) fn echo_after_a_settled_keypress_is_not_paced_from_the_keypress() {
    use super::super::super::frame_counters::DeferRule;
    for child_owner in [false, true] {
        let (mut app, main, child) = counting_owners();
        let owner = if child_owner { child } else { main };
        let keypress_at = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, owner, keypress_at - Duration::from_secs(1));
        // An output-only frame presented 20 ms before the keypress.
        publish_visible_output(&app, owner);
        admitted_attempt(
            &mut app,
            owner,
            RedrawCause::Output,
            FrameSettlement::Presented,
            keypress_at - Duration::from_millis(20),
        );
        // The keypress frame finds no echo yet and settles without presenting.
        admitted_attempt(
            &mut app,
            owner,
            RedrawCause::Input,
            FrameSettlement::Settled,
            keypress_at,
        );
        // The echo arrives 4 ms later: 24 ms after the last streaming frame, more than a 60 Hz period.
        publish_visible_output(&app, owner);
        app.mark_window_redraw(owner, RedrawCause::Output);
        assert!(
            app.begin_window_redraw(owner, keypress_at + Duration::from_millis(4)),
            "the echo is admitted at once (child: {child_owner})"
        );
        assert_eq!(defer_count(&app, owner, DeferRule::Streaming), 0);
        let counters = app.windows[&owner].redraw.frame_counters.as_deref().unwrap();
        assert_eq!(counters.stream_clock_exempt, 1, "only the keypress kept the streaming clock");
    }
}

/// For every deferral rule, the armed Frame deadline is the first instant begin admits.
#[test]
pub(super) fn frame_deadlines_match_admission_for_every_rule() {
    use super::super::super::frame_counters::DeferRule;
    let period = Duration::from_micros(16_667);
    for case in 0..4 {
        let (mut app, main, _) = counting_owners();
        let start = Instant::now() + Duration::from_secs(1);
        backdate_clocks(&mut app, main, start - Duration::from_secs(1));
        let (rule, deadline, probe) = match case {
            0 => {
                // Streaming after a presented input frame.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Presented,
                    start,
                );
                (DeferRule::Streaming, start + period, start + Duration::from_millis(1))
            }
            1 => {
                // Streaming after an exempt settled keypress that followed a non-input attempt at `start`.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Output,
                    FrameSettlement::Presented,
                    start,
                );
                let keypress = start + Duration::from_millis(5);
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Settled,
                    keypress,
                );
                (DeferRule::Streaming, start + period, keypress + Duration::from_millis(1))
            }
            2 => {
                // A surface timeout waits one period from the attempt.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
                    start,
                );
                (DeferRule::Timeout, start + period, start + Duration::from_millis(1))
            }
            _ => {
                // A contention floor later than the paced instant wins.
                admitted_attempt(
                    &mut app,
                    main,
                    RedrawCause::Input,
                    FrameSettlement::Presented,
                    start,
                );
                app.windows.get_mut(&main).unwrap().retry_not_before = Some(start + period * 2);
                (DeferRule::Contention, start + period * 2, start + Duration::from_millis(1))
            }
        };
        publish_visible_output(&app, main);
        app.mark_window_redraw(main, RedrawCause::Output);
        assert!(!app.begin_window_redraw(main, probe), "case {case} defers");
        let frames: Vec<_> = app
            .frame_due_work_at(probe)
            .into_iter()
            .filter(|work| work.owner == Some(main) && work.cause == DueCause::Frame)
            .collect();
        assert_eq!(frames.len(), 1, "case {case} arms one frame deadline");
        assert_eq!(frames[0].deadline, deadline, "case {case} deadline");
        let before = defer_count(&app, main, rule);
        assert!(!app.begin_window_redraw(main, deadline - Duration::from_nanos(1)), "case {case}");
        assert_eq!(defer_count(&app, main, rule), before + 1, "case {case} defers with {rule:?}");
        assert!(app.begin_window_redraw(main, deadline), "case {case} admits at its deadline");
    }
}

/// Both roles complete through `complete_window_redraw`, the seam the behavioural tests drive, passing
/// the renderer's own settlement; the adapter stamps the dispatch clock, and neither handler writes a
/// clock, settles a cause or picks a software policy itself, so a wrong argument cannot hide in a
/// second writer.
#[test]
pub(super) fn main_and_child_completions_share_one_clock_writer() {
    let child_source = include_str!("child_window_redraw.rs").replace("\r\n", "\n");
    let main_source = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    )
    .replace("\r\n", "\n");
    let redraw_source = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    )
    .replace("\r\n", "\n");
    let child_call = "self.complete_window_redraw(win_id, snapshot, settlement);";
    assert_eq!(child_source.matches("complete_window_redraw(").count(), 1, "one child completion");
    assert!(child_source.contains(child_call), "the child passes its own snapshot and outcome");
    assert!(!child_source.contains("finish_window_redraw("), "the child uses the adapter");
    assert!(
        child_source.contains("let settlement = super::redraw::FrameSettlement::of(&outcome);"),
        "the child's outcome is the renderer's"
    );
    assert!(
        child_source.contains("frame_completion = Some(settlement);"),
        "the child records only the settlement; the adapter takes the instant"
    );
    for inline in [
        ".complete_attempt(",
        "last_render =",
        "stream_clock",
        ".settle(",
        "software_render_degrade",
    ] {
        assert!(!child_source.contains(inline), "the child adapter must not use {inline} itself");
    }
    assert_eq!(
        main_source.matches("self.complete_window_redraw(win_id, snapshot, outcome);").count(),
        1
    );
    assert!(!main_source.contains("finish_window_redraw("), "the main handler uses the adapter");
    assert!(
        method_body(&redraw_source, "fn complete_window_redraw(")
            .contains("let at = self.dispatch_now();"),
        "the adapter stamps the dispatch clock"
    );
    assert!(
        method_body(&redraw_source, "fn finish_window_redraw(").contains(
            "window.complete_attempt(snapshot, outcome, at, self.software_render_degrade);"
        ),
        "completion reads the App's software policy, never a caller's"
    );
}
