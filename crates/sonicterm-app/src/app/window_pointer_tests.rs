#[cfg(windows)]
use crate::app::{hovered_url::HoveredUrl, App};
#[cfg(windows)]
use sonicterm_cfg::{
    config::{Config, ScrollbarMode},
    keymap::Keymap,
    theme::Theme,
};
#[cfg(windows)]
use sonicterm_ui::selection::Selection;
#[cfg(windows)]
use winit::keyboard::ModifiersState;

/// A snapshot never borrows the parser or app, so a blocked handler cannot hide its last probe boundary.
struct NativeProbeStallState {
    probe: String,
    case: String,
    phase: String,
    parser_lock_held: bool,
    // Callback counts preserve delivery order even when adjacent clock readings are equal.
    heartbeat_count: u64,
    last_about_to_wait: Option<std::time::Instant>,
}

/// Heartbeats and disarm share a channel so neither watchdog wait needs polling or a sleep loop.
enum NativeWatchdogEvent {
    AboutToWait,
    Disarm,
}

/// Shares only short-lived diagnostic state locks with the watchdog, independently of the fixture's parser lock.
struct NativeProbeProgress {
    state: std::sync::Mutex<NativeProbeStallState>,
    events: std::sync::mpsc::Sender<NativeWatchdogEvent>,
}

impl NativeProbeProgress {
    /// Give the fixture shared progress and the watchdog sole ownership of its notification receiver.
    fn new() -> (std::sync::Arc<Self>, std::sync::mpsc::Receiver<NativeWatchdogEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        (
            std::sync::Arc::new(Self {
                state: std::sync::Mutex::new(NativeProbeStallState {
                    probe: "fixture".into(),
                    case: "all".into(),
                    phase: "before_run_app".into(),
                    parser_lock_held: false,
                    heartbeat_count: 0,
                    last_about_to_wait: None,
                }),
                events,
            }),
            receiver,
        )
    }

    /// Publish the next operation before entering it, without keeping this lock during native or parser work.
    fn set(&self, probe: &str, case: &str, phase: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.probe = probe.into();
        state.case = case.into();
        state.phase = phase.into();
    }

    /// Bracket the deliberately held parser lock, including a stall while trying to acquire it.
    fn parser_lock(&self, held: bool) {
        self.state.lock().unwrap().parser_lock_held = held;
    }

    /// Mark callback entry before the loop can block in any of the fixture's probes.
    fn about_to_wait(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.heartbeat_count += 1;
            state.last_about_to_wait = Some(std::time::Instant::now());
        }
        // A finished watchdog no longer needs heartbeats; closed notification channels are harmless.
        let _ = self.events.send(NativeWatchdogEvent::AboutToWait);
    }
}

/// The guard owns the worker only until run_app returns, including its unwind path.
struct NativeWatchdog {
    disarm: std::sync::mpsc::Sender<NativeWatchdogEvent>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NativeWatchdog {
    /// Start the injected expiry actions on a separate thread before any native callback runs.
    fn start(
        progress: std::sync::Arc<NativeProbeProgress>,
        events: std::sync::mpsc::Receiver<NativeWatchdogEvent>,
        deadline: std::time::Instant,
        wake: impl FnOnce() -> bool + Send + 'static,
        mut report: impl std::io::Write + Send + 'static,
        exit: impl FnOnce(i32) + Send + 'static,
    ) -> Self {
        let disarm = progress.events.clone();
        let worker = std::thread::spawn(move || {
            run_native_watchdog(&progress, &events, deadline, wake, &mut report, exit);
        });
        Self { disarm, worker: Some(worker) }
    }
}

// Lifecycle: NativeWatchdog disarms its channel wait and joins the worker before the fixture leaves scope.
impl Drop for NativeWatchdog {
    fn drop(&mut self) {
        // An already expired worker may have closed its receiver; disarm still must not mask a fixture failure.
        let _ = self.disarm.send(NativeWatchdogEvent::Disarm);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("native watchdog thread");
        }
    }
}

/// Expiry reports through Write rather than libtest capture, tries one wake, and bounds the follow-up to two seconds.
fn run_native_watchdog(
    progress: &NativeProbeProgress,
    events: &std::sync::mpsc::Receiver<NativeWatchdogEvent>,
    deadline: std::time::Instant,
    wake: impl FnOnce() -> bool,
    report: &mut impl std::io::Write,
    exit: impl FnOnce(i32),
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait) => {}
            Ok(NativeWatchdogEvent::Disarm) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    let initial = {
        let state = progress.state.lock().unwrap();
        format!(
            "native watchdog expired: probe={} case={} phase={} parser_lock_held={} last_about_to_wait={:?}\n",
            state.probe, state.case, state.phase, state.parser_lock_held, state.last_about_to_wait,
        )
    };
    // Reporting failure cannot prevent the wake or nonzero exit; stderr may already be closed.
    let _ = report.write_all(initial.as_bytes());
    let wake_started = Instant::now();
    // Snapshot callback order, not clock time: an immediate heartbeat need not advance Instant.
    let heartbeats_before_wake = progress.state.lock().unwrap().heartbeat_count;
    let wake_sent = wake();
    let wake_deadline = wake_started + Duration::from_secs(2);
    let after_wake = loop {
        let observed = progress.state.lock().unwrap().heartbeat_count;
        if observed > heartbeats_before_wake {
            break true;
        }
        let remaining = wake_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait | NativeWatchdogEvent::Disarm) => {}
            Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => {
                break progress.state.lock().unwrap().heartbeat_count > heartbeats_before_wake;
            }
        }
    };
    // Direct write_all also keeps this post-wake verdict visible when libtest captures the stalled test's output.
    let _ = report.write_all(
        format!("native watchdog wake_sent={wake_sent} about_to_wait_after_wake={after_wake}\n")
            .as_bytes(),
    );
    exit(1);
}

/// Expiry names the stalled native step and distinguishes a delivered wake from an unresponsive loop.
#[test]
fn native_watchdog_expiry_reports_phase_lock_and_wake_progress() {
    use std::time::{Duration, Instant};

    for reaches_about_to_wait in [false, true] {
        let (progress, events) = NativeProbeProgress::new();
        progress.set("hover", "child/OSC8", "PointerContendedActive");
        progress.parser_lock(true);
        let mut report = Vec::new();
        let mut wakes = 0;
        let mut exit_code = None;
        run_native_watchdog(
            &progress,
            &events,
            Instant::now() + Duration::from_millis(2),
            || {
                wakes += 1;
                if reaches_about_to_wait {
                    progress.about_to_wait();
                }
                true
            },
            &mut report,
            |code| exit_code = Some(code),
        );
        let report = String::from_utf8(report).unwrap();
        assert!(
            report.contains("probe=hover case=child/OSC8 phase=PointerContendedActive"),
            "{report}"
        );
        assert!(report.contains("parser_lock_held=true"), "{report}");
        assert!(
            report.contains(&format!("about_to_wait_after_wake={reaches_about_to_wait}")),
            "{report}"
        );
        assert_eq!(wakes, 1);
        assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    }
}

