//! The probe: an `ApplicationHandler` around the real `App`, on macOS and Windows.
//!
//! It forwards lifecycle, redraw and user events, records and drops the measurement window's
//! native focus, refuses unexpected physical input before the App sees it, and injects synthetic
//! input on a schedule merged with the App's own `ControlFlow`. Around every forwarded dispatch
//! it counts presented frames and `RedrawRequested` time and, while an S2 sample is open, takes
//! nonblocking grid snapshots. Scratch files are polled only outside measured phases, and
//! `progress.json` is written between phases.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sonicterm_app::app::{App, PaneState, UserEvent};
use sonicterm_cfg::config::{Config, SoftwareRenderMode};
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{
    DeviceId, ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use crate::cli::{RunArgs, REFUSED};
use crate::record::{
    attribute_dispatch, echo_target, line_near_cursor, presenter_blocked, prompt_origin,
    snapshot_echo, write_progress, Attribution, CheckpointRecord, DispatchObservation,
    EchoSnapshot, EchoTarget, LatencySample, Measurements, MonitorInfo, PhaseRecord,
    PresenterRecord, RunResult, Status, Throughput, UnattributedReason, CREDITED,
};
use crate::scan_throttle::{ScanThrottle, ScanTrigger};
use crate::scenarios::{self, Act, Driver, PhaseEnd, PhaseSpec, Plan, SetupAction, Step, Workload};
use crate::waits::{
    self, CheckpointProgress, FirstPresentBound, ImagePresent, ImageProgress, FIRST_PRESENT_WAIT,
    IMAGE_PRESENT_WAIT,
};
use crate::workload;

/// Target of every harness log line. The `info` filter admits `sonicterm` and its children but
/// no directive matches the example's own crate path, so that default target would be dropped.
const LOG_TARGET: &str = "sonicterm::perf_scenarios";
/// File polls outside measured phases: sessions, acknowledgements, READY and checkpoint `.done`.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Least spacing between two grid scans for a sentinel, the prompt or an image.
const SCAN_INTERVAL: Duration = Duration::from_millis(20);
/// How long a covered or uncovered window waits for the native occlusion event.
const OCCLUSION_FALLBACK: Duration = Duration::from_secs(2);
/// How long a managed checkpoint waits for the comparison script's `.done`; past it the run
/// ends invalid.
const CHECKPOINT_WAIT: Duration = Duration::from_secs(60);
/// How long past the run's deadline the event loop may take to return before the watchdog aborts.
const WATCHDOG_GRACE: Duration = Duration::from_secs(30);
/// Rows above the cursor scanned for a READY line or a sentinel; zsh's prompt adds one row.
const PROTOCOL_ROWS: u16 = 3;

/// Where the run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Before the warm-pool barrier, the end of the first `about_to_wait` after a present.
    Startup,
    /// Opening roles one at a time; each waits for the previous role's session record.
    Setup { next_action: usize },
    /// Waiting for (managed) or writing (unmanaged) every role's acknowledgement.
    Acks,
    /// Waiting for every role's READY line in its grid.
    Ready,
    /// Running plan step `index`; GO is written as the first phase starts.
    Steps(usize),
    /// Finished; nothing more reaches the App.
    Done,
}

/// What a forwarded dispatch was, for the per-phase counts and the scan throttle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dispatch {
    /// `RedrawRequested` for the measurement window.
    Redraw,
    /// Another dispatch from outside the harness: a native window event or a user event.
    Native,
    /// A loop turn (`new_events`, `about_to_wait`), a setup action, a synthetic injection, or an
    /// occlusion state the probe delivers; occlusion changes no grid content.
    Harness,
}

impl Dispatch {
    /// Whether a throttled scan in this dispatch may arm the trailing check.
    fn scan_trigger(self) -> ScanTrigger {
        match self {
            Self::Redraw | Self::Native => ScanTrigger::App,
            Self::Harness => ScanTrigger::Harness,
        }
    }
}

/// How the probe treats one native window event.
#[derive(Clone, Copy, Debug)]
enum Arrival {
    Redraw,
    Focus,
    Occlusion(bool),
    Routed(Route),
}

/// What happens to an ordinary native window event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// The App receives it.
    Forward,
    /// It carries no user input, so it is dropped.
    Drop,
    /// It is physical input; the run is void before the App sees it.
    Refuse(&'static str),
}

/// One measured phase's running totals.
struct PhaseMeter {
    name: &'static str,
    started: Instant,
    start_unix_s: f64,
    cpu_start: (f64, f64),
    presented_frames: u64,
    redraw_requested: u64,
    dispatch_ms: Vec<f64>,
    present_interval_ms: Vec<f64>,
    allocations: Option<Vec<u64>>,
    last_present: Option<Instant>,
}

/// The synthetic input a phase is injecting.
enum DriverState {
    None,
    Typing(Typing),
    Sweep(Sweep),
    Drag(Drag),
    Wheel(Wheel),
}

/// S2 typing: one `Ime::Commit` per character once the prompt shows, then a settle.
struct Typing {
    role: usize,
    pane: u64,
    chars: u32,
    interval: Duration,
    settle: Duration,
    /// Where typing starts, once the prompt is in the grid.
    origin: Option<(u64, u16)>,
    cols: u16,
    first_at: Option<Instant>,
    typed: u32,
    done_at: Option<Instant>,
}

/// S6 hover sweep across the tab bar and the grid.
struct Sweep {
    interval: Duration,
    hertz: u32,
    started: Instant,
    ticks: u32,
    lanes: [f64; 4],
    left: f64,
    right: f64,
}

/// S6 selection drag inside the grid.
struct Drag {
    interval: Duration,
    started: Instant,
    ticks: u32,
    cycle_ticks: u32,
    move_ticks: u32,
    start: (f64, f64),
    end: (f64, f64),
    pressed: bool,
    deadline: Instant,
    finished: bool,
}

/// S7 wheel: line ticks up through the retained history, then back down.
struct Wheel {
    interval: Duration,
    started: Instant,
    sent: u32,
    total: u32,
}

/// An S2 sample between its injection and its attribution.
struct OpenSample {
    pane: u64,
    target: EchoTarget,
    injected: Instant,
    inject_unix_s: f64,
}

/// Native occlusion as delivered to the App, and the harness's own cover.
#[derive(Default)]
struct OcclusionState {
    /// The state the App last received, native or synthetic.
    delivered: Option<bool>,
    /// The harness's cover is up.
    cover_up: bool,
    /// A state the harness caused and its synthetic deadline.
    wait: Option<(bool, Instant)>,
    /// When `Occluded(false)` reached the App after an uncover.
    uncover_from: Option<Instant>,
    /// The harness has uncovered, so the next `Occluded(false)` starts the uncover time.
    uncover_requested: bool,
}

/// S11: the role whose image is awaited, and whether a frame known to show it has presented.
#[derive(Default)]
struct ImageState {
    role: Option<usize>,
    present: ImagePresent,
}

/// A managed checkpoint waiting for the comparison script's `.done`.
struct CheckpointWait {
    stem: String,
    done: PathBuf,
    json: PathBuf,
    relative_json: String,
    deadline: Instant,
}

/// The probe around one run's `App`.
struct Probe {
    app: App,
    plan: Arc<Plan>,
    request: RunArgs,
    scratch: PathBuf,
    sentinels: Vec<String>,
    allocation_counter: Option<fn() -> u64>,
    run_deadline: Instant,
    stage: Stage,
    main_id: Option<WindowId>,
    cover: Option<Window>,
    occlusion: OcclusionState,
    frames: u64,
    first_present: bool,
    meter: Option<PhaseMeter>,
    phases: Vec<PhaseRecord>,
    role_panes: Vec<u64>,
    go_at: Option<Instant>,
    sentinel_roles: Vec<usize>,
    sentinel_seen: Vec<Option<Instant>>,
    image: ImageState,
    scan: ScanThrottle,
    driver: DriverState,
    open_sample: Option<OpenSample>,
    samples: Vec<LatencySample>,
    checkpoints: Vec<CheckpointRecord>,
    checkpoint_wait: Option<CheckpointWait>,
    grid: Option<(u16, u16)>,
    monitor: Option<MonitorInfo>,
    throughput: Option<Throughput>,
    uncover_ms: Option<f64>,
    scrollback_rows_retained: Option<u64>,
    native_focus_dropped: u64,
    synthetic_occlusion: bool,
    first_present_bound: FirstPresentBound,
    /// How the main window presents, recorded at the end of startup on Windows.
    presenter: Option<PresenterRecord>,
    /// The configured software render mode, as the config file spells it.
    software_render_mode: &'static str,
    outcome: Option<(Status, Option<String>)>,
}

