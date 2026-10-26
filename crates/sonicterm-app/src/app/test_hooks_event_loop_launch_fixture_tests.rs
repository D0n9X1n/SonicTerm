//! Launch-request fixtures and explicit-source user-event contracts. regression contracts.

use super::*;

#[cfg(unix)]
#[test]
fn startup_without_script_requests_keeps_the_normal_blank_tab() {
    use std::os::unix::fs::PermissionsExt;

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
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

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
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
    assert!(!crate::launch_queue::push_scripts(vec![request]));

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    assert_eq!(app.drain_launch_requests(), 0, "pre-window wake must not drain the FIFO");

    app.__test_synthetic_main();
    app.seed_initial_tabs();
    assert_eq!(app.__test_tab_count(), 1, "readiness must consume the retained request");
    assert_eq!(app.drain_launch_requests(), 0, "the retained request drains exactly once");

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn late_script_open_reveals_a_drained_main_window() {
    use sonicterm_types::OpenScriptRequest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
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
    assert!(!crate::launch_queue::push_scripts(vec![request]));
    assert_eq!(app.drain_launch_requests(), 1);

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

/// A serviced typed maintenance entry is removed instead of leaving a boolean
/// capable of suppressing the next unrelated frame wake.
#[test]
fn the_memory_only_marker_does_not_survive_the_wake_it_describes() {
    let mut app = app_with_main_window();
    let now = Instant::now();
    app.redraw_due.push(super::super::super::super::redraw::DueWork {
        owner: None,
        cause: super::super::super::super::redraw::DueCause::Memory,
        deadline: now,
    });
    app.service_redraw_due(now);
    assert!(
        app.redraw_due.is_empty(),
        "serviced maintenance identities cannot suppress the next frame"
    );
}

/// Each stopped renderer gets a native boundary for its one-time report; usable replacements recover only once.
#[test]
fn gpu_device_state_change_checks_each_owner_without_repeating_a_usable_recovery_request() {
    let source = include_str!("event_loop.rs");
    let legacy = source.split_once("UserEvent::GpuDeviceStateChanged => {").unwrap().1;
    let legacy = legacy.split_once("UserEvent::GpuDeviceGenerationChanged").unwrap().0;
    assert!(legacy.contains("self.request_device_state_redraws();"));
    let recovery = include_str!("gpu_recovery.rs");
    let tagged = recovery.split_once("pub(super) fn gpu_generation_changed(").unwrap().1;
    let tagged = tagged.split_once("pub(super) fn gpu_recovery_ready(").unwrap().0;
    let generation_check = tagged.find("recovery.coordinator.committed() == generation").unwrap();
    let redraw = tagged.find("self.request_device_state_redraws();").unwrap();
    let service = tagged.find("self.service_gpu_recovery(event_loop, Instant::now());").unwrap();
    assert!(generation_check < redraw && redraw < service);
    let redraw = concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    );
    let event = redraw.split_once("pub(super) fn request_device_state_redraws(").unwrap().1;
    let event = event.split_once("pub(super) fn request_recovered_window(").unwrap().0;
    assert!(event.contains("self.windows.keys().copied().collect()"));
    let recovery = event.find("!self.request_recovered_window(id)").unwrap();
    let stopped = event.find("!renderer.device_accepts_gpu_work()").unwrap();
    let request = event.find("window.request_window_redraw()").unwrap();
    assert!(recovery < stopped && stopped < request);
    assert_eq!(event.matches("window.request_window_redraw()").count(), 1);
    assert!(!event.contains("note_render_attempt"));
}

/// A fallback wake for a closed window, or for a window whose renderer is absent, requests no
/// frame and marks no cause: only a renderer can say its notice still needs applying.
#[test]
fn a_fallback_wake_without_a_live_renderer_does_nothing() {
    let (mut app, child) = app_with_child_window();
    let before = crate::app::window_state::window_redraw_requests();
    app.handle_font_fallback_ready(child, 1);
    let closed = WindowId::dummy();
    app.handle_font_fallback_ready(closed, 1);
    let window = &app.windows[&child];
    assert!(!window.redraw.has_pending());
    assert!(!window.redraw.request_in_flight);
    assert_eq!(crate::app::window_state::window_redraw_requests(), before);
}

/// A due fallback generation in a visible window asks for exactly one frame; a second wake
/// before that frame runs adds no native request, because one is already in flight.
#[test]
fn a_due_fallback_wake_requests_one_frame_for_a_visible_window() {
    let (mut app, child) = app_with_child_window();
    let before = crate::app::window_state::window_redraw_requests();
    app.request_font_fallback_frame(child, true);
    assert!(app.windows[&child].redraw.has_pending());
    assert!(app.windows[&child].redraw.request_in_flight);
    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1);
    app.request_font_fallback_frame(child, true);
    assert_eq!(crate::app::window_state::window_redraw_requests(), before + 1);
}