/// A delivered heartbeat counts as progress even when the clock cannot order its timestamp after the wake.
#[test]
fn native_watchdog_detects_heartbeat_without_a_clock_tick() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let before_watchdog = Instant::now();
    let mut report = Vec::new();
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            progress.about_to_wait();
            // Freeze the recorded time before the watchdog so callback ordering cannot depend on clock resolution.
            progress.state.lock().unwrap().last_about_to_wait = Some(before_watchdog);
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report).unwrap();
    assert!(report.contains("about_to_wait_after_wake=true"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// A heartbeat recorded before the wake is not a response to it, even when it arrives after the watchdog starts.
#[test]
fn native_watchdog_does_not_count_pre_wake_heartbeat_as_response() {
    use std::time::{Duration, Instant};

    // The first report write runs after the deadline wait and before the wake baseline is sampled.
    struct PreWakeHeartbeat<'a> {
        first_write: Option<&'a NativeProbeProgress>,
        bytes: Vec<u8>,
    }
    impl std::io::Write for PreWakeHeartbeat<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(progress) = self.first_write.take() {
                progress.about_to_wait();
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (progress, events) = NativeProbeProgress::new();
    let mut report = PreWakeHeartbeat { first_write: Some(&progress), bytes: Vec::new() };
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            assert_eq!(
                progress.state.lock().unwrap().heartbeat_count,
                1,
                "heartbeat precedes wake"
            );
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report.bytes).unwrap();
    assert!(report.contains("about_to_wait_after_wake=false"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// Once the deadline expires, a loop that returns during the wake grace period still fails instead of hiding the stall.
#[test]
fn native_watchdog_expiry_cannot_be_erased_by_a_late_disarm() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let mut report = Vec::new();
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            progress.events.send(NativeWatchdogEvent::Disarm).unwrap();
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    assert!(String::from_utf8(report).unwrap().contains("about_to_wait_after_wake=false"));
}

/// Dropping the guard cancels its channel wait without waking, reporting, or invoking the exit action.
#[test]
fn native_watchdog_guard_disarms_before_deadline() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    // Count writes rather than their content: a passing fixture must not produce even a partial report.
    struct ReportWrites(Arc<Mutex<usize>>);
    impl std::io::Write for ReportWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            *self.0.lock().unwrap() += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let writes = Arc::new(Mutex::new(0));
    let exit_code = Arc::new(Mutex::new(None));
    let exit_result = exit_code.clone();
    let (progress, events) = NativeProbeProgress::new();
    let guard = NativeWatchdog::start(
        progress,
        events,
        Instant::now() + Duration::from_secs(180),
        || panic!("a disarmed watchdog must not wake the event loop"),
        ReportWrites(writes.clone()),
        move |code| *exit_result.lock().unwrap() = Some(code),
    );
    drop(guard);
    assert_eq!(*writes.lock().unwrap(), 0);
    assert_eq!(*exit_code.lock().unwrap(), None);
}

#[cfg(windows)]
#[test]
fn native_main_and_child_handlers_preserve_wheel_and_modifier_selection_contracts() {
    // One native loop preserves wheel/selection ownership and proves quiet Ctrl-hover refreshes after parser contention.
    use std::time::{Duration, Instant};
    use winit::{
        application::ApplicationHandler,
        event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent},
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::WindowId,
    };
    struct Probe {
        failures: Vec<String>,
        ran: bool,
        selection_probe: Option<ModifierSelectionProbe>,
        hover_probe: Option<HoverRetryProbe>,
        hover_case: usize,
        progress: std::sync::Arc<NativeProbeProgress>,
        proxy_user_event: bool,
        proxy_about_to_wait: bool,
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            for child in [false, true] {
                let case = if child { "child" } else { "main" };
                self.progress.set("wheel", case, "setup");
                let mut app = App::new(Default::default(), Default::default(), Default::default());
                let main_pane = app.__test_seed_tab("wheel-main");
                let (window, pane_id) = if child {
                    let window = app.__test_seed_child_window(&["wheel-child"]);
                    let pane = app.__test_child_pane_ids(window).unwrap()[0];
                    app.__test_set_child_pane_viewport(
                        window,
                        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                        10.0,
                        10.0,
                    );
                    (window, pane)
                } else {
                    app.__test_set_main_pane_viewport(
                        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                        10.0,
                        10.0,
                    );
                    (app.main_window_id.unwrap(), main_pane)
                };
                self.progress.set("wheel", case, "spawn_pty");
                let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
                    "cmd.exe",
                    &["/D".into(), "/Q".into()],
                    80,
                    24,
                )
                .unwrap();
                let input = pty.input_sender();
                let window_state = app.windows.get_mut(&window).unwrap();
                window_state.cursor_pos = (40.0, 40.0);
                let pane = window_state.panes.get_mut(&pane_id).unwrap();
                pane.pty = Some(pty);
                self.progress.set("wheel", case, "prepare_tracking");
                pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
                pane.viewport_top_abs = Some(10);
                {
                    let mut parser = pane.parser.lock();
                    parser.advance(b"\x1b[?1003h\x1b[?1006h");
                    // Pointer handlers read the published byte, as the VT worker would leave it.
                    pane.__test_publish_input_modes(&parser);
                }
                self.progress.set("wheel", case, "tracked_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    event_loop,
                    window,
                    WindowEvent::MouseWheel {
                        device_id: DeviceId::dummy(),
                        delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                        phase: TouchPhase::Moved,
                    },
                );
                self.progress.set("wheel", case, "wait_for_input");
                let deadline = Instant::now() + Duration::from_secs(1);
                while input.diagnostics().completed_messages == 0 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                self.progress.set("wheel", case, "check_tracked_wheel");
                let completed = input.diagnostics().completed_messages;
                let viewport = app.windows[&window].panes[&pane_id].viewport_top_abs;
                if completed != 1 || viewport != Some(10) {
                    self.failures.push(format!(
                        "child={child}: accepted/completed={completed}, viewport={viewport:?}"
                    ));
                }
                self.progress.set("wheel", case, "reset_tracking");
                {
                    let pane = &app.windows[&window].panes[&pane_id];
                    let mut parser = pane.parser.lock();
                    parser.advance(b"\x1b[?1003l");
                    // The reset reaches the wheel handler only through the published byte.
                    pane.__test_publish_input_modes(&parser);
                }
                self.progress.set("wheel", case, "fallback_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    event_loop,
                    window,
                    WindowEvent::MouseWheel {
                        device_id: DeviceId::dummy(),
                        delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                        phase: TouchPhase::Moved,
                    },
                );
                self.progress.set("wheel", case, "check_fallback");
                let fallback = app.windows[&window].panes[&pane_id].viewport_top_abs;
                if fallback != viewport.map(|top| top.saturating_sub(3)) {
                    self.failures
                        .push(format!("child={child}: reset fallback viewport={fallback:?}"));
                }
                self.progress.set("wheel", case, "teardown");
            }
            self.ran = true;
            self.selection_probe =
                Some(ModifierSelectionProbe::new(event_loop, self.progress.clone()));
        }
        // EventLoop<()> delivers proxy wakes here, not through the production App user-event type.
        fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
            self.proxy_user_event = true;
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if let Some(probe) = self.selection_probe.as_mut() {
                probe.event(event_loop, id, event, &mut self.failures);
            } else if let Some(probe) = self.hover_probe.as_mut() {
                probe.event(event_loop, id, event);
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.progress.about_to_wait();
            self.proxy_about_to_wait |= self.proxy_user_event;
            if self.selection_probe.as_mut().is_some_and(|probe| probe.poll(&mut self.failures)) {
                self.progress.set("selection", "main/child", "teardown");
                self.selection_probe = None;
                self.hover_probe =
                    Some(HoverRetryProbe::new(event_loop, self.hover_case, self.progress.clone()));
            } else if self.hover_probe.as_mut().is_some_and(|probe| probe.poll(event_loop)) {
                let probe = self.hover_probe.as_ref().unwrap();
                self.progress.set("hover", probe.case, "teardown");
                self.hover_probe = None;
                self.hover_case += 1;
                if self.hover_case == 4 {
                    self.progress.set("fixture", "all", "exit_event_loop");
                    event_loop.exit();
                    return;
                }
                self.hover_probe =
                    Some(HoverRetryProbe::new(event_loop, self.hover_case, self.progress.clone()));
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(5),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let (progress, events) = NativeProbeProgress::new();
    let proxy = event_loop.create_proxy();
    let watchdog = NativeWatchdog::start(
        progress.clone(),
        events,
        Instant::now() + Duration::from_secs(180),
        move || proxy.send_event(()).is_ok(),
        std::io::stderr(),
        |code| sonicterm_logging::exit_with(code, "native wheel/selection/hover watchdog expired"),
    );
    let mut probe = Probe {
        failures: Vec::new(),
        ran: false,
        selection_probe: None,
        hover_probe: None,
        hover_case: 0,
        progress,
        proxy_user_event: false,
        proxy_about_to_wait: false,
    };
    // A real proxy event must pass user_event and then about_to_wait; polling alone cannot satisfy this control.
    event_loop.create_proxy().send_event(()).expect("native watchdog proxy control");
    let result = event_loop.run_app(&mut probe);
    drop(watchdog);
    result.unwrap();
    assert!(probe.ran);
    assert!(probe.proxy_user_event, "proxy wake must reach user_event");
    assert!(probe.proxy_about_to_wait, "about_to_wait must follow the proxy user_event");
    assert!(probe.failures.is_empty(), "{}", probe.failures.join("; "));
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum HoverRetryPhase {
    Setup,
    BaselineActive,
    BaselineInactive,
    ContendedActive,
    ContendedInactive,
    PointerReady,
    PointerBaselineBlank,
    PointerBaselineActive,
    PointerContendedBlank,
    PointerContendedActive,
}

#[cfg(windows)]
struct HoverRetryProbe {
    app: App,
    native_id: winit::window::WindowId,
    tracked_id: winit::window::WindowId,
    pane_id: u64,
    case: &'static str,
    progress: std::sync::Arc<NativeProbeProgress>,
    phase: HoverRetryPhase,
    cycle: usize,
    deadline: std::time::Instant,
    phase_started: std::time::Instant,
    last_native_frame: std::time::Instant,
    native_frames: u64,
    phase_native_start: u64,
    phase_present_start: u64,
    active_pixels: Vec<[u8; 4]>,
    inactive_pixels: Vec<[u8; 4]>,
    blank_pixels: Vec<[u8; 4]>,
}

#[cfg(windows)]
impl HoverRetryProbe {
    const URI: &'static str = "https://example.com/docs";
    const QUIET: std::time::Duration = std::time::Duration::from_millis(200);

    /// Publish setup milestones before native window and renderer construction can block.
    fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        case_index: usize,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use sonicterm_cfg::config::{BackdropKind, SoftwareRenderMode, SubpixelAaMode};
        use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
        use std::{sync::Arc, time::Instant};
        use winit::{dpi::PhysicalSize, window::Window};
        let case = ["main/plain", "main/OSC8", "child/plain", "child/OSC8"][case_index];
        progress.set("hover", case, "Setup/create_window");
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_inner_size(PhysicalSize::new(640, 360))
                        .with_active(false)
                        .with_title("SonicTerm quiet hover regression"),
                )
                .unwrap(),
        );
        let native_id = window.id();
        let theme = Theme::default();
        let mut config = Config::default();
        config.font.size = 14.0;
        config.font.subpixel_aa = SubpixelAaMode::Rgb;
        config.appearance.backdrop = BackdropKind::Opaque;
        config.appearance.opacity = 1.0;
        config.appearance.software_render_mode = SoftwareRenderMode::Force;
        config.appearance.scrollbar = ScrollbarMode::Never;
        config.window.padding_left = 0.0;
        config.window.padding_right = 0.0;
        config.window.padding_top = 0.0;
        config.window.padding_bottom = 0.0;
        progress.set("hover", case, "Setup/create_renderer");
        let mut renderer = GpuRenderer::new(
            window.clone(),
            event_loop,
            &theme,
            RendererSettings {
                font_family: &config.font.family,
                font_dirs: &[],
                font_size: config.font.size,
                line_height_mult: config.font.line_height,
                font_weight_scale: config.font.effective_weight_scale(),
                subpixel_aa: config.font.subpixel_aa,
                padding: [0.0; 4],
                appearance: SurfaceAppearance {
                    backdrop: config.appearance.backdrop,
                    opacity: 1.0,
                    scrollbar: config.appearance.scrollbar,
                    panel_padding: 0.0,
                    software_render_mode: SoftwareRenderMode::Force,
                },
                role: "quiet-hover-test",
            },
        )
        .unwrap();
        renderer.set_tab_bar_visible(false);
        renderer.set_cursor_blink(false);
        progress.set("hover", case, "Setup/create_pane");
        let mut app = App::new(theme, config, Keymap::default());
        app.__test_set_software_render_degrade(true);
        let (tracked_id, pane_id) = if case_index < 2 {
            let pane = app.__test_seed_tab("hover-main");
            (app.main_window_id.unwrap(), pane)
        } else {
            let id = app.__test_seed_child_window(&["hover-child"]);
            (id, app.windows[&id].tab_states[0].active_pane)
        };
        assert!(app.__test_attach_window_renderer(tracked_id, window, renderer));
        app.windows.get_mut(&tracked_id).unwrap().cursor_pos = (-100.0, -100.0);
        let label = if case_index.is_multiple_of(2) {
            Self::URI.to_owned()
        } else {
            format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", Self::URI, Self::URI)
        };
        // Identical terminal cells isolate OSC 8 metadata; alternate-screen mouse reporting stays enabled without a PTY.
        {
            let pane = &app.windows[&tracked_id].panes[&pane_id];
            let mut parser = pane.parser.lock();
            parser.advance(
                format!("\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;2H{label}\x1b[6;1H")
                    .as_bytes(),
            );
            // Pointer handlers read the published byte, as the VT worker would leave it.
            pane.__test_publish_input_modes(&parser);
        }
        assert!(app.path_workers.is_none());
        assert!(app.windows[&tracked_id].panes[&pane_id].pty.is_none());
        let now = Instant::now();
        let mut probe = Self {
            app,
            native_id,
            tracked_id,
            pane_id,
            case,
            progress,
            phase: HoverRetryPhase::Setup,
            cycle: 0,
            deadline: now + std::time::Duration::from_secs(20),
            phase_started: now,
            last_native_frame: now,
            native_frames: 0,
            phase_native_start: 0,
            phase_present_start: 0,
            active_pixels: Vec::new(),
            inactive_pixels: Vec::new(),
            blank_pixels: Vec::new(),
        };
        // Focus supplies initial layout independently of the modifier scheduling contract under test.
        probe.progress.set("hover", case, "Setup/focus");
        winit::application::ApplicationHandler::window_event(
            &mut probe.app,
            event_loop,
            tracked_id,
            winit::event::WindowEvent::Focused(true),
        );
        probe.progress.set("hover", case, "Setup");
        probe
    }

    fn assert_unscheduled(&self) {
        assert!(
            !self.app.pending_redraw
                && self.app.pending_redraw_windows.is_empty()
                && self.app.windows[&self.tracked_id].retry_not_before.is_none(),
            "INVALID {} {:?}: pacing or parser retry contaminated the native observation",
            self.case,
            self.phase,
        );
    }

    fn event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if id != self.native_id || !matches!(event, winit::event::WindowEvent::RedrawRequested) {
            return;
        }
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/render cycle={}", self.phase, self.cycle),
        );
        assert!(
            self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.try_lock().is_some(),
            "INVALID {} {:?}: native frame arrived while parser lock was held",
            self.case,
            self.phase,
        );
        self.assert_unscheduled();
        // Backdate pacing only after native delivery; frame admission cannot supply a missing native redraw request.
        assert!(self.app.__test_set_window_last_render(
            self.tracked_id,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        ));
        self.native_frames += 1;
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            event_loop,
            self.tracked_id,
            event,
        );
        self.last_native_frame = std::time::Instant::now();
        self.assert_unscheduled();
    }

    fn row_pixels(&self) -> Vec<[u8; 4]> {
        let renderer = self.app.windows[&self.tracked_id].renderer.as_ref().unwrap();
        let [origin_x, origin_y] =
            renderer.pane_grid_origin(self.pane_id).expect("native layout must exist");
        let (cell_w, cell_h) = renderer.cell_size();
        // Restrict readback to the URI row and prove the real tooltip geometry cannot cover its pixels.
        if let Some(preview) = self.app.windows[&self.tracked_id].link_preview.as_ref() {
            let font_size = renderer.font_size().max(1.0) * renderer.scale_factor();
            let layout = sonicterm_ui::overlays::LinkPreviewLayout::compute(
                &sonicterm_ui::overlays::link_preview_text(&preview.uri),
                preview.pointer,
                renderer.logical_size(),
                font_size * 1.4,
                renderer.scale_factor(),
                |text| renderer.measure_overlay_text_width(text, font_size),
            )
            .expect("baseline preview must fit the native window");
            assert!(
                layout.border.y >= (origin_y + 2.0 * cell_h).ceil()
                    || layout.border.y + layout.border.h <= (origin_y + cell_h).floor(),
                "INVALID {}: link preview overlaps target-row readback",
                self.case,
            );
        }
        ((origin_y + cell_h).floor() as u32..(origin_y + 2.0 * cell_h).ceil() as u32)
            .flat_map(|pixel_y| {
                ((origin_x + cell_w).floor() as u32
                    ..(origin_x + (1 + Self::URI.len()) as f32 * cell_w).ceil() as u32)
                    .map(move |pixel_x| (pixel_x, pixel_y))
            })
            .map(|(pixel_x, pixel_y)| {
                self.app
                    .__test_window_software_frame_pixel_bgra(self.tracked_id, pixel_x, pixel_y)
                    .expect("forced software frame must expose target-row pixels")
            })
            .collect()
    }

    fn assert_hover(&self, active: bool) {
        let hover = self.app.windows[&self.tracked_id]
            .hovered_url
            .as_ref()
            .expect("stationary URI must retain its hover identity");
        assert_eq!(hover.url, Self::URI, "{} {:?}", self.case, self.phase);
        assert_eq!(hover.active(), active, "{} {:?}", self.case, self.phase);
        assert_eq!(self.app.frontmost_window, Some(self.tracked_id));
    }

    /// The watchdog sees every modifier and pointer phase, including its repeated cycle, before the stimulus runs.
    fn begin_phase(&mut self, phase: HoverRetryPhase) {
        self.progress.set("hover", self.case, format!("{phase:?} cycle={}", self.cycle));
        self.assert_unscheduled();
        let period = crate::app::effective_frame_period(true, false, self.app.frame_period);
        assert!(self.app.windows[&self.tracked_id].last_render.elapsed() > period);
        self.phase = phase;
        self.phase_started = std::time::Instant::now();
        self.phase_native_start = self.native_frames;
        self.phase_present_start =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count();
    }

    /// Keep the fixture's lock flag visible throughout the real modifier lookup, but clear it before rendering.
    fn modifiers(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        active: bool,
        contended: bool,
        phase: HoverRetryPhase,
    ) {
        self.begin_phase(phase);
        let parser = self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.clone();
        let previous = self.app.windows[&self.tracked_id].hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            event_loop,
            self.tracked_id,
            winit::event::WindowEvent::ModifiersChanged(
                if active { ModifiersState::CONTROL } else { ModifiersState::empty() }.into(),
            ),
        );
        if contended && active {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            self.assert_hover(false);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else {
            self.assert_hover(active);
        }
        if contended && !active {
            assert!(!self.app.windows[&self.tracked_id].hover_link);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        }
        // The lock covers modifier lookup only; rendering with it held would test a different retry mechanism.
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    /// Diagnose a held-lock pointer lookup without turning its subsequent redraw into a parser-contention test.
    fn pointer_refresh(&mut self, on_uri: bool, contended: bool, phase: HoverRetryPhase) {
        self.begin_phase(phase);
        assert_eq!(self.app.windows[&self.tracked_id].modifiers, ModifiersState::CONTROL);
        let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
        let renderer = window.renderer.as_ref().unwrap();
        let [origin_x, origin_y] = renderer.pane_grid_origin(self.pane_id).unwrap();
        let (cell_w, cell_h) = renderer.cell_size();
        window.cursor_pos = (
            (origin_x + 4.5 * cell_w) as f64,
            (origin_y + if on_uri { 1.5 } else { 5.5 } * cell_h) as f64,
        );
        let parser = window.panes[&self.pane_id].parser.clone();
        let previous = window.hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        // This is the shared pointer-refresh seam, not CursorMoved: its later mouse-report lock would block this fixture.
        self.app.refresh_target_hover(self.tracked_id);
        if contended {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else if on_uri {
            self.assert_hover(true);
        } else {
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
        }
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    fn poll(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) -> bool {
        use HoverRetryPhase::{
            BaselineActive, BaselineInactive, ContendedActive, ContendedInactive,
            PointerBaselineActive, PointerBaselineBlank, PointerContendedActive,
            PointerContendedBlank, PointerReady, Setup,
        };
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/poll cycle={}", self.phase, self.cycle),
        );
        let now = std::time::Instant::now();
        assert!(
            now < self.deadline,
            "INVALID {} {:?}: hover watchdog expired",
            self.case,
            self.phase
        );
        if now.duration_since(self.phase_started.max(self.last_native_frame)) < Self::QUIET {
            return false;
        }
        self.assert_unscheduled();
        let frames = self.native_frames - self.phase_native_start;
        let presents =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count()
                - self.phase_present_start;
        let hover = self.app.windows[&self.tracked_id].hovered_url.as_ref();
        eprintln!(
            "hover case={} phase={:?} cycle={} native_frames={frames} presents={presents} uri={:?} active={:?}",
            self.case,
            self.phase,
            self.cycle,
            hover.map(|hover| hover.url.as_str()),
            hover.map(HoveredUrl::active),
        );
        if matches!(self.phase, ContendedActive | ContendedInactive | PointerContendedActive) {
            assert!(
                frames > 0,
                "{} {:?} cycle={}: 0 native frames in the quiet window",
                self.case,
                self.phase,
                self.cycle
            );
        } else {
            assert!(
                frames > 0,
                "INVALID {} {:?}: baseline received 0 native frames",
                self.case,
                self.phase
            );
        }
        assert!(
            presents > 0,
            "INVALID {} {:?}: native redraw produced no presentation",
            self.case,
            self.phase
        );
        match self.phase {
            Setup => {
                let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
                let renderer = window.renderer.as_ref().unwrap();
                assert!(
                    renderer.__test_pane_focus_flash_target().is_none(),
                    "INVALID: setup focus flash must not affect the baseline"
                );
                let [origin_x, origin_y] =
                    renderer.pane_grid_origin(self.pane_id).expect("setup layout");
                let (cell_w, cell_h) = renderer.cell_size();
                window.cursor_pos =
                    ((origin_x + 4.5 * cell_w) as f64, (origin_y + 1.5 * cell_h) as f64);
                self.modifiers(event_loop, true, false, BaselineActive);
            }
            BaselineActive => {
                self.assert_hover(true);
                self.active_pixels = self.row_pixels();
                self.modifiers(event_loop, false, false, BaselineInactive);
            }
            BaselineInactive => {
                self.assert_hover(false);
                self.inactive_pixels = self.row_pixels();
                assert_ne!(
                    self.active_pixels, self.inactive_pixels,
                    "INVALID {}: baseline target-row pixels do not distinguish Ctrl",
                    self.case
                );
                self.modifiers(event_loop, true, true, ContendedActive);
            }
            ContendedActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{} cycle={}: contended Ctrl must paint the free-lock active row",
                    self.case,
                    self.cycle
                );
                self.modifiers(event_loop, false, true, ContendedInactive);
            }
            ContendedInactive => {
                self.assert_hover(false);
                assert_eq!(
                    self.row_pixels(),
                    self.inactive_pixels,
                    "{} cycle={}: contended release must restore the free-lock inactive row",
                    self.case,
                    self.cycle
                );
                self.cycle += 1;
                if self.cycle == 3 {
                    self.cycle = 0;
                    self.modifiers(event_loop, true, false, PointerReady);
                } else {
                    self.modifiers(event_loop, true, true, ContendedActive);
                }
            }
            PointerReady => {
                self.assert_hover(true);
                self.pointer_refresh(false, false, PointerBaselineBlank);
            }
            PointerBaselineBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                self.blank_pixels = self.row_pixels();
                self.pointer_refresh(true, false, PointerBaselineActive);
            }
            PointerBaselineActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{}: free pointer refresh must match the modifier baseline",
                    self.case
                );
                assert_ne!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "INVALID {}: blank and active pointer baselines must differ",
                    self.case
                );
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
            PointerContendedBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                assert_eq!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "{}: blank row must settle before the held-lock pointer refresh",
                    self.case
                );
                self.pointer_refresh(true, true, PointerContendedActive);
            }
            PointerContendedActive => {
                self.assert_hover(true);
                assert_eq!(self.row_pixels(), self.active_pixels, "{} cycle={}: contended pointer-refresh seam must paint the free-lock active row", self.case, self.cycle);
                self.cycle += 1;
                if self.cycle == 3 {
                    return true;
                }
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
        }
        false
    }
}

