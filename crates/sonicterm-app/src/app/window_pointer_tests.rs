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
                pane.parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
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
                app.windows
                    .get_mut(&window)
                    .unwrap()
                    .panes
                    .get_mut(&pane_id)
                    .unwrap()
                    .parser
                    .lock()
                    .advance(b"\x1b[?1003l");
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
        app.windows[&tracked_id].panes[&pane_id].parser.lock().advance(
            format!("\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;2H{label}\x1b[6;1H")
                .as_bytes(),
        );
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
