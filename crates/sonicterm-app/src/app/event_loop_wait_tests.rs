//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::event_loop_tests::*;
use super::*;

/// Recovery maintenance never requests a frame or suppresses a coincident or delayed owner deadline.
#[test]
fn recovery_wakes_preserve_due_owner_frames_without_an_idle_heartbeat() {
    use crate::app::redraw::{DueCause, DueWork};
    for frame_offset in [Duration::ZERO, Duration::from_millis(10)] {
        let mut app = app_with_main_window();
        let owner = app.main_window_id.unwrap();
        let now = Instant::now();
        let frame_at = now + frame_offset;
        app.redraw_due = vec![
            DueWork { owner: None, cause: DueCause::GpuRecovery, deadline: now },
            DueWork { owner: Some(owner), cause: DueCause::Frame, deadline: frame_at },
        ];
        app.service_redraw_due(now);
        assert_eq!(app.windows[&owner].redraw.request_in_flight, frame_offset.is_zero());
        app.service_redraw_due(frame_at + Duration::from_millis(1));
        assert!(app.windows[&owner].redraw.request_in_flight);
        assert!(app.redraw_due.is_empty());
    }
    let mut app = app_with_main_window();
    let owner = app.main_window_id.unwrap();
    let now = Instant::now();
    app.request_device_state_redraws();
    assert!(!app.windows[&owner].redraw.request_in_flight);
    app.redraw_due.push(DueWork { owner: None, cause: DueCause::GpuRecovery, deadline: now });
    app.service_redraw_due(now);
    assert!(!app.windows[&owner].redraw.request_in_flight);
    assert!(app.redraw_due.is_empty());
}

#[test]
fn contended_redraw_after_idle_arms_a_future_deadline_without_faking_a_frame() {
    // A failed collection after idle needs a future retry, not the expired successful-frame deadline.
    let mut app = app_with_main_window();
    let now = Instant::now();
    let previous = now - Duration::from_secs(1);
    app.main_mut().unwrap().last_render = previous;
    app.main_mut().unwrap().stream_clock = previous;

    app.defer_redraw_on_lock_contention(true);

    assert!(app.wake_deadline(None).unwrap() > now);
    assert_eq!(app.main().unwrap().last_render, previous);
    assert_eq!(app.main().unwrap().stream_clock, previous, "contention moves neither clock");
    assert!(app.pending_redraw);
    assert!(app.__test_input_dirty());
}

#[test]
fn quit_confirmation_deadline_survives_a_pending_main_window_redraw() {
    // A Cmd+Q confirmation armed just under its full window ago: it expires
    // shortly, well before the next frame boundary.
    let now = Instant::now();
    let mut app = app_with_main_window();
    let armed_at = now - (QUIT_CONFIRM_DURATION - Duration::from_millis(10));
    let quit_deadline = match app.quit_hold.on_press(armed_at, false) {
        crate::app::quit_hold::QuitHoldAction::ShowPrompt { deadline } => deadline,
        other => panic!("first press must arm the confirmation, got {other:?}"),
    };

    // A redraw deferred for vsync pacing, rendered `now`.
    arm_pending_redraw_composing(&mut app, now);
    let frame_boundary = now + COMPOSE_PERIOD;

    // The two are unambiguously apart: quit expires ~10ms out, the frame
    // boundary ~83ms out.
    assert!(
        quit_deadline < frame_boundary,
        "test setup: quit deadline must precede the frame boundary"
    );

    // A deadline is a "wake no later than" bound, so the earliest contributor
    // wins. Taking the frame boundary here would let the confirmation window
    // outlive its due instant by up to one frame period.
    assert_eq!(
        app.wake_deadline(None),
        Some(quit_deadline),
        "pending redraw must min-fold with the quit deadline, not replace it"
    );
}