#[cfg(windows)]
struct ModifierSelectionProbe {
    app: App,
    progress: std::sync::Arc<NativeProbeProgress>,
    window: winit::window::Window,
    targets: Vec<(winit::window::WindowId, u64, sonicterm_io::pty::PtyInputSender)>,
    steps: std::collections::VecDeque<(bool, u16, u16, bool, bool)>,
    in_flight: Option<(bool, u16, u16, bool, bool)>,
    completed: u64,
    deadline: std::time::Instant,
}

#[cfg(windows)]
impl ModifierSelectionProbe {
    /// Name native construction and per-window PTY setup separately from the subsequent key-delivery phases.
    fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use winit::window::Window;
        progress.set("selection", "main/child", "create_window");
        let window = event_loop
            .create_window(Window::default_attributes().with_visible(false).with_active(false))
            .unwrap();
        let mut app = App::new(Default::default(), Default::default(), Default::default());
        app.clipboard = None;
        app.test_clipboard_text = Some("clipboard sentinel".into());
        app.keymap = Keymap::parse_resilient("[meta]\nname = \"selection\"\nversion = \"1.0\"\n[[binding]]\nkeys = \"ctrl+shift+c\"\naction = \"copy_to_clipboard\"\n", "native selection fixture").unwrap();
        app.__test_enable_pty_write_log();
        let main_pane = app.__test_seed_tab("modifier-main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["modifier-child"]);
        let child_pane = app.__test_child_pane_ids(child).unwrap()[0];
        let mut targets = Vec::new();
        for (window_id, pane_id) in [(main, main_pane), (child, child_pane)] {
            let case = if window_id == main { "main" } else { "child" };
            progress.set("selection", case, "spawn_pty");
            let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
                "cmd.exe",
                &["/D".into(), "/Q".into()],
                80,
                24,
            )
            .unwrap();
            let input = pty.input_sender();
            let pane = app.windows.get_mut(&window_id).unwrap().panes.get_mut(&pane_id).unwrap();
            pane.pty = Some(pty);
            progress.set("selection", case, "prepare_parser");
            let mut parser = pane.parser.lock();
            parser.advance(b"selected text\x1b[?9001h");
            pane.keyboard_input
                .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
            targets.push((window_id, pane_id, input));
        }
        let mut steps = std::collections::VecDeque::new();
        for kitty in [false, true] {
            for (virtual_key, scan) in
                [(0x11, 0x1d), (0x10, 0x2a), (0x43, 0x2e), (0x58, 0x2d), (0x59, 0x15)]
            {
                steps.extend([
                    (kitty, virtual_key, scan, true, false),
                    (kitty, virtual_key, scan, true, true),
                    (kitty, virtual_key, scan, false, false),
                ]);
            }
        }
        progress.set("selection", "main/child", "ready");
        Self {
            app,
            progress,
            window,
            targets,
            steps,
            in_flight: None,
            completed: 0,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(15),
        }
    }

    fn poll(&mut self, failures: &mut Vec<String>) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP},
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("poll in_flight={:?}", self.in_flight),
        );
        if std::time::Instant::now() >= self.deadline {
            failures.push(format!("modifier input deadline: {:?}", self.in_flight));
            return true;
        }
        if self.in_flight.is_some()
            || self
                .targets
                .iter()
                .any(|(_, _, input)| input.diagnostics().completed_messages < self.completed)
        {
            return false;
        }
        let Some(stroke @ (kitty, virtual_key, scan, down, repeat)) = self.steps.pop_front() else {
            return true;
        };
        if down && !repeat {
            for (window_id, pane_id, _) in &self.targets {
                let case =
                    if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
                self.progress.set(
                    "selection",
                    case,
                    format!("prepare kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
                );
                let pane = &self.app.windows[window_id].panes[pane_id];
                let mut parser = pane.parser.lock();
                parser.advance(if kitty { b"\x1b[=10u" } else { b"\x1b[=0u" });
                pane.keyboard_input
                    .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
                drop(parser);
                if virtual_key == 0x43 {
                    // Copy must use the selection preserved across the preceding Shift lifecycle, not a fresh fixture range.
                    continue;
                }
                let mut selection = Selection::new(0, 0);
                selection.end = (0, 8);
                if Some(*window_id) == self.app.main_window_id {
                    self.app.__test_set_main_selection(Some(selection));
                } else {
                    self.app.__test_set_child_selection(*window_id, Some(selection));
                }
            }
        }
        let RawWindowHandle::Win32(handle) = self.window.window_handle().unwrap().as_raw() else {
            panic!("Windows handle");
        };
        let bits = 1
            | (u32::from(scan) << 16)
            | (u32::from(repeat || !down) << 30)
            | (u32::from(!down) << 31);
        self.in_flight = Some(stroke);
        self.progress.set(
            "selection",
            "main/child",
            format!("post kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
        );
        // SAFETY: this test owns the destination HWND; only scalar key metadata is posted to its message queue.
        unsafe {
            PostMessageW(
                Some(HWND(handle.hwnd.get() as *mut _)),
                if down { WM_KEYDOWN } else { WM_KEYUP },
                WPARAM(usize::from(virtual_key)),
                LPARAM(bits as isize),
            )
        }
        .unwrap();
        false
    }

    fn event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
        failures: &mut Vec<String>,
    ) {
        use winit::{event::WindowEvent, platform::windows::KeyEventExtWindows};
        if id != self.window.id() {
            return;
        }
        let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = &event else {
            return;
        };
        let Some((kitty, virtual_key, scan, down, repeat)) = self.in_flight.take() else {
            failures.push("unrequested native key".into());
            return;
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("native_key kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
        );
        let native = key.native_key_event().expect("posted key must carry native metadata");
        assert_eq!(
            (native.virtual_key, native.scan_code, native.key_down),
            (virtual_key, scan, down)
        );
        assert_eq!(key.repeat, repeat);
        let physical = key.physical_key;
        let modifier = virtual_key == 0x11 || virtual_key == 0x10;
        if !modifier {
            assert!(matches!(key.logical_key, winit::keyboard::Key::Character(_)));
        }
        let copy_chord = virtual_key == 0x43;
        for (window_id, pane_id, _) in &self.targets {
            let case = if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
            self.progress.set(
                "selection",
                case,
                format!("dispatch kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
            );
            // The native event is retained; only aggregate modifiers are controlled for copy and AltGr policy checks.
            let mods = if modifier {
                ModifiersState::empty()
            } else if copy_chord {
                ModifiersState::CONTROL | ModifiersState::SHIFT
            } else if virtual_key == 0x58 {
                ModifiersState::CONTROL | ModifiersState::ALT
            } else {
                ModifiersState::empty()
            };
            self.app.windows.get_mut(window_id).unwrap().modifiers = mods;
            let writes_before = self.app.__test_pty_write_log().len();
            winit::application::ApplicationHandler::window_event(
                &mut self.app,
                event_loop,
                *window_id,
                event.clone(),
            );
            self.progress.set(
                "selection",
                case,
                format!("check kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
            );
            let state = &self.app.windows[window_id];
            if state.selection.is_some() != (modifier || copy_chord) {
                failures.push(format!("window={window_id:?} kitty={kitty} vk={virtual_key} down={down} repeat={repeat}: selection_present={}", state.selection.is_some()));
            }
            let held = state.pty_pressed_keys.get(&physical).and_then(|routes| routes.get(pane_id));
            assert_eq!(
                held.is_some(),
                down && !copy_chord,
                "only admitted press/repeat owns a matching release"
            );
            if down && !copy_chord {
                assert_eq!(matches!(held, Some(crate::app::HeldKey::Legacy)), kitty);
            }
            let writes = self.app.__test_pty_write_log();
            if copy_chord {
                assert_eq!(writes.len(), writes_before, "copy shortcut never leaks terminal input");
                if down {
                    assert_eq!(self.app.test_clipboard_text.as_deref(), Some("selected"));
                    self.app.test_clipboard_text = Some("clipboard sentinel".into());
                }
            } else {
                assert_eq!(writes.len(), writes_before + 1);
                let bytes = &writes.last().unwrap().1;
                if kitty {
                    assert!(
                        bytes.starts_with(b"\x1b[") && bytes.ends_with(b"u"),
                        "Kitty report-all record: {bytes:?}"
                    );
                } else {
                    assert_eq!(
                        *bytes,
                        crate::app::key_encoding::encode_win32_key(
                            crate::app::key_encoding::Win32KeyEvent {
                                virtual_key: native.virtual_key,
                                scan_code: native.scan_code,
                                unicode: &native.unicode,
                                key_down: native.key_down,
                                control_key_state: native.control_key_state,
                                repeat_count: native.repeat_count
                            }
                        )
                    );
                }
            }
        }
        if !copy_chord {
            self.completed += 1;
        }
        assert_eq!(self.app.test_clipboard_text.as_deref(), Some("clipboard sentinel"));
    }
}

#[test]
fn hover_moves_request_a_native_redraw_only_when_the_hovered_tab_changes() {
    // The renderer cannot be built off a native window, so the source pins the
    // callers: every hover update passes its own window's tab bar, and the main
    // and child move handlers ask for a native redraw only on its result, so a
    // sweep inside one tab issues none and a tab crossing issues one.
    let sources = [
        ("window_pointer.rs", include_str!("window_pointer.rs").replace("\r\n", "\n")),
        ("child_window_pointer.rs", include_str!("child_window_pointer.rs").replace("\r\n", "\n")),
        ("child_window.rs", include_str!("child_window.rs").replace("\r\n", "\n")),
    ];
    let mut call_count = 0;
    for (name, source) in &sources {
        assert!(!source.contains("muted ×"), "{name} keeps a stale close-button comment");
        for (offset, _) in source.match_indices("set_hover_cursor(") {
            let call = &source[offset..offset + source[offset..].find(')').unwrap() + 40];
            assert!(
                call.contains("&window.tabs)") || call.contains("&child.tabs)"),
                "{name}: hover update without its window's tab bar: {call}"
            );
            call_count += 1;
        }
    }
    assert_eq!(call_count, 4, "main move, main leave, child move and child leave");
    let main = &sources[0].1;
    let moved = main.find("fn handle_main_cursor_moved(").expect("main move handler");
    let moved = &main[moved..];
    let update = moved.find("hover_redraw = renderer.set_hover_cursor(").expect("hover update");
    let guard = moved.find("if hover_redraw {").expect("redraw guard");
    let request = moved.find("request_native_redraw(main_window)").expect("redraw request");
    assert!(update < guard && guard < request, "the main redraw follows the hover result");
    let child = &sources[1].1;
    assert!(child.contains(
        "if renderer.set_hover_cursor(Some((cursor_x, cursor_y)), &child.tabs) {\n            if let Some(window) = child.window.as_ref() {\n                crate::app::frame_counters::request_native_redraw(window);"
    ));
}

/// A pointer handler reads a pane's mouse modes while another thread holds that pane's
/// parser (as the VT worker does during a large parse) and returns without waiting.
#[test]
fn pointer_modes_read_while_the_parser_is_held_by_another_thread() {
    let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(80, 24));
    parser.advance(b"\x1b[?1003h\x1b[?1006h");
    let pane =
        crate::app::PaneState::new(std::sync::Arc::new(parking_lot::Mutex::new(parser)), None);
    let parser = pane.parser.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        held_tx.send(()).unwrap();
        // Hold the parser until the reader has finished or the test gives up.
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
    });
    held_rx.recv().unwrap();
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| read_tx.send(pane.pointer_modes()).unwrap());
        // A blocking read would wait for the holder; the published byte answers at once.
        let modes = read_rx.recv_timeout(std::time::Duration::from_secs(2));
        release_tx.send(()).unwrap();
        let modes = modes.expect("pointer mode read waited for the parser lock");
        assert_eq!(modes.tracking(), sonicterm_vt::vt::MouseTracking::AnyMotion);
        assert!(modes.sgr());
    });
    holder.join().unwrap();
}

