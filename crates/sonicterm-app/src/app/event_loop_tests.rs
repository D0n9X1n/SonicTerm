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

/// Drive a run of frames and return the gap between each consecutive pair.
///
/// Each iteration asks the pacing path for the next wake, then advances
/// `last_render` to it — which is what the render path does when that wake
/// fires. The returned gaps are therefore the cadence the loop would actually
/// run at, not a restatement of the constant that produced it.
fn measure_frame_gaps(app: &mut App, frames: usize, start: Instant) -> Vec<Duration> {
    let mut gaps = Vec::with_capacity(frames);
    let mut last = start;
    for _ in 0..frames {
        app.pending_redraw = true;
        {
            let ws = app.main_mut().expect("synthetic main window");
            ws.last_render = last;
        }
        let at = app.wake_deadline(None).expect("a pending redraw must arm a wake");
        gaps.push(at.duration_since(last));
        last = at;
    }
    gaps
}

/// A 120 Hz monitor is genuinely slowed to the software cap, measured.
///
/// The issue this closes asks for the *cadence*, not the constant: a test that
/// asserts `SOFTWARE_RENDER_FRAME_PERIOD == 25ms` proves the constant has the
/// value it has. This drives a run of frames through the pacing path at a
/// 120 Hz monitor period and measures every gap, so a monitor period passed
/// through unclamped — the actual failure — shows up as an 8.3 ms gap.
///
/// The whole distribution is checked rather than an average. A related v1.2.0
/// finding reported "~11 resets/sec sustained" which turned out to be bursts
/// at the frame period with nothing in the final 21 seconds; an average hides
/// exactly the shape that matters.
#[test]
fn a_120hz_monitor_is_slowed_to_the_software_cap() {
    const HZ_120: Duration = Duration::from_micros(8_333);
    let mut app = app_with_main_window();
    app.software_render_degrade = true;
    app.frame_period = HZ_120;

    let gaps = measure_frame_gaps(&mut app, 32, Instant::now());

    assert_eq!(gaps.len(), 32, "the run must produce a gap per frame");
    for (index, gap) in gaps.iter().enumerate() {
        assert_eq!(
            *gap,
            crate::app::SOFTWARE_RENDER_FRAME_PERIOD,
            "frame {index} of {} ran at {gap:?}; the software path must not rasterize at the \
             monitor's {HZ_120:?} period",
            gaps.len()
        );
    }
    assert!(
        gaps.iter().all(|g| *g > HZ_120),
        "every gap must be wider than the monitor period, or nothing was capped"
    );
}

/// The hardware path is left at the monitor's own period.
///
/// The control for the test above: without it, a pacing path that returned the
/// software cap unconditionally would satisfy the cap assertion and silently
/// cap a machine with a real GPU at 40 fps.
#[test]
fn a_120hz_monitor_is_untouched_on_the_hardware_path() {
    const HZ_120: Duration = Duration::from_micros(8_333);
    let mut app = app_with_main_window();
    app.software_render_degrade = false;
    app.frame_period = HZ_120;

    let gaps = measure_frame_gaps(&mut app, 8, Instant::now());

    for (index, gap) in gaps.iter().enumerate() {
        assert_eq!(
            *gap, HZ_120,
            "frame {index} ran at {gap:?}; a hardware path must present at the monitor period"
        );
    }
}

/// The IME drop engages while composing **and releases afterwards**.
///
/// The release is the half worth hunting: a cap stuck at ~12 fps after
/// composition ends would make every later keystroke feel heavy, and the path
/// never runs on macOS so no developer would meet it. Driven as a sequence —
/// normal, composing, normal — because a stuck cap is only visible as a
/// transition that fails to come back.
#[test]
fn the_ime_compose_drop_engages_and_then_releases() {
    const HZ_120: Duration = Duration::from_micros(8_333);
    let mut app = app_with_main_window();
    app.software_render_degrade = true;
    app.frame_period = HZ_120;

    let before = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        before.iter().all(|g| *g == crate::app::SOFTWARE_RENDER_FRAME_PERIOD),
        "precondition: the software cap must be in effect before composing, got {before:?}"
    );

    {
        let ws = app.main_mut().expect("synthetic main window");
        ws.ime.handle_preedit("あ", Some((0, 1)));
        assert!(ws.ime.is_composing(), "the preedit must put the window on the compose cadence");
    }
    let during = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        during.iter().all(|g| *g == crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD),
        "composing must drop to the compose cadence, got {during:?}"
    );

    {
        let ws = app.main_mut().expect("synthetic main window");
        ws.ime.handle_commit("あ");
        assert!(!ws.ime.is_composing(), "the commit must end composition");
    }
    let after = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        after.iter().all(|g| *g == crate::app::SOFTWARE_RENDER_FRAME_PERIOD),
        "the compose cadence must be released once composition ends; a cap stuck at {:?} \
         makes every later keystroke feel heavy and never reproduces on macOS. Got {after:?}",
        crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD
    );
}

/// The worst-case latency a deferred keystroke waits, stated as a number.
///
/// The issue asks whether typing at 40 fps "feels acceptable", which is a
/// judgement. What can be established is the quantity the judgement is about:
/// a keystroke arriving just after a frame waits at most one frame period, so
/// the software path adds at most 25 ms and the compose path at most ~83 ms.
/// Measured through the pacing path rather than restated from the constants.
#[test]
fn a_deferred_keystroke_waits_at_most_one_frame_period() {
    const HZ_120: Duration = Duration::from_micros(8_333);
    let mut app = app_with_main_window();
    app.software_render_degrade = true;
    app.frame_period = HZ_120;

    let rendered_at = Instant::now();
    // A keystroke arriving one microsecond after a frame: the worst case, with
    // almost a whole period left to wait.
    let keystroke_at = rendered_at + Duration::from_micros(1);
    app.pending_redraw = true;
    {
        let ws = app.main_mut().expect("synthetic main window");
        ws.last_render = rendered_at;
    }

    let wake = app.wake_deadline(None).expect("a pending redraw must arm a wake");
    let waited = wake.duration_since(keystroke_at);

    assert!(
        waited <= crate::app::SOFTWARE_RENDER_FRAME_PERIOD,
        "a keystroke waited {waited:?}, longer than the {:?} frame period it should never \
         exceed",
        crate::app::SOFTWARE_RENDER_FRAME_PERIOD
    );
    println!(
        "worst-case added latency: software {:?}, compose {:?}",
        crate::app::SOFTWARE_RENDER_FRAME_PERIOD,
        crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD
    );
}