impl ApplicationHandler<UserEvent> for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.stage != Stage::Startup || self.main_id.is_some() {
            // When: the main window already exists, a later resume must not build a second one.
            return;
        }
        self.forward(event_loop, Dispatch::Native, |app, active| app.resumed(active));
        let Some(window) = self.app.main_window().cloned() else {
            self.invalidate(event_loop, "the App opened no main window".to_owned());
            return;
        };
        // The floating level keeps this inactive app's window above other apps' normal windows
        // without activating it, so it renders while the user's application keeps focus.
        window.set_window_level(WindowLevel::AlwaysOnTop);
        self.main_id = Some(window.id());
        // The first-present bound counts from here, so App construction is outside it.
        self.first_present_bound.window_opened(Instant::now());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.stage == Stage::Done
            || self.cover.as_ref().is_some_and(|cover| cover.id() == window_id)
        {
            // When: the run is over, or the event is for the harness's own cover, the App must not see it.
            return;
        }
        let is_main = self.main_id == Some(window_id);
        let arrival = match &event {
            WindowEvent::RedrawRequested => Arrival::Redraw,
            WindowEvent::Focused(_) if is_main => Arrival::Focus,
            WindowEvent::Occluded(occluded) if is_main => Arrival::Occlusion(*occluded),
            other => Arrival::Routed(classify(other, is_main)),
        };
        match arrival {
            Arrival::Redraw => {
                let kind = if is_main { Dispatch::Redraw } else { Dispatch::Native };
                self.forward(event_loop, kind, |app, active| {
                    app.window_event(active, window_id, event)
                });
            }
            Arrival::Focus => {
                // When: native focus arrives, it is recorded and dropped; the App takes focus only
                // from the harness's synthetic events, so the user's application keeps keyboard focus.
                self.native_focus_dropped += 1;
            }
            Arrival::Occlusion(occluded) => self.native_occlusion(event_loop, occluded),
            Arrival::Routed(Route::Forward) => {
                self.forward(event_loop, Dispatch::Native, |app, active| {
                    app.window_event(active, window_id, event)
                });
            }
            Arrival::Routed(Route::Drop) => {}
            Arrival::Routed(Route::Refuse(what)) => {
                self.invalidate(
                    event_loop,
                    format!("unexpected physical input reached the window: {what}"),
                );
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if self.stage == Stage::Done {
            return;
        }
        let unexpected = match &event {
            UserEvent::MenuAction => Some("a menu action"),
            UserEvent::OpenScripts => Some("a script-open request"),
            UserEvent::OsDrag | UserEvent::DragMoved | UserEvent::DragEnded => {
                Some("an OS drag event")
            }
            _ => None,
        };
        if let Some(what) = unexpected {
            // When: no bridge is installed, such an event came from outside the harness and could
            // reach settings paths outside the scratch directory.
            self.invalidate(event_loop, format!("unexpected user event before dispatch: {what}"));
            return;
        }
        self.forward(event_loop, Dispatch::Native, |app, active| app.user_event(active, event));
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if self.stage == Stage::Done {
            return;
        }
        self.forward(event_loop, Dispatch::Harness, |app, active| app.new_events(active, cause));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.stage == Stage::Done {
            return;
        }
        let now = Instant::now();
        if now >= self.run_deadline {
            let reason = format!(
                "the {} s harness deadline expired in {:?}",
                self.plan.timeout_s, self.stage
            );
            self.finish(event_loop, Status::Timeout, Some(reason));
            return;
        }
        if self.stage == Stage::Startup && self.first_present_bound.expired(now, self.first_present)
        {
            // When: no frame presented within the bound, the run cannot be exercised; it ends now
            // as a suspected occlusion, which the comparison retries, not at the run's deadline.
            let redraws = self.meter.as_ref().map_or(0, |meter| meter.redraw_requested);
            let reason = waits::first_present_missing_reason(
                FIRST_PRESENT_WAIT,
                redraws,
                self.occlusion.delivered,
            );
            self.invalidate(event_loop, reason);
            return;
        }
        self.deliver_overdue_occlusion(event_loop, now);
        if matches!(self.stage, Stage::Steps(_)) {
            self.maybe_scan(Instant::now(), ScanTrigger::Harness);
        }
        self.advance(event_loop);
        if self.stage == Stage::Done {
            return;
        }
        self.forward(event_loop, Dispatch::Harness, |app, active| app.about_to_wait(active));
        if self.stage != Stage::Done && event_loop.exiting() {
            // When: the App asked to exit, for example after a last-window close, the run cannot complete.
            self.invalidate(event_loop, "the App requested exit".to_owned());
            return;
        }
        if self.stage == Stage::Startup && self.first_present {
            // The end of the first about_to_wait after a present is the warm-pool barrier.
            self.end_startup(event_loop);
        }
        let flow =
            merge_control_flow(event_loop.control_flow(), self.next_deadline(Instant::now()));
        event_loop.set_control_flow(flow);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }
}

/// How an ordinary native window event is routed. Physical input could reach settings paths
/// outside the scratch directory, so it voids the run before the App sees it.
fn classify(event: &WindowEvent, is_main: bool) -> Route {
    match event {
        WindowEvent::KeyboardInput { .. } => Route::Refuse("keyboard input"),
        // Nothing held means no input; anything held is a physical key.
        WindowEvent::ModifiersChanged(modifiers) if modifiers.state().is_empty() => Route::Drop,
        WindowEvent::ModifiersChanged(_) => Route::Refuse("a modifier key"),
        // IME enable and disable follow the App's own `set_ime_allowed`, not the user.
        WindowEvent::Ime(Ime::Enabled | Ime::Disabled) => Route::Forward,
        // An empty preedit is an IME reset, not typed text.
        WindowEvent::Ime(Ime::Preedit(text, _)) if text.is_empty() => Route::Drop,
        WindowEvent::Ime(_) => Route::Refuse("IME text"),
        WindowEvent::CursorMoved { .. } => Route::Refuse("pointer motion"),
        // Crossing the window edge carries no position or button the App needs.
        WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => Route::Drop,
        WindowEvent::MouseWheel { .. } => Route::Refuse("a wheel scroll"),
        WindowEvent::MouseInput { .. } => Route::Refuse("a mouse button"),
        WindowEvent::Touch(_)
        | WindowEvent::TouchpadPressure { .. }
        | WindowEvent::AxisMotion { .. }
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::DoubleTapGesture { .. }
        | WindowEvent::RotationGesture { .. } => Route::Refuse("a touch or gesture"),
        WindowEvent::DroppedFile(_)
        | WindowEvent::HoveredFile(_)
        | WindowEvent::HoveredFileCancelled => Route::Refuse("a file drag"),
        WindowEvent::CloseRequested => Route::Refuse("a close request"),
        WindowEvent::Destroyed if is_main => Route::Refuse("the measurement window's destruction"),
        _ => Route::Forward,
    }
}

/// The App's `ControlFlow`, shortened to the probe's next wake so neither starves the other.
fn merge_control_flow(app_flow: ControlFlow, probe: Option<Instant>) -> ControlFlow {
    match (app_flow, probe) {
        (ControlFlow::Poll, _) => ControlFlow::Poll,
        (flow, None) => flow,
        (ControlFlow::Wait, Some(at)) => ControlFlow::WaitUntil(at),
        (ControlFlow::WaitUntil(app_at), Some(at)) => ControlFlow::WaitUntil(app_at.min(at)),
    }
}

/// `struct timeval` from macOS `<sys/_types/_timeval.h>`; `tv_usec` is a 32-bit `suseconds_t`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: i64,
    tv_usec: i32,
}

/// `struct rusage` from macOS `<sys/resource.h>`: two `timeval`s, then fourteen `long` counters.
#[cfg(target_os = "macos")]
#[repr(C)]
struct ResourceUsage {
    ru_utime: TimeVal,
    ru_stime: TimeVal,
    /// `ru_maxrss` through `ru_nivcsw`, which the probe does not read.
    _counters: [i64; 14],
}

#[cfg(target_os = "macos")]
extern "C" {
    fn getrusage(who: i32, usage: *mut ResourceUsage) -> i32;
}

/// `RUSAGE_SELF` from `<sys/resource.h>`.
#[cfg(target_os = "macos")]
const RUSAGE_SELF: i32 = 0;

/// Process user and kernel CPU so far, in seconds; zeros when `GetProcessTimes` fails.
#[cfg(windows)]
fn cpu_times() -> (f64, f64) {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let result =
        // SAFETY: the pseudo-handle needs no closing; every output points to writable FILETIME storage.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if result.is_err() {
        // When: the call failed, zeros mark the figure as missing, as on macOS.
        return (0.0, 0.0);
    }
    let seconds =
        |time: FILETIME| crate::record::filetime_seconds(time.dwLowDateTime, time.dwHighDateTime);
    (seconds(user), seconds(kernel))
}

/// Process user and system CPU so far, in seconds; zeros when `getrusage` fails.
#[cfg(target_os = "macos")]
fn cpu_times() -> (f64, f64) {
    let zero = TimeVal { tv_sec: 0, tv_usec: 0 };
    let mut usage = ResourceUsage { ru_utime: zero, ru_stime: zero, _counters: [0; 14] };
    let result =
        // SAFETY: `usage` is a live, writable `struct rusage` with macOS's layout, as `getrusage` requires.
        unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    if result != 0 {
        return (0.0, 0.0);
    }
    let seconds = |time: TimeVal| time.tv_sec as f64 + f64::from(time.tv_usec) / 1e6;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}

/// Seconds since the Unix epoch; 0.0 if the clock is before it.
fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Milliseconds from `from` to `to`; zero when `to` is earlier.
fn ms_between(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1_000.0
}

/// A seed that differs between runs, for the sentinel nonce.
fn nonce_seed() -> u64 {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}

/// Write `bytes` through a temporary sibling and a rename, so a reader never sees part of a file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, |file| std::io::Write::write_all(file, bytes))
}

/// Fill a temporary sibling of `path` through `fill`, then rename it over `path`.
fn write_atomic_with(
    path: &std::path::Path,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = std::fs::File::create(&temporary)?;
    fill(&mut file)?;
    drop(file);
    std::fs::rename(&temporary, path)
}