/// Every main and child pointer route reads the published byte: the blocking profile
/// helper is gone, and the only parser lock left in the child pointer file is the
/// scrollbar-drag viewport baseline, which needs grid state the byte does not carry.
#[test]
fn pointer_routes_read_published_modes_without_a_parser_lock() {
    let main = include_str!("window_pointer.rs").replace("\r\n", "\n");
    let child = include_str!("child_window_pointer.rs").replace("\r\n", "\n");
    let event = include_str!("window_event.rs").replace("\r\n", "\n");
    assert!(
        !event.contains("fn parser_mouse_profile"),
        "the blocking profile helper must stay deleted"
    );
    for (name, source, lock_count) in
        [("window_pointer.rs", &main, 0), ("child_window_pointer.rs", &child, 1)]
    {
        assert!(
            !source.contains("parser_mouse_profile"),
            "{name} still reads modes under the parser"
        );
        assert_eq!(
            source.matches(".pointer_modes()").count(),
            3,
            "{name}: motion, wheel and press"
        );
        assert_eq!(source.matches("lock_parser(").count(), lock_count, "{name} parser locks");
    }
    let retained = child.find("lock_parser(").unwrap();
    assert!(child[retained..retained + 200].contains("ViewportBaseline::of"));
}

/// Hold `parser` on another thread until the returned sender fires or three seconds pass.
fn hold_parser(
    parser: std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(3));
    });
    held_rx.recv().unwrap();
    (holder, release_tx)
}

