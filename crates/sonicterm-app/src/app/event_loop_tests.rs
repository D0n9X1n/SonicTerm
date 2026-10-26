//! Unit tests for the event loop's wake-deadline fold.
//!
//! `do_about_to_wait` itself needs an `ActiveEventLoop`, which only exists
//! inside a running winit loop, so these drive [`App::wake_deadline`] — the
//! winit-free half that computes the instant `do_about_to_wait` then hands to
//! `set_control_flow`.

use super::*;
use crate::app::quit_hold::QUIT_CONFIRM_DURATION;
#[cfg(windows)]
use crate::app::FOREGROUND_PROCESS_TTL;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

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

/// Native drop callbacks stay window-local and form one atomic path list at the event-loop's wait boundary.
#[cfg(windows)]
#[test]
fn real_pty_winit_drop_turns_batch_each_window() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    if isolated() {
        return;
    }
    struct Probe {
        ran: bool,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let (mut app, windows) = input_test_windows();
            app.config.window.warm_window_pool = 0;
            let submitted = PtySubmissions::start();
            app.wait_for_input_queues();
            for (window, _) in windows {
                app.do_window_event(event_loop, window, WindowEvent::DroppedFile("a".into()));
                app.do_window_event(event_loop, window, WindowEvent::DroppedFile("b".into()));
            }
            assert!(submitted.take().is_empty(), "DroppedFile must wait until about_to_wait");
            app.do_about_to_wait(event_loop);
            let mut writes = submitted.take();
            writes.sort_by_key(|(pane, _)| *pane);
            assert_eq!(writes, windows.map(|(_, pane)| (pane, b"\"a\" \"b\"".to_vec())));
            app.wait_for_input_queues();
            for (window, _) in windows {
                app.do_window_event(event_loop, window, WindowEvent::DroppedFile("valid".into()));
                app.do_window_event(
                    event_loop,
                    window,
                    WindowEvent::DroppedFile("invalid\npath".into()),
                );
            }
            assert!(submitted.take().is_empty());
            app.do_about_to_wait(event_loop);
            assert!(
                submitted.take().is_empty(),
                "one invalid member must refuse the entire window's drop"
            );
            for (window, _) in windows {
                assert_eq!(
                    app.windows[&window].notification.as_ref().unwrap().message,
                    "Paste refused for 1 of 1 destinations: ControlCharacter (destinations: 1)"
                );
            }
            app.do_about_to_wait(event_loop);
            assert!(submitted.take().is_empty(), "a consumed turn cannot replay");
            self.ran = true;
            event_loop.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: WindowEvent,
        ) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut probe = Probe { ran: false };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}

/// Software-render + IME-composing frame period, the widest gap between a
/// frame boundary and any other contributor.
const COMPOSE_PERIOD: Duration = crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD;

#[test]
fn resumed_retires_previous_main_before_installing_its_replacement() {
    // Real window creation must use the same retirement path as the owned-PTY replacement regression.
    let source = include_str!("event_loop.rs");
    let (_, resumed) = source.split_once("pub(super) fn do_resumed(").unwrap();
    let retire = resumed.find("self.retire_previous_main()").unwrap();
    let install = resumed.find("self.main_window_id = Some(main_id)").unwrap();
    assert!(retire < install);
}

#[test]
fn macos_tabbing_policy_precedes_hooks_and_native_window_creation() {
    // Source wiring protects startup order; macOS native smoke separately exercises the AppKit property.
    let source = include_str!("event_loop.rs");
    let (_, resumed) = source.split_once("pub(super) fn do_resumed(").unwrap();
    let setter = resumed.find("event_loop.set_allows_automatic_window_tabbing(false)").unwrap();
    let hook = resumed.find("self.on_resumed.take()").unwrap();
    let create = resumed.find("let window = match event_loop.create_window(attrs)").unwrap();
    assert!(setter < hook && hook < create);
    assert!(source.contains("use winit::platform::macos::ActiveEventLoopExtMacOS;"));
    let policy_prefix = &resumed[..setter];
    assert!(policy_prefix.contains("#[cfg(target_os = \"macos\")]"));
}