/// Aborts the process when the event loop has not returned shortly after the run's deadline.
struct Watchdog {
    cancel: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn arm(deadline: Instant) -> Self {
        let (cancel, receiver) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(Instant::now()) + WATCHDOG_GRACE;
            if matches!(receiver.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Timeout)) {
                eprintln!(
                    "perf_scenarios: the event loop did not return after the deadline; aborting"
                );
                std::process::abort();
            }
        });
        Self { cancel: Some(cancel), thread: Some(thread) }
    }
}

// Lifecycle: Watchdog disarms by dropping its sender, then joins its thread, so no abort outlives the run.
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.cancel = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Measure one scenario in a new scratch directory; returns the process exit code.
pub(crate) fn run(request: &RunArgs, allocation_counter: Option<fn() -> u64>) -> u8 {
    let started = Instant::now();
    let Some(plan) = scenarios::plan(request.scenario, request.variant, request.short) else {
        eprintln!(
            "perf_scenarios: refused: {} has no variant {}",
            request.scenario, request.variant
        );
        return REFUSED;
    };
    let scratch = PathBuf::from(&request.scratch);
    // The new scratch directory and harness.pid come first, before any other file or any window.
    if let Err(error) = std::fs::create_dir(&scratch) {
        eprintln!("perf_scenarios: refused: cannot create {}: {error}", request.scratch);
        return REFUSED;
    }
    if let Err(error) =
        std::fs::write(scratch.join("harness.pid"), format!("{}\n", std::process::id()))
    {
        eprintln!("perf_scenarios: refused: cannot write harness.pid: {error}");
        return REFUSED;
    }
    // Every pane's program inherits the scratch directory through this variable. No other thread
    // exists yet: logging starts in `prepare_scratch` and the watchdog after it.
    #[cfg(windows)]
    std::env::set_var(workload::SCRATCH_ENV, &scratch);
    let prepared = match prepare_scratch(&plan, request, &scratch) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("perf_scenarios: refused: {error}");
            return REFUSED;
        }
    };
    let run_deadline = started + Duration::from_secs(plan.timeout_s);
    let watchdog = Watchdog::arm(run_deadline);
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        // Launch must not activate the app, so the user's front application keeps focus.
        builder.with_activate_ignoring_other_apps(false);
    }
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("perf_scenarios: refused: cannot create the event loop: {error}");
            return REFUSED;
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let roles = plan.roles.len();
    let sentinels = (0..roles).map(|role| workload::sentinel_line(role, &prepared.nonce)).collect();
    // Startup is measured from just before the App is built.
    let meter = PhaseMeter::start("startup", allocation_counter.is_some());
    // Read before the App takes the config, so the presenter record can name the configured mode.
    let software_render_mode = render_mode_text(prepared.config.appearance.software_render_mode);
    let app =
        App::new_with_proxy(Theme::default(), prepared.config, Keymap::default(), Some(proxy));
    let mut probe = Probe {
        app,
        plan: Arc::new(plan),
        request: request.clone(),
        scratch,
        sentinels,
        allocation_counter,
        run_deadline,
        stage: Stage::Startup,
        main_id: None,
        cover: None,
        occlusion: OcclusionState::default(),
        frames: 0,
        first_present: false,
        meter: Some(meter),
        phases: Vec::new(),
        role_panes: Vec::new(),
        go_at: None,
        sentinel_roles: Vec::new(),
        sentinel_seen: vec![None; roles],
        image: ImageState::default(),
        scan: ScanThrottle::new(SCAN_INTERVAL),
        driver: DriverState::None,
        open_sample: None,
        samples: Vec::new(),
        checkpoints: Vec::new(),
        checkpoint_wait: None,
        grid: None,
        monitor: None,
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        native_focus_dropped: 0,
        synthetic_occlusion: false,
        first_present_bound: FirstPresentBound::default(),
        presenter: None,
        software_render_mode,
        outcome: None,
    };
    if let Err(error) = event_loop.run_app(&mut probe) {
        probe
            .outcome
            .get_or_insert((Status::Invalid, Some(format!("the event loop failed: {error}"))));
    }
    // Every exit path after the App exists tears its sessions down; the group kill ends the anchors.
    let settled = probe.app.finish_session();
    let result = probe.result(settled);
    let status = result.status;
    let document = result.to_json();
    let text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string());
    if let Err(error) = write_atomic(&probe.scratch.join("result.json"), text.as_bytes()) {
        eprintln!("perf_scenarios: cannot write result.json: {error}");
    }
    tracing::info!(target: LOG_TARGET, status = status.as_str(), settled, "perf_scenarios result written");
    println!(
        "perf_scenarios {} {}: {} (exit {}){}",
        request.scenario,
        request.variant,
        status.as_str(),
        status.exit_code(),
        result.invalid_reason.map(|reason| format!(": {reason}")).unwrap_or_default()
    );
    drop(probe);
    drop(watchdog);
    drop(prepared.logging);
    status.exit_code()
}

/// The configured `[appearance].software_render_mode`, as the config file spells it.
fn render_mode_text(mode: SoftwareRenderMode) -> &'static str {
    match mode {
        SoftwareRenderMode::Auto => "auto",
        SoftwareRenderMode::Force => "force",
        SoftwareRenderMode::Off => "off",
    }
}

/// What scratch preparation hands the run.
struct Prepared {
    config: Config,
    logging: sonicterm_logging::LoggingGuard,
    nonce: String,
}

/// Lay out the scratch directory, load its config, start logging there and write the workload.
fn prepare_scratch(plan: &Plan, request: &RunArgs, scratch: &Path) -> Result<Prepared, String> {
    for dir in [
        "config",
        "logs",
        "workload/fixtures",
        "roles",
        "sessions",
        "acks",
        "go",
        "done",
        "checkpoints",
    ] {
        std::fs::create_dir_all(scratch.join(dir))
            .map_err(|error| format!("create {dir}: {error}"))?;
    }
    let config_path = scratch.join("config/sonicterm.toml");
    std::fs::write(&config_path, scratch_config_text(plan, request)?)
        .map_err(|error| format!("write the config: {error}"))?;
    let config =
        Config::load_strict(&config_path).map_err(|error| format!("load the config: {error:#}"))?;
    let logging = sonicterm_logging::init_in(&config.logging, &scratch.join("logs"))
        .map_err(|error| format!("start logging: {error}"))?;
    // The path exactly as given, so a search of other logs for it proves nothing was written there.
    let (scenario, variant, given) = (plan.scenario, plan.variant, &request.scratch);
    tracing::info!(target: LOG_TARGET, "perf_scenarios run: scenario={scenario} variant={variant} scratch={given}");
    let fixtures = workload::fixtures(plan);
    let nonce = workload::choose_nonce(nonce_seed(), &fixtures);
    let fixture_root = scratch.join("workload/fixtures");
    let mut fixture_bytes = 0_usize;
    for fixture in &fixtures {
        fixture
            .write_under(&fixture_root)
            .map_err(|error| format!("write fixture {}: {error}", fixture.relative_path))?;
        fixture_bytes += fixture.byte_len();
    }
    let fixture_files = fixtures.len();
    tracing::info!(target: LOG_TARGET, fixture_files, fixture_bytes, "perf_scenarios fixtures written");
    write_role_program(plan, request, scratch, &nonce)?;
    Ok(Prepared { config, logging, nonce })
}

/// The scratch config on macOS: the generated role script is every pane's shell.
#[cfg(unix)]
fn scratch_config_text(plan: &Plan, request: &RunArgs) -> Result<String, String> {
    Ok(workload::config_toml(plan, &request.scratch, request.laps))
}

/// The scratch config on Windows: this harness binary is every pane's shell, in program mode.
///
/// A path that would break the TOML literal string is refused before any window opens.
#[cfg(windows)]
fn scratch_config_text(plan: &Plan, request: &RunArgs) -> Result<String, String> {
    let harness = std::env::current_exe()
        .map_err(|error| format!("cannot find the harness executable: {error}"))?;
    let harness = harness
        .to_str()
        .ok_or_else(|| format!("harness path {} is not UTF-8", harness.display()))?
        .to_owned();
    crate::cli::check_harness_shell(&harness)?;
    Ok(workload::config_toml_with_shell(plan, &harness, request.laps))
}

/// Write `workload/role.sh`, the shell every pane runs on macOS, and make it executable.
#[cfg(unix)]
fn write_role_program(
    plan: &Plan,
    request: &RunArgs,
    scratch: &Path,
    nonce: &str,
) -> Result<(), String> {
    let script_path = scratch.join("workload/role.sh");
    std::fs::write(&script_path, workload::role_script(plan, &request.scratch, nonce))
        .map_err(|error| format!("write role.sh: {error}"))?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("make role.sh executable: {error}"))
}

/// Write `workload/program.json`, from which each pane's program rebuilds the plan and nonce.
#[cfg(windows)]
fn write_role_program(
    plan: &Plan,
    _request: &RunArgs,
    scratch: &Path,
    nonce: &str,
) -> Result<(), String> {
    let spec = workload::program_json(plan, nonce);
    write_atomic(&scratch.join("workload/program.json"), spec.as_bytes())
        .map_err(|error| format!("write program.json: {error}"))
}