/// Published modes paired with what they must produce: the parser itself keeps every mode
/// off on the primary screen, so only the published byte can route these.
fn published_cases() -> [(sonicterm_vt::vt::PointerModes, Vec<u8>); 2] {
    use sonicterm_vt::vt::{MouseTracking, PointerModes};
    [
        // Any-motion SGR tracking: no-button motion at column 3, row 2 reports button 35.
        (
            PointerModes::new(MouseTracking::AnyMotion, true, false, false),
            b"\x1b[<35;3;2M".to_vec(),
        ),
        // Untracked alternate screen with DECCKM: motion reports nothing.
        (PointerModes::new(MouseTracking::Off, false, true, true), Vec::new()),
    ]
}

/// Unheld motion and the wheel go through the real main-window handlers while another thread
/// holds the pane's parser (as the VT worker does during a large parse): they return within a
/// bound, and the bytes follow the published modes, not the parser's own state.
// Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
#[test]
fn main_pointer_handlers_route_by_published_modes_while_the_parser_is_held() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::{dpi::PhysicalPosition, event::MouseScrollDelta};
    let wheel_bytes = [b"\x1b[<64;3;2M".repeat(3), b"\x1bOA".repeat(3)];
    for ((modes, motion), wheel) in published_cases().into_iter().zip(wheel_bytes) {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        let pane_id = app.__test_seed_tab("main");
        app.test_viewport_override =
            Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
        app.__test_enable_pty_write_log();
        let pane = &app.main().unwrap().panes[&pane_id];
        pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
        let (holder, release) = hold_parser(pane.parser.clone());

        let started = std::time::Instant::now();
        app.handle_main_cursor_moved(PhysicalPosition::new(25.0, 15.0));
        app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
        let elapsed = started.elapsed();
        let _ = release.send(());
        holder.join().unwrap();

        assert!(elapsed < std::time::Duration::from_secs(1), "handlers waited {elapsed:?}");
        let expected: Vec<(u64, Vec<u8>)> = [motion, wheel]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{modes:?}");
    }
}