/// A hidden window only records the cause; its next prepared frame applies the generation.
#[test]
fn a_due_fallback_wake_only_marks_a_hidden_window() {
    let (mut app, child) = app_with_child_window();
    app.windows.get_mut(&child).unwrap().hidden = true;
    let before = crate::app::window_state::window_redraw_requests();
    app.request_font_fallback_frame(child, true);
    assert!(app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert_eq!(crate::app::window_state::window_redraw_requests(), before);
}

/// A wake whose generation is already applied, or whose notice was replaced, needs no frame.
#[test]
fn a_fallback_wake_that_is_not_due_does_nothing() {
    let (mut app, child) = app_with_child_window();
    let before = crate::app::window_state::window_redraw_requests();
    app.request_font_fallback_frame(child, false);
    assert!(!app.windows[&child].redraw.has_pending());
    assert!(!app.windows[&child].redraw.request_in_flight);
    assert_eq!(crate::app::window_state::window_redraw_requests(), before);
}

/// A legacy `ClearShapeCache` clears every live renderer's shape caches (its style revision moves)
/// and asks each of those windows for one native frame. Windows-only: there an event loop and real
/// GDI renderers may run off the main thread, and `isolated()` keeps it the only loop in its process.
#[cfg(windows)]
#[test]
fn clear_shape_cache_clears_and_redraws_every_window_with_a_renderer() {
    use sonicterm_cfg::config::{ScrollbarMode, SoftwareRenderMode};
    use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::EventLoopExtPumpEvents;
    use winit::platform::windows::EventLoopBuilderExtWindows;

    /// Runs the case once inside `resumed`, where a renderer can be built.
    #[derive(Default)]
    struct Case {
        outcome: Option<Result<(), String>>,
    }

    impl ApplicationHandler<UserEvent> for Case {
        fn resumed(&mut self, active: &ActiveEventLoop) {
            if self.outcome.is_none() {
                self.outcome = Some(run(active));
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    pub(super) fn renderer(
        active: &ActiveEventLoop,
    ) -> Result<(std::sync::Arc<winit::window::Window>, GpuRenderer), String> {
        let window = std::sync::Arc::new(
            active
                .create_window(winit::window::Window::default_attributes().with_visible(false))
                .map_err(|error| error.to_string())?,
        );
        let settings = RendererSettings {
            font_family: "monospace",
            font_dirs: &[],
            font_size: 14.0,
            line_height_mult: 1.2,
            font_weight_scale: 1.0,
            subpixel_aa: Default::default(),
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: Default::default(),
                opacity: 1.0,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "clear-shape-cache-test",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        };
        let renderer = GpuRenderer::new(window.clone(), active, &Theme::default(), settings)
            .map_err(|error| error.to_string())?;
        Ok((window, renderer))
    }

    pub(super) fn run(active: &ActiveEventLoop) -> Result<(), String> {
        let (mut app, first) = app_with_child_window();
        let second = app.__test_seed_child_window(&["second"]);
        for id in [first, second] {
            let (window, renderer) = renderer(active)?;
            if !app.__test_attach_window_renderer(id, window, renderer) {
                return Err(format!("{id:?} has no window state"));
            }
        }
        let revision = |app: &App, id: WindowId| {
            app.windows[&id].renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::style_rev)
        };
        let before = [revision(&app, first), revision(&app, second)];
        let requests_before = crate::app::window_state::window_redraw_requests();
        app.handle_clear_shape_cache();
        let requests = crate::app::window_state::window_redraw_requests() - requests_before;
        let after = [revision(&app, first), revision(&app, second)];
        if requests != 2 {
            return Err(format!("expected one native frame request per window, saw {requests}"));
        }
        for (before, after) in before.into_iter().zip(after) {
            if !matches!((before, after), (Some(old), Some(new)) if new > old) {
                return Err(format!(
                    "a renderer's caches were not cleared: {before:?} -> {after:?}"
                ));
            }
        }
        Ok(())
    }

    if crate::app::pty_test_support::isolated() {
        return;
    }
    let mut event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .expect("Windows event loop");
    let mut case = Case::default();
    // Bounded well inside the isolation envelope's 60 s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while case.outcome.is_none() && std::time::Instant::now() < deadline {
        let _ = event_loop.pump_app_events(Some(std::time::Duration::from_millis(50)), &mut case);
    }
    match case.outcome {
        Some(Ok(())) => {}
        Some(Err(error)) => panic!("clear shape cache: {error}"),
        None => panic!("the event loop never resumed"),
    }
}

/// A queued terminal launch reaches `spawn_pane` as exactly that command, cwd, title and hold,
/// names the tab and the native window, and drains once.
#[cfg(unix)]
#[test]
fn a_queued_terminal_launch_reaches_spawn_pane_exactly() {
    use sonicterm_types::{CommandLaunch, CwdSource, LaunchRequest, TerminalLaunch};

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
    let root = tempfile::tempdir().unwrap();
    let launch = TerminalLaunch {
        command: Some(CommandLaunch {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
        }),
        cwd: Some(root.path().to_path_buf()),
        cwd_source: Some(CwdSource::Dir),
        title: Some("Logs".to_string()),
        hold: true,
        ..TerminalLaunch::default()
    };
    assert!(!crate::launch_queue::push_requests(vec![LaunchRequest::Terminal(launch.clone())]));

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    app.seed_initial_tabs();

    assert_eq!(app.__test_tab_count(), 1, "the launch replaces the default shell tab");
    let recorded = app.test_pane_launches.borrow().clone();
    assert_eq!(recorded.len(), 1);
    let pane_launch = &recorded[0].1;
    assert_eq!(pane_launch.command, launch.command);
    assert_eq!(pane_launch.cwd.as_deref(), Some(root.path()));
    assert_eq!(pane_launch.cwd_source, Some(CwdSource::Dir));
    assert!(pane_launch.hold);
    assert_eq!(pane_launch.title.as_deref(), Some("Logs"));
    let main = app.main().unwrap();
    assert_eq!(main.tabs.tabs()[0].title, "Logs");
    assert_eq!(main.custom_window_name, "Logs");
    assert!(main.panes[&main.tab_states[0].active_pane].hold_on_exit);
    assert_eq!(app.drain_launch_requests(), 0, "the launch drains exactly once");
}

/// A command that cannot start opens its tab with the reason in the grid and starts no shell.
#[cfg(unix)]
#[test]
fn a_failed_command_shows_its_reason_and_starts_no_shell() {
    use sonicterm_types::{CommandLaunch, TerminalLaunch};

    let root = tempfile::tempdir().unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let launch = crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
        command: Some(CommandLaunch {
            program: "sonicterm-missing-program".into(),
            args: vec!["secret-value".into()],
        }),
        cwd: Some(root.path().to_path_buf()),
        ..TerminalLaunch::default()
    });
    app.new_tab_with_launch("failed", launch);

    let main = app.main().unwrap();
    let pane = &main.panes[&main.tab_states[0].active_pane];
    assert!(pane.pty.is_none(), "a failed command must not fall back to a shell");
    let text = main_tab_text(&app, 0);
    assert!(text.contains("cannot run sonicterm-missing-program: no such program"), "{text}");
    assert!(!text.contains("secret-value"));
}