impl PhaseMeter {
    fn start(name: &'static str, counting: bool) -> Self {
        Self {
            name,
            started: Instant::now(),
            start_unix_s: unix_now(),
            cpu_start: cpu_times(),
            presented_frames: 0,
            redraw_requested: 0,
            dispatch_ms: Vec::new(),
            present_interval_ms: Vec::new(),
            allocations: counting.then(Vec::new),
            last_present: None,
        }
    }

    fn finish(self) -> PhaseRecord {
        let (user_s, system_s) = cpu_times();
        PhaseRecord {
            name: self.name,
            start_unix_s: self.start_unix_s,
            end_unix_s: unix_now(),
            cpu_user_s: user_s - self.cpu_start.0,
            cpu_system_s: system_s - self.cpu_start.1,
            presented_frames: self.presented_frames,
            redraw_requested: self.redraw_requested,
            dispatch_ms: self.dispatch_ms,
            present_interval_ms: self.present_interval_ms,
            allocations_per_frame: self.allocations,
        }
    }
}

/// One step of a drag gesture.
enum DragMotion {
    Press((f64, f64)),
    Move((f64, f64)),
    Release,
}

impl Typing {
    /// The next injection, or the end of the settle once every character is typed.
    fn next_due(&self) -> Option<Instant> {
        let first_at = self.first_at?;
        if self.typed < self.chars {
            Some(first_at + self.interval * self.typed)
        } else {
            self.done_at.map(|at| at + self.settle)
        }
    }
}

impl Sweep {
    /// A triangle wave across the window every 2 s, moving to the next lane every quarter second.
    fn position(&self) -> (f64, f64) {
        let period = u64::from(self.hertz.max(1)) * 2;
        let half = period / 2;
        let tick = u64::from(self.ticks);
        let along = tick % period;
        let rising = if along < half { along } else { period - along };
        let fraction = rising as f64 / half as f64;
        let lane = (tick / (u64::from(self.hertz) / 4).max(1)) as usize % self.lanes.len();
        (self.left + (self.right - self.left) * fraction, self.lanes[lane])
    }
}

fn lerp(from: (f64, f64), to: (f64, f64), fraction: f64) -> (f64, f64) {
    (from.0 + (to.0 - from.0) * fraction, from.1 + (to.1 - from.1) * fraction)
}

impl Probe {
    /// Forward one dispatch to the App and account for it: frames, `RedrawRequested` time and
    /// allocations, present intervals, the uncover time and, while a sample is open, attribution.
    fn forward(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: Dispatch,
        dispatch: impl FnOnce(&mut App, &ActiveEventLoop),
    ) {
        let frames_before = self.frame_count();
        // The snapshot is released before the dispatch; whether it advanced is known only after.
        let before = self.open_sample.is_some().then(|| self.echo_snapshot());
        let allocations_before = match kind {
            Dispatch::Redraw => self.allocation_counter.map(|counter| counter()),
            Dispatch::Native | Dispatch::Harness => None,
        };
        let started = Instant::now();
        dispatch(&mut self.app, event_loop);
        let allocations = allocations_before
            .zip(self.allocation_counter)
            .map(|(before_count, counter)| counter().saturating_sub(before_count));
        let ended = Instant::now();
        let frames_after = self.frame_count();
        let advanced = frames_after > frames_before;
        self.frames = frames_after;
        if advanced {
            self.first_present = true;
            if let Some(from) = self.occlusion.uncover_from.take() {
                self.uncover_ms = Some(ms_between(from, ended));
            }
            self.image.present.observe_frames(frames_after);
        }
        if let Some(meter) = self.meter.as_mut() {
            if advanced {
                meter.presented_frames += frames_after - frames_before;
                if let Some(last) = meter.last_present {
                    meter.present_interval_ms.push(ms_between(last, ended));
                }
                meter.last_present = Some(ended);
            }
            if kind == Dispatch::Redraw {
                meter.redraw_requested += 1;
                meter.dispatch_ms.push(ms_between(started, ended));
                if let (Some(counts), Some(count)) = (meter.allocations.as_mut(), allocations) {
                    counts.push(count);
                }
            }
        }
        if let Some(before) = before {
            if self.open_sample.is_some() {
                let after = self.echo_snapshot();
                self.attribute(&DispatchObservation { before, after, advanced }, ended);
            }
        }
        if matches!(self.stage, Stage::Steps(_)) {
            self.maybe_scan(ended, kind.scan_trigger());
        }
    }

    /// Settle the open sample from one observed dispatch that ended at `ended`.
    fn attribute(&mut self, observation: &DispatchObservation, ended: Instant) {
        match attribute_dispatch(observation) {
            Attribution::Pending => {}
            Attribution::Credited => {
                if let Some(sample) = self.open_sample.take() {
                    self.samples.push(LatencySample {
                        inject_unix_s: sample.inject_unix_s,
                        latency_ms: Some(ms_between(sample.injected, ended)),
                        reason: CREDITED,
                    });
                }
            }
            Attribution::Unattributed(reason) => self.close_sample(reason),
        }
    }

    /// Close the open sample, if any, as unattributed.
    fn close_sample(&mut self, reason: UnattributedReason) {
        if let Some(sample) = self.open_sample.take() {
            self.samples.push(LatencySample {
                inject_unix_s: sample.inject_unix_s,
                latency_ms: None,
                reason: reason.as_str(),
            });
        }
    }

    /// A nonblocking look at the open sample's echo cell.
    fn echo_snapshot(&self) -> EchoSnapshot {
        let Some(sample) = self.open_sample.as_ref() else {
            return EchoSnapshot::Busy;
        };
        let Some(pane) = self.app.main_panes().and_then(|panes| panes.get(&sample.pane)) else {
            return EchoSnapshot::Busy;
        };
        match pane.parser.try_lock() {
            Some(parser) => snapshot_echo(parser.grid(), &sample.target),
            None => EchoSnapshot::Busy,
        }
    }