/// Unheld motion and the wheel go through the real child-window handlers while the parser is
/// held: they return within a bound, motion reports follow the published modes, and the wheel
/// never falls back to local scrollback (which would lock the parser) when the published
/// modes route it to the terminal.
// Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
#[test]
fn child_pointer_handlers_route_by_published_modes_while_the_parser_is_held() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::{dpi::PhysicalPosition, event::MouseScrollDelta};
    for (modes, motion) in published_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let child = app.__test_seed_child_window(&["child"]);
        assert!(app.__test_set_child_pane_viewport(
            child,
            sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
            10.0,
            10.0,
        ));
        let pane_id = app.__test_child_active_pane(child).unwrap();
        app.__test_enable_pty_write_log();
        let pane = &app.windows[&child].panes[&pane_id];
        pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
        let (holder, release) = hold_parser(pane.parser.clone());
        let config = app.config.clone();
        let position = PhysicalPosition::new(25.0, 15.0);

        let started = std::time::Instant::now();
        if !app.handle_child_cursor_moved_chrome(child, &position) {
            app.handle_child_cursor_moved(child, position, &config);
        }
        crate::app::App::handle_child_mouse_wheel(
            app.windows.get_mut(&child).unwrap(),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            &None,
            config.appearance.scrollbar,
        );
        let elapsed = started.elapsed();
        let _ = release.send(());
        holder.join().unwrap();

        assert!(elapsed < std::time::Duration::from_secs(1), "handlers waited {elapsed:?}");
        let expected: Vec<(u64, Vec<u8>)> = [motion]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{modes:?}");
        assert_eq!(app.windows[&child].panes[&pane_id].viewport_top_abs, None, "no local scroll");
    }
}

/// Advance `pane`'s parser with each batch and publish its modes after every batch, as the VT
/// worker does; a fixture without a VT worker must do the same before driving a handler.
fn advance_published(pane: &crate::app::PaneState, batches: &[&[u8]]) {
    let mut parser = pane.parser.lock();
    for batch in batches {
        parser.advance(batch);
        pane.__test_publish_input_modes(&parser);
    }
}