/// No log level records an argument value, a cwd or a title, for a launch that starts or fails.
#[cfg(unix)]
#[test]
fn launch_logs_carry_no_argument_values() {
    use sonicterm_types::{CommandLaunch, CwdSource, TerminalLaunch};

    let root = tempfile::tempdir().unwrap();
    let secret_dir = root.path().join("secret-dir-name");
    std::fs::create_dir(&secret_dir).unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let launch_with = |program: &str| {
        crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
            command: Some(CommandLaunch {
                program: program.into(),
                args: vec!["-c".into(), "exit 0 # secret-token-AAA".into(), "hunter2-BBB".into()],
            }),
            cwd: Some(secret_dir.clone()),
            cwd_source: Some(CwdSource::Dir),
            title: Some("secret-title-CCC".to_string()),
            hold: false,
            ..TerminalLaunch::default()
        })
    };
    let log = capture_all_levels(|| {
        app.new_tab_with_launch("one", launch_with("/bin/sh"));
        app.new_tab_with_launch("two", launch_with("sonicterm-missing-program"));
    });

    assert!(log.contains("pane launched"), "the successful launch logs: {log}");
    assert!(log.contains("arg_count=3"), "{log}");
    assert!(log.contains("pane command did not start"), "the failure logs: {log}");
    for secret in ["secret-token-AAA", "hunter2-BBB", "secret-dir-name", "secret-title-CCC"] {
        assert!(!log.contains(secret), "{secret} leaked into the log: {log}");
    }
}