    /// Fully redraw the measurement window once, through the App's own output path, after the
    /// scan first sees S11's image registered. The renderer skips a frame whose identity is
    /// unchanged, and a skip presents nothing, so the retained identity is cleared first: the
    /// frame then presents whenever the window can present at all.
    fn request_image_redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window_id) = self.main_id else {
            return;
        };
        if let Some(renderer) = self.app.main_renderer_mut() {
            renderer.invalidate_retained_frame();
        }
        self.forward(event_loop, Dispatch::Harness, |app, active| {
            app.user_event(active, UserEvent::RequestRedraw(window_id))
        });
    }

    /// Write `progress.json` with every measurement completed so far; called only between
    /// phases. A failed write is logged and leaves the measurements untouched.
    fn record_progress(&self) {
        let path = self.scratch.join("progress.json");
        let hash = self.request.harness_hash.as_deref();
        let written = write_atomic_with(&path, |file| {
            let mut writer = std::io::BufWriter::new(file);
            write_progress(&mut writer, hash, self.measurements())?;
            std::io::Write::flush(&mut writer)
        });
        if let Err(error) = written {
            tracing::warn!(target: LOG_TARGET, %error, "cannot write progress.json");
        }
    }

    /// The main renderer's presented frames so far.
    fn frame_count(&self) -> u64 {
        self.app.main_renderer().map_or(0, GpuRenderer::successful_frame_count)
    }

    /// Whether a sentinel, an image or the typing prompt is still awaited.
    fn scan_wanted(&self) -> bool {
        self.sentinel_roles.iter().any(|role| self.sentinel_seen[*role].is_none())
            || (self.image.role.is_some() && !self.image.present.seen())
            || matches!(&self.driver, DriverState::Typing(typing) if typing.origin.is_none())
    }

    /// Scan in dispatches the App already receives, at most every `SCAN_INTERVAL`. A throttled
    /// scan in a dispatch from outside the harness leaves one trailing check `SCAN_INTERVAL` after
    /// the last scan; the harness's own wakes, that check's included, never arm another.
    fn maybe_scan(&mut self, now: Instant, trigger: ScanTrigger) {
        let wanted = self.scan_wanted();
        if self.scan.observe(now, wanted, trigger) {
            self.run_scan(now);
        }
    }

    /// Look, without blocking, for each awaited sentinel, the awaited image and the typing prompt.
    fn run_scan(&mut self, now: Instant) {
        for index in 0..self.sentinel_roles.len() {
            let role = self.sentinel_roles[index];
            if self.sentinel_seen[role].is_some() {
                continue;
            }
            let sentinel = &self.sentinels[role];
            if self.role_grid(role, |grid| line_near_cursor(grid, sentinel, PROTOCOL_ROWS))
                == Some(true)
            {
                self.sentinel_seen[role] = Some(now);
            }
        }
        if let (Some(role), false) = (self.image.role, self.image.present.seen()) {
            let registered = self
                .role_pane(role)
                .and_then(|pane| pane.inline_images.try_lock().map(|images| !images.is_empty()));
            if registered == Some(true) {
                self.image.present.saw_registration(now, self.frames);
            }
        }
        let prompt_role = match &self.driver {
            DriverState::Typing(typing) if typing.origin.is_none() => Some(typing.role),
            _ => None,
        };
        if let Some(role) = prompt_role {
            let found = self
                .role_grid(role, |grid| {
                    prompt_origin(grid, workload::PROMPT).map(|origin| (origin, grid.cols))
                })
                .flatten();
            if let (Some((origin, cols)), DriverState::Typing(typing)) = (found, &mut self.driver) {
                typing.origin = Some(origin);
                typing.cols = cols;
                typing.first_at = Some(now);
            }
        }
    }

    /// Read `role`'s grid without blocking; `None` when the pane is gone or its parser is busy.
    fn role_grid<Output>(&self, role: usize, read: impl FnOnce(&Grid) -> Output) -> Option<Output> {
        let pane = self.role_pane(role)?;
        let parser = pane.parser.try_lock()?;
        Some(read(parser.grid()))
    }

    fn role_pane(&self, role: usize) -> Option<&PaneState> {
        let pane_id = *self.role_panes.get(role)?;
        self.app.main_panes()?.get(&pane_id)
    }

    /// The active tab's active pane in the measurement window.
    fn active_pane(&self) -> Option<u64> {
        let main = self.app.main()?;
        main.tab_states.get(main.tabs.active_index()).map(|tab| tab.active_pane)
    }

    /// Every measurement completed so far, as both `progress.json` and `result.json` record it;
    /// latency samples count only in a scenario that types.
    fn measurements(&self) -> Measurements<'_> {
        Measurements {
            phases: &self.phases,
            latency: self.has_typing().then_some(self.samples.as_slice()),
            throughput: self.throughput,
            uncover_ms: self.uncover_ms,
            scrollback_rows_retained: self.scrollback_rows_retained,
            checkpoints: &self.checkpoints,
        }
    }

    fn has_typing(&self) -> bool {
        self.plan.steps.iter().any(
            |step| matches!(step, Step::Phase(phase) if matches!(phase.driver, Driver::Typing { .. })),
        )
    }

    /// The run's result; a run that ended early keeps its partial phases and samples.
    fn result(&mut self, settled: bool) -> RunResult {
        self.close_sample(UnattributedReason::NoCandidate);
        if let Some(meter) = self.meter.take() {
            self.phases.push(meter.finish());
        }
        let (status, invalid_reason) = self.outcome.take().unwrap_or_else(|| {
            (Status::Invalid, Some("the event loop ended before the run finished".to_owned()))
        });
        // The same measurements progress.json records, copied rather than taken.
        let measured = self.measurements();
        RunResult {
            harness_hash: self.request.harness_hash.clone(),
            scenario: self.plan.scenario,
            variant: self.plan.variant,
            managed: self.request.managed,
            short: self.request.short,
            laps: self.request.laps,
            alloc_counting: self.allocation_counter.is_some(),
            status,
            invalid_reason,
            harness_pid: std::process::id(),
            grid: self.grid,
            monitor: self.monitor.clone(),
            window_path: "production",
            synthetic_occlusion: self.synthetic_occlusion,
            native_focus_events_dropped: self.native_focus_dropped,
            finish_session_settled: settled,
            presenter: self.presenter.clone(),
            phases: measured.phases.to_vec(),
            latency: measured.latency.map(|samples| samples.to_vec()),
            throughput: measured.throughput,
            uncover_ms: measured.uncover_ms,
            scrollback_rows_retained: measured.scrollback_rows_retained,
            checkpoints: measured.checkpoints.to_vec(),
            notes: self.notes(),
        }
    }

    /// Conditions a reader needs to interpret this run's numbers.
    fn notes(&self) -> Vec<String> {
        let mut notes = vec![
            "The example runs the same App, renderer, PTY and VT as the shipping binary; platform main() setup (menus, crash markers, breadcrumb recording) is outside the measurement on both sides.".to_owned(),
            "A measurement ends when the dispatch returns, not at scanout.".to_owned(),
            "Hypothesis: the first about_to_wait after the first present fills the default warm pool of one; the pool is not observable from outside the crate.".to_owned(),
            "The window takes focus only from a synthetic Focused(true) before setup; native Focused events are recorded and dropped.".to_owned(),
            "Completion is proven only by the sentinel in the parsed grid; done/<role> files are never read.".to_owned(),
        ];
        if self.has_typing() {
            notes.push("Typing is a labelled proxy: one Ime::Commit per character, which skips keymap lookup and key encoding.".to_owned());
        }
        if !self.request.managed {
            notes.push("Unmanaged: the probe acknowledged its own session records, so this run never enters a comparison.".to_owned());
        }
        if self.allocation_counter.is_some() {
            notes.push("allocations_per_frame counts alloc, alloc_zeroed and realloc calls in the whole process, every thread, during each RedrawRequested dispatch.".to_owned());
        }
        if self.request.laps {
            notes.push("Laps run: logging at debug adds a render_timing line per frame, so it is never pooled with timed runs.".to_owned());
        }
        if self.plan.roles.iter().any(|role| matches!(role, Workload::Frames { .. })) {
            notes.push("Frames are paced by sleep 0.016 between writes, so slightly fewer than 60 arrive each second.".to_owned());
        }
        if self.throughput.is_some() {
            notes.push("Throughput counts workload output bytes before the terminal turns LF into CR LF, from GO to the sentinel seen in the grid.".to_owned());
        }
        if self.plan.scrollback_rows > 1_000 {
            notes.push(format!(
                "Scrollback asks for {} rows; the per-pane cell budget clamps it, and scrollback_rows_retained records what was kept.",
                self.plan.scrollback_rows
            ));
        }
        if self.synthetic_occlusion {
            notes.push(
                "No native occlusion change arrived within 2 s, so a synthetic one was delivered."
                    .to_owned(),
            );
        }
        notes
    }
}

impl Probe {
    /// End startup at the warm-pool barrier, record the grid and the display, then focus.
    fn end_startup(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(meter) = self.meter.take() {
            self.phases.push(meter.finish());
        }
        self.record_progress();
        let Some(pane) = self.active_pane() else {
            self.invalidate(event_loop, "no active pane after startup".to_owned());
            return;
        };
        self.grid = self.app.__test_pane_grid_size(pane);
        self.monitor = self.monitor_info();
        // Only Windows records how it presented, so macOS results, and their tables, stay as they were.
        if cfg!(windows) {
            self.presenter = self.presenter_record();
        }
        let blocked = self.presenter.as_ref().and_then(|presenter| {
            presenter_blocked(self.plan.presentation, presenter, scenarios::BUILD_HOST)
        });
        if let Some(reason) = blocked {
            // When: the window did not present through its variant's presenter, the run measures nothing it names.
            self.finish(event_loop, Status::Blocked, Some(reason));
            return;
        }
        self.role_panes.push(pane);
        tracing::info!(target: LOG_TARGET, grid = ?self.grid, "perf_scenarios startup ended at the warm-pool barrier");
        if self.plan.focused {
            self.focus(event_loop, true);
        }
        self.stage = Stage::Setup { next_action: 0 };
    }

    /// The measurement window's display; `None` when winit reports no monitor.
    fn monitor_info(&self) -> Option<MonitorInfo> {
        let window = self.app.main_window()?;
        let monitor = window.current_monitor()?;
        Some(MonitorInfo {
            name: monitor.name(),
            refresh_rate_millihertz: monitor.refresh_rate_millihertz(),
            scale_factor: window.scale_factor(),
        })
    }

    /// How the main window presents: the configured mode and the renderer's software flags.
    fn presenter_record(&self) -> Option<PresenterRecord> {
        let renderer = self.app.main_renderer()?;
        let degraded = renderer.is_software_render_degraded();
        Some(PresenterRecord {
            software_render_mode: self.software_render_mode,
            software_rendering: renderer.is_software_rendering(),
            software_render_degraded: degraded,
            // The degrade path presents through GDI on Windows; elsewhere it stays on wgpu.
            windows_gdi: cfg!(windows) && degraded,
        })
    }

    /// Move through every stage that can progress now.
    fn advance(&mut self, event_loop: &ActiveEventLoop) {
        loop {
            let stage = self.stage;
            match stage {
                Stage::Startup | Stage::Done => return,
                Stage::Setup { next_action } => self.advance_setup(event_loop, next_action),
                Stage::Acks => self.advance_acks(event_loop),
                Stage::Ready => self.advance_ready(event_loop),
                Stage::Steps(index) => self.advance_steps(event_loop, index),
            }
            if self.stage == stage {
                return;
            }
        }
    }

    /// Open roles one at a time; each waits for the previous role's session record, so roles
    /// number in the order their panes open.
    fn advance_setup(&mut self, event_loop: &ActiveEventLoop, next_action: usize) {
        let newest = self.role_panes.len().saturating_sub(1);
        if !self.scratch.join(format!("sessions/{newest}.json")).exists() {
            // When: the newest pane's script has not recorded its session, the next pane waits.
            return;
        }
        let plan = Arc::clone(&self.plan);
        let Some(setup) = plan.setup.get(next_action).copied() else {
            if self.role_panes.len() == plan.roles.len() {
                self.stage = Stage::Acks;
            } else {
                let opened = self.role_panes.len();
                self.invalidate(
                    event_loop,
                    format!("setup opened {opened} roles for {} planned", plan.roles.len()),
                );
            }
            return;
        };
        let before = self.active_pane();
        let (action, opens_role) = match setup {
            SetupAction::NewTab => (Action::NewTab, true),
            SetupAction::SplitRight => (Action::SplitRight, true),
            SetupAction::SplitDown => (Action::SplitDown, true),
            SetupAction::ActivateTab(index) => (Action::ActivateTab(index), false),
        };
        if !self.run_action(event_loop, &action) {
            self.invalidate(event_loop, format!("the App refused setup action {setup:?}"));
            return;
        }
        if opens_role {
            match self.active_pane() {
                Some(pane) if Some(pane) != before && !self.role_panes.contains(&pane) => {
                    self.role_panes.push(pane);
                }
                _ => {
                    self.invalidate(
                        event_loop,
                        format!("setup action {setup:?} opened no new pane"),
                    );
                    return;
                }
            }
        }
        self.stage = Stage::Setup { next_action: next_action + 1 };
    }

