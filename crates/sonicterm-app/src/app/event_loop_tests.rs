//! Unit tests for the event loop's wake-deadline fold.
//!
//! `do_about_to_wait` itself needs an `ActiveEventLoop`, which only exists
//! inside a running winit loop, so these drive [`App::wake_deadline`] — the
//! winit-free half that computes the instant `do_about_to_wait` then hands to
//! `set_control_flow`.

use super::*;
use crate::app::quit_hold::QUIT_CONFIRM_DURATION;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// Software-render + IME-composing frame period, the widest gap between a
/// frame boundary and any other contributor.
const COMPOSE_PERIOD: Duration = crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD;

fn app_with_main_window() -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    app
}

/// Put the main window on the software + IME-composing frame cadence, so its
/// deferred-redraw boundary sits `COMPOSE_PERIOD` after `last_render`.
fn arm_pending_redraw_composing(app: &mut App, last_render: Instant) {
    app.software_render_degrade = true;
    app.pending_redraw = true;
    let ws = app.main_mut().expect("synthetic main window");
    ws.last_render = last_render;
    ws.ime.handle_preedit("あ", Some((0, 1)));
    assert!(ws.ime.is_composing(), "preedit must put the window on the composing cadence");
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

#[test]
fn notification_expiry_survives_a_pending_main_window_redraw() {
    // Same shape for the other contributor computed before the redraw branch:
    // a notification expiring before the next frame boundary.
    let now = Instant::now();
    let mut app = app_with_main_window();
    arm_pending_redraw_composing(&mut app, now);
    let notification_wake = now + Duration::from_millis(10);

    assert_eq!(
        app.wake_deadline(Some(notification_wake)),
        Some(notification_wake),
        "pending redraw must min-fold with the notification expiry, not replace it"
    );
}

#[test]
fn pending_main_window_redraw_wins_when_it_is_the_earliest() {
    // The fold is a minimum, not a demotion: when the frame boundary is the
    // earliest contributor it is still the one that gets scheduled.
    let now = Instant::now();
    let mut app = app_with_main_window();
    arm_pending_redraw_composing(&mut app, now);
    let frame_boundary = now + COMPOSE_PERIOD;
    // Quit confirmation armed `now`, so it expires a full window out — long
    // after the frame boundary.
    let _ = app.quit_hold.on_press(now, false);

    assert_eq!(
        app.wake_deadline(None),
        Some(frame_boundary),
        "the earliest contributor wins, and here that is the frame boundary"
    );
}

#[test]
fn no_armed_contributor_parks_the_loop() {
    // Nothing armed: `None` is what makes `do_about_to_wait` choose
    // `ControlFlow::Wait` and drive no wakes at all while idle.
    let app = app_with_main_window();
    assert_eq!(app.wake_deadline(None), None);
}