/// A cold start whose only request is a notice opens the plain shell tab, then shows the notice.
#[test]
fn a_notice_only_cold_start_opens_the_shell_and_shows_the_notice() {
    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
    assert!(!crate::launch_queue::push_notice("Link not opened: secret\u{1b}[2J".to_string()));

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    app.seed_initial_tabs();

    assert_eq!(
        app.__test_tab_count(),
        1,
        "the notice opens no tab of its own; the shell tab opens"
    );
    assert_eq!(
        app.__test_main_notification_message(),
        Some("Link not opened: secret\u{FFFD}[2J"),
        "the notice is shown with control characters replaced"
    );
    assert!(app.test_pane_launches.borrow()[0].1.command.is_none());
}

/// A notice beside a tab launch adds no shell tab, and a running app's drain opens none for it.
#[cfg(unix)]
#[test]
fn a_notice_beside_a_launch_adds_no_extra_tab() {
    use sonicterm_types::{LaunchRequest, TerminalLaunch};

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    assert!(!crate::launch_queue::push_requests(vec![
        LaunchRequest::Terminal(TerminalLaunch::default()),
        LaunchRequest::Notice("Not run: tool is quarantined (downloaded).".to_string()),
    ]));
    app.seed_initial_tabs();
    assert_eq!(app.__test_tab_count(), 1);

    assert!(!crate::launch_queue::push_notice("second".to_string()));
    assert_eq!(app.drain_launch_requests(), 0, "a running app's notice opens no tab");
    assert_eq!(app.__test_tab_count(), 1);
    assert_eq!(app.__test_main_notification_message(), Some("second"));
}