    /// Run an App action as a forwarded dispatch; whether the App accepted it.
    fn run_action(&mut self, event_loop: &ActiveEventLoop, action: &Action) -> bool {
        let mut accepted = false;
        self.forward(event_loop, Dispatch::Harness, |app, _active| {
            accepted = app.run_action(action)
        });
        accepted
    }

    /// Managed runs wait for the comparison script's acknowledgements; unmanaged runs write them.
    fn advance_acks(&mut self, event_loop: &ActiveEventLoop) {
        for role in 0..self.plan.roles.len() {
            let ack = self.scratch.join(format!("acks/{role}"));
            if self.request.managed {
                if !ack.exists() {
                    // When: the comparison script has not validated this role's record, GO waits.
                    return;
                }
            } else if let Err(error) = std::fs::write(&ack, b"") {
                self.invalidate(event_loop, format!("cannot acknowledge role {role}: {error}"));
                return;
            }
        }
        self.stage = Stage::Ready;
    }

    /// GO waits until every role shows READY and no pane outside the plan has claimed a role.
    fn advance_ready(&mut self, event_loop: &ActiveEventLoop) {
        let roles = self.plan.roles.len();
        if self.scratch.join(format!("roles/{roles}")).exists() {
            self.invalidate(event_loop, format!("a pane outside the plan claimed role {roles}"));
            return;
        }
        for role in 0..roles {
            let ready = workload::ready_line(role);
            if self.role_grid(role, |grid| line_near_cursor(grid, &ready, PROTOCOL_ROWS))
                != Some(true)
            {
                return;
            }
        }
        self.stage = Stage::Steps(0);
    }

    /// Run plan steps from `index` until one has to wait.
    fn advance_steps(&mut self, event_loop: &ActiveEventLoop, mut index: usize) {
        let plan = Arc::clone(&self.plan);
        loop {
            let Some(step) = plan.steps.get(index) else {
                self.finish(event_loop, Status::Valid, None);
                return;
            };
            match step {
                Step::Phase(phase) => {
                    if self.meter.is_none() {
                        self.begin_phase(event_loop, phase);
                    }
                    if self.stage == Stage::Done {
                        return;
                    }
                    self.drive(event_loop, Instant::now());
                    if self.stage == Stage::Done {
                        return;
                    }
                    if self.image.present.take_redraw_request() {
                        self.request_image_redraw(event_loop);
                    }
                    if self.image.present.progress(Instant::now()) == ImageProgress::Expired {
                        let reason = format!(
                            "the scan saw the image registered, but no frame presented within {} s after it, so no frame is known to show the image",
                            IMAGE_PRESENT_WAIT.as_secs()
                        );
                        self.invalidate(event_loop, reason);
                        return;
                    }
                    if !self.phase_done(phase, Instant::now()) {
                        self.stage = Stage::Steps(index);
                        return;
                    }
                    self.end_phase(event_loop, phase);
                }
                Step::Act(act) => {
                    self.perform(event_loop, *act);
                    if self.stage == Stage::Done {
                        return;
                    }
                }
                Step::Checkpoint(label) => {
                    if !self.advance_checkpoint(event_loop, label) {
                        if self.stage != Stage::Done {
                            self.stage = Stage::Steps(index);
                        }
                        return;
                    }
                    // The record is complete and its footprint taken: progress.json gets it now,
                    // not when the next phase ends, so a run killed in that phase keeps it.
                    self.record_progress();
                }
            }
            index += 1;
        }
    }

    /// Write the checkpoint request; a managed run then waits up to `CHECKPOINT_WAIT`, without
    /// blocking the event loop, for the comparison script's `.done`. With no `.done` by then the
    /// run ends invalid and the next phase never starts. Returns whether the plan may move on.
    fn advance_checkpoint(&mut self, event_loop: &ActiveEventLoop, label: &'static str) -> bool {
        if let Some(wait) = self.checkpoint_wait.take() {
            let done = wait.done.exists();
            // The footprint file is looked at only once `.done` says it is complete.
            let footprint = done && wait.json.exists();
            match waits::checkpoint_progress(done, footprint, Instant::now(), wait.deadline) {
                CheckpointProgress::Waiting => {
                    self.checkpoint_wait = Some(wait);
                    return false;
                }
                CheckpointProgress::Answered { footprint } => {
                    if footprint {
                        if let Some(checkpoint) = self.checkpoints.last_mut() {
                            checkpoint.footprint_file = Some(wait.relative_json);
                        }
                    }
                    return true;
                }
                CheckpointProgress::Expired => {
                    let reason = waits::checkpoint_expired_reason(&wait.stem, CHECKPOINT_WAIT);
                    self.invalidate(event_loop, reason);
                    return false;
                }
            }
        }
        let index = self.checkpoints.len();
        let stem = format!("{index}-{label}");
        let unix_s = unix_now();
        let request =
            serde_json::json!({"label": label, "unix_s": unix_s, "pid": std::process::id()});
        let path = self.scratch.join(format!("checkpoints/{stem}.request"));
        if let Err(error) = write_atomic(&path, request.to_string().as_bytes()) {
            tracing::warn!(target: LOG_TARGET, %error, label, "cannot write the checkpoint request");
        }
        tracing::info!(target: LOG_TARGET, index, label, "perf_scenarios checkpoint");
        self.checkpoints.push(CheckpointRecord { index, label, unix_s, footprint_file: None });
        if !self.request.managed {
            // When: unmanaged, nothing answers the request, so the footprint stays unavailable.
            return true;
        }
        self.checkpoint_wait = Some(CheckpointWait {
            done: self.scratch.join(format!("checkpoints/{stem}.done")),
            json: self.scratch.join(format!("checkpoints/{stem}.json")),
            relative_json: format!("checkpoints/{stem}.json"),
            deadline: Instant::now() + CHECKPOINT_WAIT,
            stem,
        });
        false
    }
}

impl Probe {
    /// Start a measured phase; the first one writes GO for every role just before it starts.
    fn begin_phase(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) {
        if self.go_at.is_none() {
            for role in 0..self.plan.roles.len() {
                if let Err(error) = std::fs::write(self.scratch.join(format!("go/{role}")), b"") {
                    self.invalidate(
                        event_loop,
                        format!("cannot write GO for role {role}: {error}"),
                    );
                    return;
                }
            }
            self.go_at = Some(Instant::now());
            tracing::info!(target: LOG_TARGET, roles = self.plan.roles.len(), "perf_scenarios GO");
        }
        tracing::info!(target: LOG_TARGET, phase = phase.name, "perf_scenarios phase started");
        self.meter = Some(PhaseMeter::start(phase.name, self.allocation_counter.is_some()));
        self.sentinel_roles = match &phase.end {
            PhaseEnd::Sentinels(roles) => roles.clone(),
            _ => Vec::new(),
        };
        let image_role = match phase.end {
            PhaseEnd::ImageRegistered(role) => Some(role),
            _ => None,
        };
        self.image = ImageState { role: image_role, ..ImageState::default() };
        self.scan = ScanThrottle::new(SCAN_INTERVAL);
        for act in &phase.enter {
            self.perform(event_loop, *act);
            if self.stage == Stage::Done {
                return;
            }
        }
        let driver = self.start_driver(event_loop, phase);
        if self.stage != Stage::Done {
            self.driver = driver;
        }
    }

    /// End a measured phase: release a held button, close the open sample, record the phase.
    fn end_phase(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) {
        if matches!(&self.driver, DriverState::Drag(drag) if drag.pressed) {
            self.button(event_loop, ElementState::Released);
        }
        self.close_sample(UnattributedReason::NoCandidate);
        self.driver = DriverState::None;
        if let Some(meter) = self.meter.take() {
            self.phases.push(meter.finish());
        }
        tracing::info!(target: LOG_TARGET, phase = phase.name, "perf_scenarios phase ended");
        if let (Some(bytes), Some(go_at)) = (phase.throughput_bytes, self.go_at) {
            let seen =
                self.sentinel_roles.iter().filter_map(|role| self.sentinel_seen[*role]).max();
            if let Some(seen) = seen {
                let seconds = seen.saturating_duration_since(go_at).as_secs_f64();
                self.throughput = Some(Throughput { bytes, seconds });
            }
        }
        self.sentinel_roles.clear();
        self.image = ImageState::default();
        self.scan = ScanThrottle::new(SCAN_INTERVAL);
        // Outside every measured window: this phase's meter is finished and the next not started.
        self.record_progress();
    }

    /// Whether `phase` has reached its end.
    fn phase_done(&self, phase: &PhaseSpec, now: Instant) -> bool {
        match &phase.end {
            PhaseEnd::Hold(_) | PhaseEnd::AfterGo(_) => {
                self.phase_deadline(phase).is_some_and(|deadline| now >= deadline)
            }
            PhaseEnd::Sentinels(roles) => {
                roles.iter().all(|role| self.sentinel_seen[*role].is_some())
            }
            // Only a frame known to follow the registration ends it; the phase's step invalidates
            // the run when none presents in time.
            PhaseEnd::ImageRegistered(_) => {
                self.image.present.progress(now) == ImageProgress::Presented
            }
            PhaseEnd::DriverDone => match &self.driver {
                DriverState::Typing(typing) => {
                    typing.typed >= typing.chars
                        && typing.done_at.is_some_and(|at| now >= at + typing.settle)
                }
                // One interval after the last tick, so its frame can present inside the phase.
                DriverState::Wheel(wheel) => {
                    wheel.sent >= wheel.total && now >= wheel.started + wheel.interval * wheel.total
                }
                _ => true,
            },
        }
    }