/// Mode batches the Windows native fixtures feed through the parser, with the wheel bytes and
/// viewport each must leave: untracked alternate screen sends arrows, any-motion SGR tracking
/// sends a report at row 2 column 3, and a published tracking reset falls back to local scroll.
fn parser_mode_cases() -> [(&'static str, Vec<&'static [u8]>, Vec<u8>, Option<u64>); 3] {
    let reset: &[u8] = b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006h";
    [
        ("alt", vec![reset, b"\x1b[?1049h"], b"\x1b[A".repeat(3), Some(10)),
        ("tracked", vec![reset, b"\x1b[?1003h"], b"\x1b[<64;3;2M".repeat(3), Some(10)),
        ("reset", vec![reset, b"\x1b[?1003h", b"\x1b[?1003l"], Vec::new(), Some(7)),
    ]
}

/// Modes set through the parser reach the real main wheel handler once published: alternate
/// screen, tracking and a tracking reset each route the wheel as the Windows native matrix expects.
#[test]
fn main_wheel_follows_modes_published_from_parser_batches() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::event::MouseScrollDelta;
    for (name, batches, wheel, viewport) in parser_mode_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        let pane_id = app.__test_seed_tab("main");
        app.test_viewport_override =
            Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
        app.__test_enable_pty_write_log();
        let window = app.main_mut().unwrap();
        // Column 3, row 2 of the 10-pixel cells, matching the report in the tracked case.
        window.cursor_pos = (25.0, 15.0);
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
        advance_published(pane, &batches);
        pane.viewport_top_abs = Some(10);

        app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));

        let expected: Vec<(u64, Vec<u8>)> = [wheel]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{name}");
        assert_eq!(app.main().unwrap().panes[&pane_id].viewport_top_abs, viewport, "{name}");
    }
}

/// The child wheel handler reads the same published byte: only a published tracking reset lets
/// the wheel scroll the child pane locally; alternate screen and tracking keep the viewport.
#[test]
fn child_wheel_follows_modes_published_from_parser_batches() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::event::MouseScrollDelta;
    for (name, batches, _, viewport) in parser_mode_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let child = app.__test_seed_child_window(&["child"]);
        assert!(app.__test_set_child_pane_viewport(
            child,
            sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
            10.0,
            10.0,
        ));
        let pane_id = app.__test_child_active_pane(child).unwrap();
        let config = app.config.clone();
        let window = app.windows.get_mut(&child).unwrap();
        window.cursor_pos = (25.0, 15.0);
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
        advance_published(pane, &batches);
        pane.viewport_top_abs = Some(10);

        crate::app::App::handle_child_mouse_wheel(
            window,
            MouseScrollDelta::LineDelta(0.0, 1.0),
            &None,
            config.appearance.scrollbar,
        );

        assert_eq!(app.windows[&child].panes[&pane_id].viewport_top_abs, viewport, "{name}");
    }
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // When: a subdirectory holds more modules, so its sources are scanned too.
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Blank `range` of `view` with spaces, keeping newlines so line numbers survive.
fn blank_span(view: &mut [u8], range: std::ops::Range<usize>) {
    for byte in &mut view[range] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Whether `byte` can continue a Rust identifier.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The quote offset and `#` count of a raw string literal (`r"`, `r#"`, `br#"`) starting at
/// `offset`, or `None` when no raw string starts there.
fn raw_string_opener(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset > 0 && is_ident_byte(bytes[offset - 1]) {
        // When: the `r` continues an identifier such as `parser`, so no literal starts here.
        return None;
    }
    let after_prefix = match bytes.get(offset..offset + 2) {
        Some([b'b', b'r']) => offset + 2,
        Some([b'r', _]) => offset + 1,
        _ => return None,
    };
    let hash_count = bytes[after_prefix..].iter().take_while(|byte| **byte == b'#').count();
    (bytes.get(after_prefix + hash_count) == Some(&b'"'))
        .then_some((after_prefix + hash_count, hash_count))
}

/// Two views of `text` with the same byte offsets: `code` blanks comments and keeps literals, so
/// mode bytes inside an `advance` argument stay visible; `bare` also blanks the contents of
/// string and char literals, so only real code can name a call, a binding or a handler.
fn code_views(text: &str) -> (String, String) {
    let bytes = text.as_bytes();
    let mut code = bytes.to_vec();
    let mut bare = bytes.to_vec();
    let mut offset = 0;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.starts_with(b"//") {
            let end =
                rest.iter().position(|byte| *byte == b'\n').map_or(bytes.len(), |len| offset + len);
            blank_span(&mut code, offset..end);
            blank_span(&mut bare, offset..end);
            offset = end;
        } else if rest.starts_with(b"/*") {
            // Block comments nest in Rust, so the scan counts openers and closers.
            let mut depth = 0usize;
            let mut cursor = offset;
            while cursor < bytes.len() {
                if bytes[cursor..].starts_with(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if bytes[cursor..].starts_with(b"*/") {
                    depth -= 1;
                    cursor += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    cursor += 1;
                }
            }
            blank_span(&mut code, offset..cursor);
            blank_span(&mut bare, offset..cursor);
            offset = cursor;
        } else if let Some((quote, hash_count)) = raw_string_opener(bytes, offset) {
            let closer: Vec<u8> =
                std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hash_count)).collect();
            let body = quote + 1;
            let close = bytes[body..]
                .windows(closer.len())
                .position(|window| window == closer.as_slice())
                .map_or(bytes.len(), |len| body + len);
            blank_span(&mut bare, body..close);
            offset = (close + closer.len()).min(bytes.len());
        } else if bytes[offset] == b'"' {
            let body = offset + 1;
            let mut cursor = body;
            while cursor < bytes.len() && bytes[cursor] != b'"' {
                // An escape consumes the next byte, which may be a quote.
                cursor += if bytes[cursor] == b'\\' { 2 } else { 1 };
            }
            let close = cursor.min(bytes.len());
            blank_span(&mut bare, body..close);
            offset = close + 1;
        } else if bytes[offset] == b'\'' {
            // A char literal closes within a few bytes; anything else is a lifetime or label.
            let body = offset + 1;
            let close = if bytes.get(body) == Some(&b'\\') {
                bytes[body..(body + 12).min(bytes.len())]
                    .iter()
                    .skip(2)
                    .position(|byte| *byte == b'\'')
                    .map(|len| body + 2 + len)
            } else {
                text[body..]
                    .chars()
                    .next()
                    .map(|first| body + first.len_utf8())
                    .filter(|after| bytes.get(*after) == Some(&b'\''))
            };
            if let Some(close) = close {
                blank_span(&mut bare, body..close);
                offset = close + 1;
            } else {
                offset += 1;
            }
        } else {
            offset += 1;
        }
    }
    // Every blanked span covers whole characters, so both views stay valid UTF-8.
    (String::from_utf8(code).unwrap(), String::from_utf8(bare).unwrap())
}

/// Offsets in `bare` where `needle` starts and is not the tail of a longer identifier.
fn token_offsets<'a>(bare: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    bare.match_indices(needle).map(|(offset, _)| offset).filter(move |offset| {
        *offset == 0 || !is_ident_byte(bare.as_bytes()[*offset - 1]) || needle.starts_with('.')
    })
}

/// The method receiver that ends at `dot` in `bare`, with whitespace removed: the expression
/// back to the enclosing statement, argument or block boundary.
fn receiver_before(bare: &str, dot: usize) -> String {
    let bytes = bare.as_bytes();
    let mut cursor = dot;
    let mut depth = 0usize;
    while cursor > 0 {
        match bytes[cursor - 1] {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth == 0 => break,
            b'(' | b'[' => depth -= 1,
            b';' | b'{' | b'}' | b'=' | b',' if depth == 0 => break,
            _ => {}
        }
        cursor -= 1;
    }
    bare[cursor..dot].split_whitespace().collect::<String>().trim_start_matches('&').to_owned()
}

/// The text between the parenthesis at `open` in `bare` and its match, as a byte range.
fn call_arguments(bare: &str, open: usize) -> std::ops::Range<usize> {
    let mut depth = 0usize;
    for (offset, byte) in bare.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return open + 1..offset;
                }
            }
            _ => {}
        }
    }
    open + 1..bare.len()
}