#[cfg(windows)]
#[test]
fn foreground_probe_input_deadline_is_not_postponed_by_output() {
    // Protect a silent gsudo launch from losing its probe behind a continuing output burst.
    let start = Instant::now();
    let mut app = app_with_main_window();

    app.arm_foreground_probe_after_input(start);
    app.arm_foreground_probe_after_output(start + Duration::from_millis(400));

    let wake = app.foreground_schedule.activity_wake.expect("input arms a probe wake");
    assert_eq!(wake.due, start + FOREGROUND_PROCESS_TTL);
    assert!(wake.fixed);

    app.arm_foreground_probe_after_input(start + Duration::from_millis(450));
    assert_eq!(
        app.foreground_schedule.activity_wake.expect("later input rearms the fixed wake").due,
        start + Duration::from_millis(450) + FOREGROUND_PROCESS_TTL
    );
}

#[cfg(windows)]
#[test]
fn due_foreground_probe_forces_shell_return_refresh() {
    // Protect a silent command return from retaining its per-tab warning after the cache TTL.
    let start = Instant::now();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("#1 pwsh");
    let main_id = app.main_window_id.expect("synthetic main window");
    let window = app.windows.get_mut(&main_id).expect("synthetic main state");
    window.panes.get_mut(&pane_id).expect("synthetic pane").fg_proc_cache = Some((
        start,
        Some(sonicterm_io::proc_info::ForegroundProcess { name: "gsudo".into(), privileged: true }),
    ));
    assert!(window.tabs.set_foreground_privileged(0, true));
    app.finish_foreground_process_probe(start, true);

    let due = start + FOREGROUND_PROCESS_TTL;
    let changed = app.refresh_foreground_privileges_if_due(due);

    assert_eq!(changed, vec![main_id]);
    assert!(!app.windows[&main_id].tabs.tabs()[0].foreground_privileged);
    assert!(app.foreground_schedule.warning_wake.is_none());
}

#[cfg(windows)]
#[test]
fn foreground_probe_only_wake_yields_to_frame_deadlines() {
    // Protect timer-only samples from repainting unless no frame is already due at that instant.
    let now = Instant::now();
    let probe = now + Duration::from_millis(500);
    let memory = now + Duration::from_secs(30);

    assert!(wake_is_foreground_probe_only(None, Some(probe), Some(memory)));
    assert!(wake_is_foreground_probe_only(None, Some(probe), Some(probe)));
    assert!(!wake_is_foreground_probe_only(Some(probe), Some(probe), Some(memory)));
    assert!(!wake_is_foreground_probe_only(
        Some(now + Duration::from_millis(100)),
        Some(probe),
        Some(memory)
    ));
}

/// Degraded main and child scrollbars contribute their earliest one-shot deadline.
#[test]
fn snap_scrollbars_join_the_render_wake_minimum() {
    let now = Instant::now();
    let mut app = app_with_main_window();
    app.software_render_degrade = true;
    app.config.appearance.scrollbar = sonicterm_cfg::config::ScrollbarMode::Auto;
    let main_id = crate::app::synthetic_main_window_id();
    let child_id = app.__test_seed_child_window(&["child"]);
    let child_pane = app.__test_child_active_pane(child_id).expect("child pane");
    arm_snap_scrollbar(&mut app, main_id, 1, now);
    arm_snap_scrollbar(&mut app, child_id, child_pane, now - Duration::from_millis(200));
    let child_deadline = now - Duration::from_millis(200)
        + Duration::from_millis(crate::app::scrollbar_visibility::IDLE_HIDE_MS);

    assert_eq!(app.wake_deadline(None), Some(child_deadline));
    assert_eq!(
        app.wake_deadline(Some(now + Duration::from_millis(10))),
        Some(now + Duration::from_millis(10)),
        "an earlier notification must remain ahead of scrollbar expiration"
    );
}

/// The Windows OSC 52 repair participates in the event-loop wake fold.
#[cfg(target_os = "windows")]
#[test]
fn pending_osc52_reassertion_arms_its_deadline() {
    let mut app = app_with_main_window();
    app.__test_set_memory_clipboard("old");
    app.handle_clipboard_write("Copilot selection".into());
    let due = app
        .pending_osc52_reassert
        .as_ref()
        .expect("successful OSC 52 write arms a reassertion")
        .due;

    assert_eq!(app.wake_deadline(None), Some(due));
}