    /// When `phase` ends by the clock, if it does.
    fn phase_deadline(&self, phase: &PhaseSpec) -> Option<Instant> {
        match &phase.end {
            PhaseEnd::Hold(hold_ms) => {
                self.meter.as_ref().map(|meter| meter.started + Duration::from_millis(*hold_ms))
            }
            PhaseEnd::AfterGo(after_go_ms) => {
                self.go_at.map(|go_at| go_at + Duration::from_millis(*after_go_ms))
            }
            PhaseEnd::ImageRegistered(_) => self.image.present.deadline(),
            PhaseEnd::Sentinels(_) | PhaseEnd::DriverDone => None,
        }
    }

    /// The phase being measured now, if a phase step is running.
    fn current_phase(&self) -> Option<&PhaseSpec> {
        let Stage::Steps(index) = self.stage else {
            return None;
        };
        self.meter.as_ref()?;
        match self.plan.steps.get(index)? {
            Step::Phase(phase) => Some(phase),
            Step::Act(_) | Step::Checkpoint(_) => None,
        }
    }

    /// Perform one synthetic action.
    fn perform(&mut self, event_loop: &ActiveEventLoop, act: Act) {
        match act {
            Act::Unfocus => self.focus(event_loop, false),
            Act::OpenSearch => {
                let accepted = self.run_action(event_loop, &Action::OpenSearch);
                let opened = self.app.main().is_some_and(|main| {
                    main.tab_states
                        .get(main.tabs.active_index())
                        .is_some_and(|tab| tab.search.is_some())
                });
                if !accepted || !opened {
                    // When: no search field opened, no public path enters a query, so the
                    // scenario is blocked rather than measured without its search.
                    let reason =
                        "Action::OpenSearch opened no search field, so no query can be entered";
                    self.finish(event_loop, Status::Blocked, Some(reason.to_owned()));
                }
            }
            Act::Commit(text) => {
                self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(text.to_owned())));
            }
            Act::ActivateTab(index) => {
                if !self.run_action(event_loop, &Action::ActivateTab(index)) {
                    self.invalidate(event_loop, format!("the App refused ActivateTab({index})"));
                }
            }
            Act::Cover => self.cover(event_loop),
            Act::Uncover => self.uncover(),
        }
    }

    /// Deliver a synthetic `Focused` to the measurement window.
    fn focus(&mut self, event_loop: &ActiveEventLoop, focused: bool) {
        self.window_input(event_loop, WindowEvent::Focused(focused));
    }

    /// End the run with `status`; the partial phase and an open sample are kept for the result.
    fn finish(&mut self, event_loop: &ActiveEventLoop, status: Status, reason: Option<String>) {
        if self.outcome.is_some() {
            return;
        }
        match &reason {
            Some(reason) => {
                tracing::warn!(target: LOG_TARGET, status = status.as_str(), %reason, "perf_scenarios run ended early");
            }
            None => tracing::info!(target: LOG_TARGET, "perf_scenarios run completed"),
        }
        self.close_sample(UnattributedReason::NoCandidate);
        self.driver = DriverState::None;
        if let Some(meter) = self.meter.take() {
            self.phases.push(meter.finish());
        }
        self.cover = None;
        self.checkpoint_wait = None;
        self.outcome = Some((status, reason));
        self.stage = Stage::Done;
        event_loop.exit();
    }

    fn invalidate(&mut self, event_loop: &ActiveEventLoop, reason: String) {
        self.finish(event_loop, Status::Invalid, Some(reason));
    }
}

impl Probe {
    /// The driver a phase starts with; `None` when the phase injects nothing.
    fn start_driver(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) -> DriverState {
        match phase.driver {
            Driver::None => DriverState::None,
            Driver::Typing { role, chars, per_second, settle_ms } => {
                let active = self.active_pane();
                let Some(pane) =
                    self.role_panes.get(role).copied().filter(|pane| Some(*pane) == active)
                else {
                    // When: the role's pane is not active, an Ime::Commit would type into another pane.
                    self.invalidate(
                        event_loop,
                        format!("role {role}'s pane is not the active pane for typing"),
                    );
                    return DriverState::None;
                };
                DriverState::Typing(Typing {
                    role,
                    pane,
                    chars,
                    interval: Duration::from_secs(1) / per_second.max(1),
                    settle: Duration::from_millis(settle_ms),
                    origin: None,
                    cols: 0,
                    first_at: None,
                    typed: 0,
                    done_at: None,
                })
            }
            Driver::Sweep { hertz } => {
                let Some((lanes, left, right)) = self.sweep_geometry() else {
                    self.invalidate(event_loop, "no pane layout for the sweep".to_owned());
                    return DriverState::None;
                };
                let interval = Duration::from_secs(1) / hertz.max(1);
                DriverState::Sweep(Sweep {
                    interval,
                    hertz,
                    started: Instant::now(),
                    ticks: 0,
                    lanes,
                    left,
                    right,
                })
            }
            Driver::Drag { hertz } => {
                let Some((start, end)) = self.drag_geometry() else {
                    self.invalidate(event_loop, "no pane layout for the drag".to_owned());
                    return DriverState::None;
                };
                // A 0.6 s cycle: press, about 0.5 s of motion, release, then idle until the next
                // press, so consecutive presses are farther apart than a double click.
                let cycle_ticks = (hertz.max(2) * 3 / 5).max(3);
                DriverState::Drag(Drag {
                    interval: Duration::from_secs(1) / hertz.max(1),
                    started: Instant::now(),
                    ticks: 0,
                    cycle_ticks,
                    move_ticks: (cycle_ticks * 5 / 6).clamp(1, cycle_ticks - 2),
                    start,
                    end,
                    pressed: false,
                    deadline: self.phase_deadline(phase).unwrap_or(self.run_deadline),
                    finished: false,
                })
            }
            Driver::Wheel { hertz, role } => {
                let Some((retained, center)) = self.wheel_setup(role) else {
                    self.invalidate(event_loop, format!("no pane layout for role {role}'s wheel"));
                    return DriverState::None;
                };
                self.scrollback_rows_retained = Some(retained as u64);
                // The wheel scrolls the pane under the pointer.
                self.pointer(event_loop, center);
                // Three lines a tick, so this many ticks reach the top of the retained history.
                let ticks_each_way =
                    u32::try_from(retained.div_ceil(3)).unwrap_or(u32::MAX / 2).max(1);
                DriverState::Wheel(Wheel {
                    interval: Duration::from_secs(1) / hertz.max(1),
                    started: Instant::now(),
                    sent: 0,
                    total: ticks_each_way * 2,
                })
            }
        }
    }

    /// Hover-only sweep lanes: the middle of the tab bar and three grid rows, and the x range.
    fn sweep_geometry(&self) -> Option<([f64; 4], f64, f64)> {
        let pane = self.active_pane()?;
        let renderer = self.app.main_renderer()?;
        let layout = renderer.pane_layout(pane)?;
        let size = self.app.main_window()?.inner_size();
        let (width, height) = (f64::from(size.width), f64::from(size.height));
        // The tab bar is pinned to the bottom. The sweep only hovers there; it never presses.
        let tab_bar = height - f64::from(renderer.tab_bar_logical_height()) / 2.0;
        let (top, span) = (f64::from(layout.origin_y_logical), f64::from(layout.h_logical));
        Some((
            [tab_bar, top + span * 0.2, top + span * 0.5, top + span * 0.8],
            width * 0.1,
            width * 0.9,
        ))
    }

    /// Drag endpoints at cell centers inside the grid.
    fn drag_geometry(&self) -> Option<((f64, f64), (f64, f64))> {
        let pane = self.active_pane()?;
        let layout = self.app.main_renderer()?.pane_layout(pane)?;
        // Startup never activates the app: its only activation call is `focus_window` when a tab
        // drops into another window, which winit implements with `activateIgnoringOtherApps`. So
        // no button is ever pressed on the tab bar; press, motion and release stay inside the grid,
        // away from the tab bar and the window edges.
        let cell = |row: u16, col: u16| {
            (
                f64::from(layout.origin_x_logical)
                    + (f64::from(col) + 0.5) * f64::from(layout.cell_w_logical),
                f64::from(layout.origin_y_logical)
                    + (f64::from(row) + 0.5) * f64::from(layout.cell_h_logical),
            )
        };
        let (rows, cols) = (layout.rows.max(4), layout.cols.max(4));
        Some((cell(rows / 8 + 1, cols / 10 + 1), cell(rows * 6 / 8, cols * 8 / 10)))
    }