/// One code event the publication scan orders by offset.
enum ScanEvent {
    /// A function starts: bindings and unpublished changes from the previous one end.
    FunctionStart,
    /// `let <name> = <pane>.parser.lock();` binds a parser guard to a pane.
    GuardBinding { name: String, pane: String },
    /// A raw `advance` whose argument carries pointer-mode bytes, on this receiver.
    ModeChange { receiver: String },
    /// `<pane>.__test_publish_input_modes(&<guard>)`.
    Publish { pane: String, guard: String },
    /// A native pointer event or a direct pointer-handler call.
    Handler,
}

/// Line pairs `(mode change, handler)` where a function in `text` changes pointer modes with a
/// raw `advance` and then drives a pointer handler before publishing that pane's modes.
///
/// Comments and the contents of string and char literals never count, and events are ordered by
/// byte offset, so a publish later on the handler's line is too late. A publish counts only when
/// its guard argument is a `let` binding of `<pane>.parser.lock()` and its receiver is that same
/// `<pane>` text as the advanced parser's. The scan does not prove that the handler's cursor
/// targets that pane, and a nested `fn` ends the enclosing function's pending changes.
fn unpublished_mode_changes(text: &str) -> Vec<(usize, usize)> {
    let mode_needles: Vec<String> = ["1049", "1047", "47", "1000", "1002", "1003", "1006", "1"]
        .iter()
        .flat_map(|mode| [format!("[?{mode}h"), format!("[?{mode}l")])
        .chain([format!("{}x1bc", '\\')])
        .collect();
    let (code, bare) = code_views(text);
    let mut events: Vec<(usize, ScanEvent)> = Vec::new();
    events.extend(token_offsets(&bare, "fn ").map(|offset| (offset, ScanEvent::FunctionStart)));
    for offset in token_offsets(&bare, "let ") {
        let statement_end = bare[offset..].find(';').map_or(bare.len(), |len| offset + len);
        let Some((left, right)) = bare[offset + 4..statement_end].split_once('=') else {
            // When: a `let` without `=` binds nothing the scan can resolve.
            continue;
        };
        let name = left.trim().trim_start_matches("mut ").trim();
        let right: String = right.split_whitespace().collect();
        if let Some(pane) = right.strip_suffix(".parser.lock()") {
            if !name.is_empty() && name.bytes().all(is_ident_byte) {
                let (name, pane) = (name.to_owned(), pane.trim_start_matches('&').to_owned());
                events.push((offset, ScanEvent::GuardBinding { name, pane }));
            }
        }
    }
    for offset in token_offsets(&bare, ".advance(") {
        let arguments = call_arguments(&bare, offset + ".advance".len());
        if mode_needles.iter().any(|needle| code[arguments.clone()].contains(needle.as_str())) {
            let receiver = receiver_before(&bare, offset);
            events.push((offset, ScanEvent::ModeChange { receiver }));
        }
    }
    let publish_call = ".__test_publish_input_modes(";
    for offset in token_offsets(&bare, publish_call) {
        let arguments = call_arguments(&bare, offset + publish_call.len() - 1);
        let guard = bare[arguments].split_whitespace().collect::<String>();
        let guard = guard.trim_start_matches('&').to_owned();
        events.push((offset, ScanEvent::Publish { pane: receiver_before(&bare, offset), guard }));
    }
    for handler in POINTER_HANDLERS {
        events.extend(token_offsets(&bare, handler).map(|offset| (offset, ScanEvent::Handler)));
    }
    events.sort_by_key(|(offset, _)| *offset);

    let line_of = |offset: usize| bare[..offset].matches('\n').count() + 1;
    let mut found = Vec::new();
    // Guard name -> pane, and pane key -> line of its newest unpublished mode change.
    let mut guards: std::collections::HashMap<String, String> = Default::default();
    let mut unpublished: std::collections::BTreeMap<String, usize> = Default::default();
    for (offset, event) in events {
        match event {
            ScanEvent::FunctionStart => {
                guards.clear();
                unpublished.clear();
            }
            ScanEvent::GuardBinding { name, pane } => {
                guards.insert(name, pane);
            }
            ScanEvent::ModeChange { receiver } => {
                // An unresolvable receiver gets a key no publish can match, so it is never cleared.
                let pane = receiver
                    .strip_suffix(".parser.lock()")
                    .map(str::to_owned)
                    .or_else(|| guards.get(&receiver).cloned())
                    .unwrap_or_else(|| format!("unresolved {receiver}"));
                unpublished.insert(pane, line_of(offset));
            }
            ScanEvent::Publish { pane, guard } => {
                if guards.get(&guard) == Some(&pane) {
                    unpublished.remove(&pane);
                }
            }
            ScanEvent::Handler => {
                if let Some(changed) = unpublished.values().min() {
                    found.push((*changed, line_of(offset)));
                    unpublished.clear();
                }
            }
        }
    }
    found
}

/// Tokens that drive a real pointer handler: a native event or a direct handler call.
const POINTER_HANDLERS: [&str; 7] = [
    "MouseWheel {",
    "MouseInput {",
    "CursorMoved {",
    "handle_main_mouse_wheel(",
    "handle_child_mouse_wheel(",
    "handle_main_cursor_moved(",
    "handle_child_cursor_moved(",
];

/// Snippets the scan must reject: each changes modes and then drives a handler while the only
/// publication is commented out, after the handler, of another pane or with another pane's
/// guard, or inside a string.
const UNPUBLISHED_FIXTURES: [(&str, &str); 6] = [
    (
        "line-commented publish",
        r#"fn case() {
    let pane = &window.panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    // pane.__test_publish_input_modes(&parser);
    drop(parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish after the handler on one line",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    app.handle_main_mouse_wheel(delta); pane.__test_publish_input_modes(&parser);
}"#,
    ),
    (
        "publish of another pane",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1003l");
    let other_guard = other.parser.lock();
    other.__test_publish_input_modes(&other_guard);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish with another pane's guard",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    let other_guard = other.parser.lock();
    parser.advance(b"\x1b[?1049h");
    pane.__test_publish_input_modes(&other_guard);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "block-commented publish",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1003l");
    /* pane.__test_publish_input_modes(&parser); */
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish named in a string literal",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1bc");
    let note = "pane.__test_publish_input_modes(&parser);";
    app.handle_child_mouse_wheel(window, delta, &None, mode);
}"#,
    ),
];

/// Snippets the scan must accept: a publication of the advanced pane before the handler, a
/// rustfmt-wrapped `advance`, and mode bytes that appear only inside a string literal.
const PUBLISHED_FIXTURES: [(&str, &str); 3] = [
    (
        "publish then handler",
        r#"fn case() {
    let pane = &window.panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    pane.__test_publish_input_modes(&parser);
    drop(parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "multi-line advance then publish",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(
        b"\x1b[?1049h\x1b[?1003h\x1b[?1006h",
    );
    pane.__test_publish_input_modes(&parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "mode bytes only in a string",
        r#"fn case() {
    let snippet = "parser.advance(b\"\\x1b[?1049h\");";
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
];

/// The scan rejects every unpublished fixture and accepts every published one, using in-memory
/// sources so its comment, string, order and receiver rules are checked without the filesystem.
#[test]
fn mode_publication_scan_rejects_unpublished_and_accepts_published_fixtures() {
    let mut wrong = Vec::new();
    for (name, source) in UNPUBLISHED_FIXTURES {
        if unpublished_mode_changes(source).is_empty() {
            wrong.push(format!("accepted {name}"));
        }
    }
    for (name, source) in PUBLISHED_FIXTURES {
        let found = unpublished_mode_changes(source);
        if !found.is_empty() {
            wrong.push(format!("rejected {name}: {found:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Handlers read the published byte, and only the VT worker publishes it, so a fixture that
/// changes pointer modes with a raw `advance` must publish before it drives a pointer handler.
/// The scan reads Windows-gated fixtures as text, so a macOS run catches one that does not.
#[test]
fn fixtures_publish_parser_pointer_modes_before_driving_a_handler() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("tests"), &mut sources);
    let mut failures = Vec::new();
    for path in sources {
        let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        for (changed, handler) in unpublished_mode_changes(&text) {
            failures.push(format!(
                "{}:{changed} changes modes, line {handler} drives a handler unpublished",
                path.strip_prefix(root).unwrap().display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