/// A document-opened executable replaced after classification does not start, and says why.
#[cfg(unix)]
#[test]
fn a_replaced_executable_fails_closed_before_spawn() {
    use sonicterm_types::{CommandLaunch, FileIdentity, LaunchOrigin, TerminalLaunch};
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("tool");
    std::fs::write(&program, b"classified").unwrap();
    let metadata = std::fs::metadata(&program).unwrap();
    let identity = FileIdentity { device: metadata.dev(), inode: metadata.ino() };
    // Swap a different inode in under the same name, as an attacker racing the spawn would.
    let replacement = root.path().join("replacement");
    std::fs::write(&replacement, b"swapped").unwrap();
    std::fs::rename(&replacement, &program).unwrap();

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let launch = crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
        command: Some(CommandLaunch { program: program.clone().into(), args: Vec::new() }),
        cwd: Some(root.path().to_path_buf()),
        hold: true,
        origin: LaunchOrigin::Executable { identity },
        ..TerminalLaunch::default()
    });
    app.new_tab_with_launch("tool", launch);

    let main = app.main().unwrap();
    assert!(main.panes[&main.tab_states[0].active_pane].pty.is_none(), "nothing may start");
    let text = main_tab_text(&app, 0);
    assert!(text.contains("Not run: tool changed before it could start."), "{text}");
}

/// Executable and link launches log their kind and scheme but never a path, host or argument.
#[cfg(unix)]
#[test]
fn executable_and_link_launch_logs_carry_kind_and_scheme_only() {
    use sonicterm_types::{CommandLaunch, CwdSource, FileIdentity, LaunchOrigin, TerminalLaunch};
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir().unwrap();
    let secret_dir = root.path().join("secret-dir-name");
    std::fs::create_dir(&secret_dir).unwrap();
    let shell = std::fs::canonicalize("/bin/sh").unwrap();
    let metadata = std::fs::metadata(&shell).unwrap();
    let identity = FileIdentity { device: metadata.dev(), inode: metadata.ino() };
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let executable = crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
        command: Some(CommandLaunch { program: shell.clone().into(), args: Vec::new() }),
        cwd: Some(secret_dir.clone()),
        cwd_source: Some(CwdSource::Parent),
        hold: true,
        origin: LaunchOrigin::Executable { identity },
        ..TerminalLaunch::default()
    });
    let link = crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
        command: Some(CommandLaunch {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0 # secret-host-DDD".into()],
        }),
        cwd: Some(secret_dir.clone()),
        cwd_source: Some(CwdSource::Home),
        origin: LaunchOrigin::Url { scheme: "ssh" },
        ..TerminalLaunch::default()
    });
    let log = capture_all_levels(|| {
        app.new_tab_with_launch("one", executable);
        app.new_tab_with_launch("two", link);
    });

    assert!(log.contains("kind=\"executable\"") || log.contains("kind=executable"), "{log}");
    assert!(log.contains("cwd_source=\"parent\"") || log.contains("cwd_source=parent"), "{log}");
    assert!(log.contains("kind=\"url\"") || log.contains("kind=url"), "{log}");
    assert!(log.contains("scheme=\"ssh\"") || log.contains("scheme=ssh"), "{log}");
    for secret in ["secret-host-DDD", "secret-dir-name", shell.to_str().unwrap()] {
        assert!(!log.contains(secret), "{secret} leaked into the log: {log}");
    }
}

#[cfg(unix)]
#[test]
fn cold_script_requests_replace_the_blank_tab_and_late_requests_append() {
    use sonicterm_types::OpenScriptRequest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    let _guard =
        crate::launch_queue::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = crate::launch_queue::drain();

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
    assert!(!crate::launch_queue::push_scripts(vec![first, second]));

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.seed_initial_tabs();
    assert_eq!(app.__test_tab_count(), 2, "cold requests must not leave a blank tab");

    let late = request("late.sh");
    assert!(!crate::launch_queue::push_scripts(vec![late]));
    assert_eq!(app.drain_launch_requests(), 1);
    assert_eq!(app.__test_tab_count(), 3, "late opens append one tab");
    assert_eq!(app.drain_launch_requests(), 0, "requests drain exactly once");

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