    /// The history rows `role`'s pane kept, and its center for the wheel's pointer.
    fn wheel_setup(&self, role: usize) -> Option<(usize, (f64, f64))> {
        let pane_id = *self.role_panes.get(role)?;
        let pane = self.app.main_panes()?.get(&pane_id)?;
        // The print phase saw the sentinel, so the worker is idle and this short lock does not wait.
        let retained = pane.parser.lock().grid().scrollback_len();
        let layout = self.app.main_renderer()?.pane_layout(pane_id)?;
        let center = (
            f64::from(layout.origin_x_logical) + f64::from(layout.w_logical) / 2.0,
            f64::from(layout.origin_y_logical) + f64::from(layout.h_logical) / 2.0,
        );
        Some((retained, center))
    }

    /// When the driver's next input is due; for typing and the wheel, also when they end.
    fn driver_due(&self) -> Option<Instant> {
        match &self.driver {
            DriverState::None => None,
            DriverState::Typing(typing) => typing.next_due(),
            DriverState::Sweep(sweep) => Some(sweep.started + sweep.interval * sweep.ticks),
            DriverState::Drag(drag) => {
                (!drag.finished).then(|| drag.started + drag.interval * drag.ticks)
            }
            DriverState::Wheel(wheel) => {
                Some(wheel.started + wheel.interval * wheel.sent.min(wheel.total))
            }
        }
    }

    /// Inject at most one due input, so a late wake catches up one input per pass.
    fn drive(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        if self.driver_due().is_none_or(|due| now < due) {
            return;
        }
        match self.driver {
            DriverState::None => {}
            DriverState::Typing(_) => self.inject_typing(event_loop),
            DriverState::Sweep(_) => self.inject_sweep(event_loop),
            DriverState::Drag(_) => self.inject_drag(event_loop),
            DriverState::Wheel(_) => self.inject_wheel(event_loop),
        }
    }

    /// Type the next character: the previous sample closes and this character's sample opens.
    fn inject_typing(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Typing(typing) = &mut self.driver else {
            return;
        };
        let Some(origin) = typing.origin else {
            return;
        };
        if typing.typed >= typing.chars {
            return;
        }
        let target = echo_target(origin, typing.cols, typing.typed);
        let pane = typing.pane;
        typing.typed += 1;
        let injected = Instant::now();
        if typing.typed == typing.chars {
            typing.done_at = Some(injected);
        }
        // A sample with no candidate by the next injection is unattributed.
        self.close_sample(UnattributedReason::NoCandidate);
        self.open_sample = Some(OpenSample { pane, target, injected, inject_unix_s: unix_now() });
        self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(target.character.to_string())));
    }

    fn inject_sweep(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Sweep(sweep) = &mut self.driver else {
            return;
        };
        let position = sweep.position();
        sweep.ticks += 1;
        self.pointer(event_loop, position);
    }

    /// Press, move across the grid, release; idle ticks are skipped to the next cycle's press.
    fn inject_drag(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Drag(drag) = &mut self.driver else {
            return;
        };
        let step = drag.ticks % drag.cycle_ticks;
        let (motion, advance) = if step == 0 {
            let release_at = drag.started + drag.interval * (drag.ticks + drag.move_ticks + 1);
            if release_at >= drag.deadline {
                // When: a whole press, move and release no longer fits in the phase, the drag stops.
                drag.finished = true;
                return;
            }
            drag.pressed = true;
            (DragMotion::Press(drag.start), 1)
        } else if step <= drag.move_ticks {
            let fraction = f64::from(step) / f64::from(drag.move_ticks);
            (DragMotion::Move(lerp(drag.start, drag.end, fraction)), 1)
        } else {
            drag.pressed = false;
            (DragMotion::Release, drag.cycle_ticks - step)
        };
        drag.ticks += advance;
        match motion {
            DragMotion::Press(point) => {
                self.pointer(event_loop, point);
                self.button(event_loop, ElementState::Pressed);
            }
            DragMotion::Move(point) => self.pointer(event_loop, point),
            DragMotion::Release => self.button(event_loop, ElementState::Released),
        }
    }

    /// One wheel tick: up through the retained history, then back down.
    fn inject_wheel(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Wheel(wheel) = &mut self.driver else {
            return;
        };
        if wheel.sent >= wheel.total {
            return;
        }
        // A positive vertical line delta scrolls up, into history.
        let lines = if wheel.sent < wheel.total / 2 { 1.0 } else { -1.0 };
        wheel.sent += 1;
        let delta = MouseScrollDelta::LineDelta(0.0, lines);
        let event = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta,
            phase: TouchPhase::Moved,
        };
        self.window_input(event_loop, event);
    }

    /// Dispatch a synthetic event to the measurement window.
    fn window_input(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(window_id) = self.main_id else {
            return;
        };
        self.forward(event_loop, Dispatch::Harness, |app, active| {
            app.window_event(active, window_id, event)
        });
    }

    fn pointer(&mut self, event_loop: &ActiveEventLoop, point: (f64, f64)) {
        let position = PhysicalPosition::new(point.0, point.1);
        self.window_input(
            event_loop,
            WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position },
        );
    }

    fn button(&mut self, event_loop: &ActiveEventLoop, state: ElementState) {
        let event = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        };
        self.window_input(event_loop, event);
    }

    /// Drop the window to the normal level and open the harness's opaque, never-activated cover
    /// at the floating level over the same frame, so the cover is above it by level.
    fn cover(&mut self, event_loop: &ActiveEventLoop) {
        let Some(main) = self.app.main_window().cloned() else {
            self.invalidate(event_loop, "no measurement window to cover".to_owned());
            return;
        };
        main.set_window_level(WindowLevel::Normal);
        let attributes = Window::default_attributes()
            .with_title("perf_scenarios cover")
            .with_decorations(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(main.outer_position().unwrap_or_default())
            .with_inner_size(main.outer_size());
        match event_loop.create_window(attributes) {
            Ok(cover) => {
                self.cover = Some(cover);
                self.occlusion.cover_up = true;
                self.arm_occlusion_wait(true);
            }
            Err(error) => {
                self.invalidate(event_loop, format!("cannot open the occlusion cover: {error}"))
            }
        }
    }

    /// Raise the window back to the floating level, then close the cover.
    fn uncover(&mut self) {
        if let Some(main) = self.app.main_window() {
            main.set_window_level(WindowLevel::AlwaysOnTop);
        }
        self.cover = None;
        self.occlusion.cover_up = false;
        self.occlusion.uncover_requested = true;
        self.arm_occlusion_wait(false);
    }

    /// Expect the harness-caused occlusion state `occluded`, and deliver it synthetically when
    /// no native event brings it within 2 s.
    fn arm_occlusion_wait(&mut self, occluded: bool) {
        if cfg!(target_os = "macos") {
            // When: on macOS, which reports occlusion natively; Windows reports none, so no
            // synthetic state reaches the App there and `uncover_ms` stays null.
            self.occlusion.wait = Some((occluded, Instant::now() + OCCLUSION_FALLBACK));
        }
    }

    /// A native occlusion change for the measurement window. Each state reaches the App at most
    /// once. Reveal events before the first present are expected; after it, a change the harness
    /// did not cause voids the run: covered while no cover is up, or uncovered while it is.
    fn native_occlusion(&mut self, event_loop: &ActiveEventLoop, occluded: bool) {
        if self.occlusion.delivered == Some(occluded) {
            // When: the App already has this state, natively or from the fallback, it is dropped.
            return;
        }
        if self.first_present && occluded != self.occlusion.cover_up {
            let what = if occluded {
                "Occluded(true) outside the cover"
            } else {
                "Occluded(false) while covered"
            };
            self.invalidate(event_loop, format!("unrequested native occlusion change: {what}"));
            return;
        }
        self.deliver_occlusion(event_loop, occluded);
    }

    fn deliver_occlusion(&mut self, event_loop: &ActiveEventLoop, occluded: bool) {
        self.occlusion.delivered = Some(occluded);
        if self.occlusion.wait.is_some_and(|(expected, _)| expected == occluded) {
            self.occlusion.wait = None;
        }
        if !occluded && self.occlusion.uncover_requested {
            // Uncover time runs from here to the end of the first dispatch that presents.
            self.occlusion.uncover_requested = false;
            self.occlusion.uncover_from = Some(Instant::now());
        }
        self.window_input(event_loop, WindowEvent::Occluded(occluded));
    }

    /// Deliver the state the harness caused when no native event brought it within 2 s.
    fn deliver_overdue_occlusion(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        let Some((expected, deadline)) = self.occlusion.wait else {
            return;
        };
        if now < deadline {
            return;
        }
        self.occlusion.wait = None;
        if self.occlusion.delivered != Some(expected) {
            self.synthetic_occlusion = true;
            tracing::info!(target: LOG_TARGET, occluded = expected, "no native occlusion change within 2 s; synthetic one delivered");
            self.deliver_occlusion(event_loop, expected);
        }
    }

    /// The probe's next wake. A measured phase adds only its next injection, its deadline and one
    /// trailing scan check; files are polled only outside measured phases.
    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let deadline = match self.stage {
            Stage::Done => return None,
            Stage::Startup => self.first_present_bound.wake(self.run_deadline),
            Stage::Setup { .. } | Stage::Acks | Stage::Ready => now + POLL_INTERVAL,
            Stage::Steps(_) if self.checkpoint_wait.is_some() => now + POLL_INTERVAL,
            Stage::Steps(_) => [
                self.driver_due(),
                self.current_phase().and_then(|phase| self.phase_deadline(phase)),
                self.scan.trailing(),
                self.occlusion.wait.map(|(_, at)| at),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(self.run_deadline),
        };
        Some(deadline.min(self.run_deadline))
    }
}