#[test]
fn macos_tabbing_smoke_checks_the_process_property_before_window_creation() {
    // Restrict failure assertions to the policy block, not the later window-creation error handler.
    let source = include_str!("event_loop.rs");
    let (_, resumed) = source.split_once("pub(super) fn do_resumed(").unwrap();
    let setter = resumed.find("event_loop.set_allows_automatic_window_tabbing(false)").unwrap();
    let hook = resumed.find("self.on_resumed.take()").unwrap();
    let policy = &resumed[setter..hook];
    assert!(policy.contains("if event_loop.allows_automatic_window_tabbing()"));
    assert!(policy.contains("self.runtime_smoke.as_mut()"));
    assert!(policy.contains("smoke.fail(RuntimeSmokeFailure::Display)"));
    assert!(policy.contains("event_loop.exit()"));
    assert!(policy.contains("return;"));
}

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
    let main = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    );
    let child = include_str!("child_window.rs");
    let shared = include_str!("window_setup.rs");

    for source in [main, child] {
        assert!(source.contains("inner_size_writer"));
        assert!(source.contains("apply_window_dpi_transition("));
    }
    assert!(shared.contains("native.is_maximized() || native.fullscreen().is_some()"));
    let renderer_resize =
        shared.find("renderer.try_resize_outcome(target.width, target.height)").unwrap();
    let native_resize = shared.find("inner_size_writer.request_inner_size(target)").unwrap();
    assert!(renderer_resize < native_resize, "renderer rejection must precede native commit");
}

/// Each language file names both requirements for explicit rmux application passthrough.
#[test]
fn rmux_osc52_documentation_enables_clipboard_and_passthrough() {
    for (language, internals) in [
        ("English", include_str!("../../../../wiki/Architecture-Internals.md")),
        ("Chinese", include_str!("../../../../wiki/Architecture-Internals-zh-CN.md")),
    ] {
        assert!(internals.contains("set -s set-clipboard on"), "{language} clipboard policy");
        assert!(internals.contains("set -g allow-passthrough on"), "{language} passthrough policy");
    }
}

/// Every terminal-window constructor installs the shared live-renderer floor.
#[test]
fn all_terminal_window_creation_paths_apply_the_minimum() {
    let main = include_str!("event_loop.rs");
    let tear_out = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );

    assert!(main.contains("apply_terminal_window_minimum(&window, &mut renderer)"));
    assert!(tear_out.contains("apply_terminal_window_minimum(window, renderer)"));
    assert!(tear_out.contains("fn create_warm_window"));
    assert!(tear_out.contains("fn install_torn_out_window"));
}

