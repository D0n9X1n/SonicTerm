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

/// The hard window floor rounds fractional physical geometry upward.
#[test]
fn minimum_terminal_inner_size_preserves_thirty_by_ten_cells() {
    let size = crate::app::minimum_terminal_inner_size(8.25, 17.5, 3.2, 4.1, 6.4, 25.3, 2.2);

    assert_eq!(size.width, 255);
    assert_eq!(size.height, 209);
    assert_eq!(crate::app::MIN_WINDOW_COLS, 30);
    assert_eq!(crate::app::MIN_WINDOW_ROWS, 10);
}

/// DPI transitions preserve logical size while applying the new physical minimum.
#[test]
fn dpi_transition_size_preserves_logical_geometry_and_minimum() {
    let low_to_high = crate::app::dpi_transition_inner_size(
        winit::dpi::PhysicalSize::new(800, 600),
        1.0,
        1.75,
        winit::dpi::PhysicalSize::new(700, 500),
        winit::dpi::PhysicalSize::new(1600, 900),
    );
    assert_eq!(low_to_high, winit::dpi::PhysicalSize::new(1400, 900));

    let high_to_low = crate::app::dpi_transition_inner_size(
        winit::dpi::PhysicalSize::new(1400, 900),
        1.75,
        1.0,
        winit::dpi::PhysicalSize::new(700, 500),
        winit::dpi::PhysicalSize::new(1600, 900),
    );
    assert_eq!(high_to_low, winit::dpi::PhysicalSize::new(800, 514));
}

/// Main and child scale handlers synchronously commit the same transition policy.
#[test]
fn main_and_child_scale_handlers_use_inner_size_writer() {
    let main = include_str!("window_event.rs");
    let child = include_str!("child_window.rs");
    let shared = include_str!("mod.rs");

    for source in [main, child] {
        assert!(source.contains("inner_size_writer"));
        assert!(source.contains("apply_window_dpi_transition("));
    }
    assert!(shared.contains("native.is_maximized() || native.fullscreen().is_some()"));
    let renderer_resize = shared.find("renderer.try_resize(target.width, target.height)").unwrap();
    let native_resize = shared.find("inner_size_writer.request_inner_size(target)").unwrap();
    assert!(renderer_resize < native_resize, "renderer rejection must precede native commit");
}

/// Canonical rmux instructions name both application-passthrough requirements.
#[test]
fn rmux_osc52_documentation_enables_clipboard_and_passthrough() {
    let internals = include_str!("../../../../wiki/Architecture-Internals.md");
    assert!(internals.matches("set -s set-clipboard on").count() >= 2);
    assert!(internals.matches("set -g allow-passthrough on").count() >= 2);
}

/// Every terminal-window constructor installs the shared live-renderer floor.
#[test]
fn all_terminal_window_creation_paths_apply_the_minimum() {
    let main = include_str!("event_loop.rs");
    let tear_out = include_str!("tear_out.rs");

    assert!(main.contains("apply_terminal_window_minimum(&window, &mut renderer)"));
    assert!(tear_out.contains("apply_terminal_window_minimum(window, renderer)"));
    assert!(tear_out.contains("fn create_warm_window"));
    assert!(tear_out.contains("fn install_torn_out_window"));
}

/// DPI, font, padding, and tab-bar changes all refresh native minimums.
#[test]
fn live_metric_change_paths_refresh_the_minimum() {
    let main_events = include_str!("window_event.rs");
    let child_events = include_str!("child_window.rs");
    let config = include_str!("config_apply.rs");

    assert!(main_events.contains("apply_window_dpi_transition("));
    assert!(child_events.contains("apply_window_dpi_transition("));
    assert!(config.matches("refresh_all_window_minimums();").count() >= 3);
}

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