/// DPI, font, padding, and tab-bar changes all refresh native minimums.
#[test]
fn live_metric_change_paths_refresh_the_minimum() {
    let main_events = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    );
    let child_events = include_str!("child_window.rs");
    // Font size and family changes live in live_font.rs; padding and tab-bar changes in config_apply.rs.
    let config = [include_str!("config_apply.rs"), include_str!("live_font.rs")].concat();

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
    app.main_mut().unwrap().redraw.deferred = true;
    let window = app.main_mut().expect("synthetic main window");
    window.last_render = last_render;
    window.stream_clock = last_render;
    window.ime.handle_preedit("あ", Some((0, 1)));
    assert!(window.ime.is_composing(), "preedit must put the window on the composing cadence");
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
fn contention_retry_is_nonstarving_per_window_across_frame_policies() {
    // Repeated input cannot bypass or postpone a due collection; coherent collection releases only the contention floor.
    for software in [false, true] {
        for composing in [false, true] {
            for is_main in [false, true] {
                let mut app = app_with_main_window();
                let id = if is_main {
                    app.main_window_id.unwrap()
                } else {
                    app.__test_seed_child_window(&["child"])
                };
                let now = Instant::now();
                let previous = now - Duration::from_secs(1);
                app.software_render_degrade = software;
                let window = app.windows.get_mut(&id).unwrap();
                window.last_render = previous;
                window.stream_clock = previous;
                if composing {
                    window.ime.handle_preedit("中", None);
                }
                let period =
                    crate::app::effective_frame_period(software, composing, app.frame_period);
                app.defer_window_lock_contention(id, true, now);
                let due = now + period;
                assert_eq!(app.windows[&id].retry_not_before, Some(due));
                assert_eq!(app.wake_deadline(None), Some(due));
                for offset in [1, 2, 3] {
                    let event_at = now + period * offset / 4;
                    assert!(app.windows[&id].contention_blocks_redraw(event_at, period, software));
                    app.defer_window_lock_contention(id, false, event_at);
                    assert_eq!(app.windows[&id].retry_not_before, Some(due));
                }
                assert!(!app.windows[&id].contention_blocks_redraw(due, period, software));
                app.defer_window_lock_contention(id, false, due);
                assert_eq!(app.windows[&id].retry_not_before, Some(due + period));
                assert_eq!(app.windows[&id].last_render, previous);
                assert_eq!(
                    app.windows[&id].stream_clock, previous,
                    "contention moves neither clock"
                );
                app.windows.get_mut(&id).unwrap().coherent_frame_collected();
                assert_eq!(app.windows[&id].retry_not_before, None);
                assert!(!app.windows[&id].contention_blocks_redraw(due, period, software));
                app.windows.get_mut(&id).unwrap().redraw.deferred = false;
                if is_main {
                    app.pending_redraw = false;
                } else {
                    app.pending_redraw_windows.remove(&id);
                }
                assert_eq!(app.wake_deadline(None), None);
                app.defer_window_lock_contention(id, false, due);
                app.windows.remove(&id);
                assert_eq!(app.wake_deadline(None), None);
            }
        }
    }
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

#[cfg(windows)]
#[test]
fn foreground_probe_output_activity_debounces_until_quiet() {
    // Protect streaming output from triggering one process-table walk per 500 ms frame.
    let start = Instant::now();
    let mut app = app_with_main_window();

    app.arm_foreground_probe_after_output(start);
    app.arm_foreground_probe_after_output(start + Duration::from_millis(400));

    let wake = app.foreground_schedule.activity_wake.expect("output arms a probe wake");
    assert_eq!(wake.due, start + Duration::from_millis(400) + FOREGROUND_PROCESS_TTL);
    assert!(!wake.fixed);
    assert_eq!(app.wake_deadline(None), Some(wake.due));
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
fn foreground_probe_rearms_only_for_per_tab_warning_in_regular_process() {
    // Protect clear-on-return polling without reviving idle or globally elevated heartbeat redraws.
    let now = Instant::now();
    let mut app = app_with_main_window();

    app.finish_foreground_process_probe(now, true);
    // The warning wake is separate from the activity wake, so output never postpones it.
    let wake = app.foreground_schedule.warning_wake.expect("foreground warning rearms");
    assert_eq!(wake, now + FOREGROUND_PROCESS_TTL);
    assert!(app.foreground_schedule.activity_wake.is_none());

    app.finish_foreground_process_probe(now, false);
    assert!(app.foreground_schedule.warning_wake.is_none());

    app.set_process_privilege(crate::ProcessPrivilege::Privileged);
    app.finish_foreground_process_probe(now, true);
    assert!(app.foreground_schedule.warning_wake.is_none());
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

fn arm_snap_scrollbar(app: &mut App, window_id: WindowId, pane_id: u64, active: Instant) {
    use crate::app::scrollbar_visibility::{note_activity, retarget, ScrollbarVisState};
    let mut state = ScrollbarVisState::new(active);
    note_activity(&mut state, active);
    retarget(&mut state, sonicterm_cfg::config::ScrollbarMode::Auto, false, active);
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

    let affected = app.expire_due_scrollbar_idle(now);

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
        app.main_mut().unwrap().redraw.deferred = true;
        {
            let window = app.main_mut().expect("synthetic main window");
            window.last_render = last;
            window.stream_clock = last;
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
    app.main_mut().unwrap().redraw.monitor_period = HZ_120;

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
        gaps.iter().all(|gap| *gap > HZ_120),
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
    app.main_mut().unwrap().redraw.monitor_period = HZ_120;

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
    app.main_mut().unwrap().redraw.monitor_period = HZ_120;

    let before = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        before.iter().all(|gap| *gap == crate::app::SOFTWARE_RENDER_FRAME_PERIOD),
        "precondition: the software cap must be in effect before composing, got {before:?}"
    );

    {
        let window = app.main_mut().expect("synthetic main window");
        window.ime.handle_preedit("あ", Some((0, 1)));
        assert!(
            window.ime.is_composing(),
            "the preedit must put the window on the compose cadence"
        );
    }
    let during = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        during.iter().all(|gap| *gap == crate::app::SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD),
        "composing must drop to the compose cadence, got {during:?}"
    );

    {
        let window = app.main_mut().expect("synthetic main window");
        window.ime.handle_commit("あ");
        assert!(!window.ime.is_composing(), "the commit must end composition");
    }
    let after = measure_frame_gaps(&mut app, 4, Instant::now());
    assert!(
        after.iter().all(|gap| *gap == crate::app::SOFTWARE_RENDER_FRAME_PERIOD),
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
    app.main_mut().unwrap().redraw.monitor_period = HZ_120;

    let rendered_at = Instant::now();
    // A keystroke arriving one microsecond after a frame: the worst case, with
    // almost a whole period left to wait.
    let keystroke_at = rendered_at + Duration::from_micros(1);
    app.pending_redraw = true;
    app.main_mut().unwrap().redraw.deferred = true;
    {
        let window = app.main_mut().expect("synthetic main window");
        window.last_render = rendered_at;
        window.stream_clock = rendered_at;
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

#[path = "test_hooks_event_loop_launch_fixture.rs"]
mod test_hooks_event_loop_launch_fixture;
use test_hooks_event_loop_launch_fixture::*;