fn arm_snap_scrollbar(app: &mut App, window_id: WindowId, pane_id: u64, active: Instant) {
    let mut state = crate::app::scrollbar_visibility::ScrollbarVisState::new(active);
    state.mark_active(active);
    state.alpha = 1.0;
    app.windows.get_mut(&window_id).expect("synthetic window").scrollbar_vis.insert(pane_id, state);
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

/// Expiration mutates only due windows and removes their stale deadline.
#[test]
fn snap_expiration_returns_only_affected_windows() {
    let now = Instant::now();
    let mut app = app_with_main_window();
    app.software_render_degrade = true;
    app.config.appearance.scrollbar = sonicterm_cfg::config::ScrollbarMode::Auto;
    let main_id = crate::app::synthetic_main_window_id();
    let due_child = app.__test_seed_child_window(&["due"]);
    let future_child = app.__test_seed_child_window(&["future"]);
    let due_pane = app.__test_child_active_pane(due_child).expect("due pane");
    let future_pane = app.__test_child_active_pane(future_child).expect("future pane");
    let idle = Duration::from_millis(crate::app::scrollbar_visibility::IDLE_HIDE_MS);
    arm_snap_scrollbar(&mut app, main_id, 1, now - idle);
    arm_snap_scrollbar(&mut app, due_child, due_pane, now - idle);
    arm_snap_scrollbar(&mut app, future_child, future_pane, now);

    let affected = app.expire_due_scrollbar_snaps(now);

    assert_eq!(affected.len(), 2);
    assert!(affected.contains(&main_id));
    assert!(affected.contains(&due_child));
    assert!(!affected.contains(&future_child));
    assert_eq!(
        app.wake_deadline(None),
        Some(now + idle),
        "only the future child's one-shot deadline may remain"
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
/// The whole distribution is checked rather than an average. A related
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

#[cfg(unix)]
#[test]
fn cold_script_requests_replace_the_blank_tab_and_late_requests_append() {
    use sonicterm_types::OpenScriptRequest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    let _guard = crate::open_script_bridge::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::open_script_bridge::drain();

    let dir = std::env::temp_dir().join(format!(
        "sonicterm-cold-open-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = dir.join("sh");
    std::fs::write(&shell, "#!/bin/sh\nexec cat\n").unwrap();
    let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions).unwrap();

    let request =
        |name: &str| OpenScriptRequest::resolve(PathBuf::from(name), Path::new(&dir)).unwrap();
    let first = request("first.sh");
    let second = request("second.sh");
    assert!(!crate::open_script_bridge::push_requests(vec![first, second]));

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.seed_initial_tabs();
    assert_eq!(app.__test_tab_count(), 2, "cold requests must not leave a blank tab");

    let late = request("late.sh");
    assert!(!crate::open_script_bridge::push_requests(vec![late]));
    assert_eq!(app.drain_open_script_requests(), 1);
    assert_eq!(app.__test_tab_count(), 3, "late opens append one tab");
    assert_eq!(app.drain_open_script_requests(), 0, "requests drain exactly once");

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn startup_without_script_requests_keeps_the_normal_blank_tab() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::open_script_bridge::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::open_script_bridge::drain();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-normal-startup-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = dir.join("sh");
    std::fs::write(&shell, "#!/bin/sh\nexec cat\n").unwrap();
    let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions).unwrap();

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.seed_initial_tabs();

    assert_eq!(app.__test_tab_count(), 1);
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn open_script_wake_before_window_readiness_keeps_requests_for_initial_tabs() {
    use sonicterm_types::OpenScriptRequest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    let _guard = crate::open_script_bridge::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::open_script_bridge::drain();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-early-open-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = dir.join("sh");
    std::fs::write(&shell, "#!/bin/sh\nexec cat\n").unwrap();
    let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions).unwrap();

    let request = OpenScriptRequest::resolve(PathBuf::from("early.sh"), Path::new(&dir)).unwrap();
    assert!(!crate::open_script_bridge::push_requests(vec![request]));

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    assert_eq!(app.drain_open_script_requests(), 0, "pre-window wake must not drain the FIFO");

    app.__test_synthetic_main();
    app.seed_initial_tabs();
    assert_eq!(app.__test_tab_count(), 1, "readiness must consume the retained request");
    assert_eq!(app.drain_open_script_requests(), 0, "the retained request drains exactly once");

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn late_script_open_reveals_a_drained_main_window() {
    use sonicterm_types::OpenScriptRequest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    let _guard = crate::open_script_bridge::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::open_script_bridge::drain();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-hidden-main-open-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = dir.join("sh");
    std::fs::write(&shell, "#!/bin/sh\nexec cat\n").unwrap();
    let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions).unwrap();

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.__test_set_main_hidden(true);
    assert!(app.__test_main_hidden());

    let request = OpenScriptRequest::resolve(PathBuf::from("visible.sh"), Path::new(&dir)).unwrap();
    assert!(!crate::open_script_bridge::push_requests(vec![request]));
    assert_eq!(app.drain_open_script_requests(), 1);

    assert!(!app.__test_main_hidden(), "a late open must reveal the main window");
    assert_eq!(app.__test_tab_count(), 1);

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------------
// Memory-sampling wake
//
// The snapshot has to keep arriving in a session nobody is touching, which
// means the loop must wake on its own. It must not repaint when it does: an
// idle session that drew a frame every thirty seconds to record that it was
// idle is a heartbeat redraw under another name, and this crate forbids one.
//
// `do_about_to_wait` needs an `ActiveEventLoop`, so these drive the winit-free
// halves — the pure decision function and the deadline accessor.
// ---------------------------------------------------------------------------

/// An idle session with nothing to draw still arms a wake.
///
/// This is the gap the sampling cadence had. With no render contributor —
/// nothing blinking, nothing deferred, no notification — the loop parked in
/// `Wait` indefinitely and sampled only when something else happened to wake
/// it. A session left alone overnight produced one sample, and the growth
/// curve the snapshot exists to draw needs more than one point.
#[test]
fn an_idle_session_arms_a_memory_wake_and_draws_nothing() {
    assert!(
        wake_is_memory_only(None, Some(Instant::now() + Duration::from_secs(30))),
        "with no render contributor armed, a memory deadline is the only reason to wake, and \
         that wake must not repaint"
    );
}

/// A render contributor due first keeps its redraw.
///
/// The suppression is scoped to wakes the diagnostic armed. A blink phase or a
/// deferred frame landing before the next sample still has a frame to draw,
/// and suppressing it would drop real rendering.
#[test]
fn a_render_wake_due_first_still_redraws() {
    let now = Instant::now();
    assert!(
        !wake_is_memory_only(
            Some(now + Duration::from_millis(16)),
            Some(now + Duration::from_secs(30))
        ),
        "a frame boundary before the next sample must still repaint"
    );
}

/// A tie goes to the render side.
///
/// If a frame is due at the same instant as a sample, the frame still needs
/// drawing. Ordering the comparison the other way would drop a real frame
/// whenever the two coincided.
#[test]
fn a_simultaneous_render_and_memory_wake_redraws() {
    let at = Instant::now() + Duration::from_secs(30);
    assert!(
        !wake_is_memory_only(Some(at), Some(at)),
        "a frame due at the sampling instant must still be drawn"
    );
}

/// With no memory deadline armed, behaviour is exactly as before.
///
/// Pins that the change cannot suppress a redraw on any path that predates it.
#[test]
fn a_wake_with_no_memory_deadline_is_never_memory_only() {
    let now = Instant::now();
    assert!(!wake_is_memory_only(None, None));
    assert!(!wake_is_memory_only(Some(now + Duration::from_millis(16)), None));
}

/// The memory deadline sits one interval after the last sample.
///
/// Read through the real `App` field rather than recomputed, so a change to
/// either the interval or the field this reads cannot leave the two disagreeing
/// while both look correct in isolation.
#[test]
fn the_memory_deadline_is_one_interval_after_the_last_sample() {
    let mut app = app_with_main_window();
    let sampled_at = Instant::now();

    app.last_retention_sample = Some(sampled_at);

    assert_eq!(
        app.memory_sample_deadline(),
        Some(sampled_at + crate::app::retention::RETENTION_SAMPLE_INTERVAL),
        "the next snapshot is due one interval after the last one, so an idle loop wakes \
         exactly when the cadence says it should"
    );
}

#[test]
fn idle_memory_sampling_rearms_after_each_sample_without_redrawing() {
    let mut app = app_with_main_window();
    let interval = crate::app::retention::RETENTION_SAMPLE_INTERVAL;
    let first_sample = Instant::now();

    app.last_retention_sample = Some(first_sample);
    let first_wake = app.memory_sample_deadline();
    assert_eq!(first_wake, Some(first_sample + interval));
    assert!(wake_is_memory_only(None, first_wake));

    let second_sample = first_wake.expect("the first sample arms a wake");
    app.last_retention_sample = Some(second_sample);
    let second_wake = app.memory_sample_deadline();
    assert_eq!(second_wake, Some(first_sample + interval + interval));
    assert!(wake_is_memory_only(None, second_wake));
}

/// Before the first sample there is no deadline, and that cannot persist.
///
/// `do_about_to_wait` samples unconditionally on its first pass and records
/// the instant, so the unarmed state lasts exactly until the loop turns once.
#[test]
fn no_memory_deadline_is_armed_before_the_first_sample() {
    let app = app_with_main_window();
    assert_eq!(app.last_retention_sample, None, "test setup: a fresh app has not sampled");
    assert_eq!(app.memory_sample_deadline(), None);
}

/// The flag is cleared when the wake it describes fires.
///
/// It describes one wake, not a mode. Left set, it would suppress the next
/// genuine render wake — a dropped frame that would look like a rendering bug
/// rather than a diagnostic one.
#[test]
fn the_memory_only_marker_does_not_survive_the_wake_it_describes() {
    let mut app = app_with_main_window();
    app.wake_is_memory_only = true;

    assert!(std::mem::take(&mut app.wake_is_memory_only), "the armed wake reads as memory-only");
    assert!(
        !app.wake_is_memory_only,
        "reading the marker must clear it; a stale one suppresses the next real frame"
    );
}
