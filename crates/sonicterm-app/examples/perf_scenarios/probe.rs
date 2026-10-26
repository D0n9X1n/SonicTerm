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
use sonicterm_types::ResourceAmount;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{
    DeviceId, ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use crate::atlas_retry::{self, Arm, Counts, Progress, RecoveryEpisodes, Scene, SceneReading};
use crate::attribution::{self, Start};
use crate::cli::{RunArgs, REFUSED};
use crate::counters::{CounterTotals, CountersMode};
use crate::dispatch_timeline::sample::{SampleTimeline, Storage as TimelineStorage};
use crate::dispatch_timeline::{Invocation, InvocationKind, OuterKind};
#[cfg(perf_completeness_api)]
use crate::record::completeness_from_checkpoint;
use crate::record::EchoOutcome;
use crate::record::{
    armed_token, attribute_dispatch, bulk_tail_mismatch, echo_api, echo_outcome, echo_target,
    line_row_near_cursor, missing_wide_tokens, planned_rows, presenter_blocked,
    presenter_record_for, prompt_identity, prompt_origin, protocol_rows, retained_text,
    row_count_mismatch, sentinel_row, snapshot_echo, snapshot_identity_changed, split_in_scope,
    takes_completeness, wide_tokens, write_progress, ArmState, AtlasReading, Attribution,
    CheckpointRecord, CompletenessRecord, DispatchObservation, EchoSnapshot, EchoTarget,
    LatencySample, Measurements, MonitorInfo, PhaseRecord, PresenterRecord, RowIdentity, RunResult,
    SlowDispatch, SlowDispatches, Status, Throughput, TrimHookOutcome, UnattributedReason,
    CHECKPOINT_MEMORY,
};
use crate::scan_throttle::{ScanThrottle, ScanTrigger};
use crate::scenarios::{
    self, Act, Driver, Fixture, Host, PhaseEnd, PhaseSpec, Plan, SetupAction, Step, Workload,
};
use crate::timeline::{analyze, unarmed_timeline, Credited, TimelineTake};
use crate::waits::{
    self, BarrierProgress, CheckpointProgress, CheckpointSampling, FirstPresentBound,
    FootprintStatus, FrameBarrier, ImagePresent, ImageProgress, ImageVerdict, TrimDispatch,
    FIRST_PRESENT_WAIT, IMAGE_REGISTER_WAIT,
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
    /// Windows: a native pointer move at rest under the window, dropped and counted.
    PointerRest,
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
    /// The phase's kind, from its contract.
    kind: scenarios::PhaseKind,
    /// A transition's endpoint; None for other kinds.
    endpoint: Option<scenarios::Endpoint>,
    name: &'static str,
    started: Instant,
    start_unix_s: f64,
    cpu_start: (f64, f64),
    presented_frames: u64,
    redraw_requested: u64,
    dispatch_ms: Vec<f64>,
    /// The redraw dispatches that presented a frame, in ms.
    dispatch_presenting_ms: Vec<f64>,
    /// The redraw dispatches that presented nothing, in ms.
    dispatch_nonpresenting_ms: Vec<f64>,
    /// The phase's longest dispatches with their spans, bounded.
    slow_dispatches: SlowDispatches,
    present_interval_ms: Vec<f64>,
    allocations: Option<Vec<u64>>,
    /// The phase's counted updates, from its spec.
    updates: Option<u32>,
    last_present: Option<Instant>,
    /// The phase's presentation trace from its main-window redraws.
    present: crate::transition::PresentTrace,
    /// Counter totals when the phase started; `None` when the run does not count.
    counters_start: Option<CounterTotals>,
}

/// The synthetic input a phase is injecting.
enum DriverState {
    None,
    Typing(Typing),
    Sweep(Sweep),
    Drag(Drag),
    Wheel(Wheel),
    AtlasRetry(Box<AtlasRetryDriver>),
    Park(Park),
    Commit(Commit),
}

/// S1/atlas-retry: the episode machine, what is armed for its next frame, and the scene it settled on.
struct AtlasRetryDriver {
    machine: RecoveryEpisodes,
    /// A redraw request the next drive forwards.
    redraw_pending: bool,
    /// When the pending arm was made; the drive is due from then.
    armed_at: Instant,
    /// Why the run cannot be read; the next drive ends it as invalid.
    failure: Option<String>,
    /// When the driver started; the evidence line's times are relative to it.
    started: Instant,
    /// The search for the final process, whose instant is the settle barrier's T.
    final_lookup: atlas_retry::FinalLookup,
    /// `atlas_recovery` once every episode is recorded and validated; the probe moves it out.
    recovery: Option<serde_json::Value>,
}

impl AtlasRetryDriver {
    /// A driver settling from `now`, with the first settling redraw already requested.
    fn new(now: Instant, final_lookup: atlas_retry::FinalLookup) -> Self {
        let mut driver = Self {
            machine: RecoveryEpisodes::new(now),
            redraw_pending: false,
            armed_at: now,
            failure: None,
            started: now,
            final_lookup,
            recovery: None,
        };
        driver.arm(Arm::Redraw, now);
        driver
    }

    /// Record the demand `arm` makes at `now`: every frame but B asks for one redraw.
    fn arm(&mut self, arm: Arm, now: Instant) {
        // B is redrawn by the retry's own request; every other frame asks for one.
        self.redraw_pending = arm != Arm::InvalidateOnly;
        self.armed_at = now;
    }

    /// Feed one forwarded dispatch to the machine and record the demand it makes. Returns the arm whose
    /// renderer side (atlas change, retained-frame invalidation) the probe applies, if any.
    fn observe(
        &mut self,
        delta: Option<Counts>,
        reading: &SceneReading,
        now: Instant,
    ) -> Option<Arm> {
        match self.machine.observe(delta, reading, now) {
            // A zero-attempt dispatch, the forwarded request's own included, never rearms.
            Progress::Waiting => None,
            Progress::Arm(arm) => {
                self.arm(arm, now);
                Some(arm)
            }
            Progress::Done => {
                match atlas_retry::records_problem(self.machine.records()) {
                    Some(problem) => self.failure = Some(problem),
                    None => {
                        let distinct = self
                            .machine
                            .scene()
                            .map_or(0, |scene| atlas_retry::distinct_keys(&scene.rows));
                        self.recovery =
                            Some(atlas_retry::recovery_json(self.machine.records(), distinct));
                    }
                }
                None
            }
            Progress::Invalid(reason) => {
                self.failure = Some(reason);
                None
            }
        }
    }

    /// Consume a pending redraw request: true at most once per arm.
    fn take_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_pending)
    }

    /// When the drive is next due: from the arm while a failure or a request waits, else the machine's
    /// deadline, so the settle deadline's wake is kept while nothing is pending.
    fn due(&self) -> Option<Instant> {
        if self.failure.is_some() || self.redraw_pending {
            // When: a failure or a redraw waits, the drive is due from when it was armed.
            Some(self.armed_at)
        } else {
            self.machine.deadline()
        }
    }
}

/// The search for S1/atlas-retry's final process. Windows fixes T at `sentinel_at`, the instant the print
/// phase saw the sentinel, since its role program is the harness binary itself; macOS looks `sleep` up.
fn final_lookup_for(
    windows: bool,
    current_exe: Option<&std::path::Path>,
    sentinel_at: Option<Instant>,
) -> atlas_retry::FinalLookup {
    let expected = final_process_name(windows, current_exe);
    if windows {
        // When: `windows`, the sentinel already proves the role program is the final process.
        atlas_retry::FinalLookup::found(expected, sentinel_at)
    } else {
        atlas_retry::FinalLookup::new(expected)
    }
}

/// The final process's name as the App's sampler reports it: the harness binary's normalized basename on
/// Windows, where it is every pane's program, and `sleep`, which the macOS role script execs, elsewhere.
fn final_process_name(windows: bool, current_exe: Option<&std::path::Path>) -> String {
    match current_exe.filter(|_| windows) {
        Some(path) => sonicterm_io::proc_info::normalize_proc_name(&path.to_string_lossy()),
        None => "sleep".to_owned(),
    }
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
    /// The row identity read with the origin, under the same guard; every sample is armed with it.
    identity: RowIdentity,
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

/// A fixed-text driver: each step's text as one `Ime::Commit` at its offset, then a settle.
struct Commit {
    steps: &'static [scenarios::CommitStep],
    sent: usize,
    started: Instant,
    settle: Duration,
}

impl Commit {
    /// When the next step is due, or the settle's end once every step is sent.
    fn next_due(&self) -> Instant {
        match self.steps.get(self.sent) {
            Some(step) => self.started + Duration::from_millis(step.after_ms),
            None => self.settle_end(),
        }
    }

    /// The end of the settle after the last step.
    fn settle_end(&self) -> Instant {
        let last_ms = self.steps.last().map_or(0, |step| step.after_ms);
        self.started + Duration::from_millis(last_ms) + self.settle
    }
}

/// S6/rest: the pointer resting on one cell, the same `CursorMoved` repeated at a fixed rate.
struct Park {
    interval: Duration,
    started: Instant,
    ticks: u32,
    point: (f64, f64),
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
    /// From here on the drag injects nothing but the release of a held press, so its last
    /// selection change presents inside the phase; `None` drags until the deadline.
    quiet_from: Option<Instant>,
    finished: bool,
}

/// How long before a status-line `select` phase ends its drag goes quiet: the pass that injects
/// one tick per wake lags the 120 Hz schedule, so the last move can otherwise present a frame late.
const STATUS_LINE_DRAG_SETTLE: Duration = Duration::from_millis(300);

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
    /// How the sample's echo watch was armed; an armed token is taken at every close.
    arm: ArmState,
    /// The row identity the sample was armed with.
    identity: RowIdentity,
    /// The sample's dispatch timeline, armed at injection and taken at every close.
    timeline: SampleTimeline,
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
    /// When the image phase began; Windows's registration bound counts from it.
    started: Option<Instant>,
}

/// A managed checkpoint waiting for the comparison script's `.done`.
struct CheckpointWait {
    stem: String,
    done: PathBuf,
    json: PathBuf,
    relative_json: String,
    deadline: Instant,
}

/// Why S1/atlas-retry is blocked in a build without `perf_atlas_retry_api`.
const ATLAS_RETRY_UNAVAILABLE: &str =
    "S1/atlas-retry is unavailable: this build lacks perf_atlas_retry_api, \
     so its App cannot change the glyph atlas during assembly or read the scene";

/// A checkpoint the plan reached and has not yet moved past: its footprint wait (managed runs) and
/// its memory sampling (builds with the checkpoint-memory hook).
struct PendingCheckpoint {
    index: usize,
    label: &'static str,
    footprint: Option<CheckpointWait>,
    /// Whether `.done` arrived; the footprint wait is kept to name the checkpoint's files.
    footprint_answered: bool,
    /// The memory sampling, created at its first attempt's clock read; `None` before that turn, and
    /// always `None` in a build without the hook.
    sampling: Option<CheckpointSampling>,
}

/// What a run's checkpoints share: where their files go, what each one waits for, and the clock and
/// file check it reads. Production passes `Instant::now` and `Path::exists`; tests pass fakes.
struct CheckpointSite<'run> {
    scratch: &'run Path,
    /// Whether a comparison script answers footprint requests.
    managed: bool,
    /// Whether this build samples memory at each checkpoint.
    sampling: bool,
    /// How long a managed checkpoint waits for `.done`.
    footprint_wait: Duration,
    /// The time now. Read afresh before each decision, so file I/O inside a turn cannot leave a
    /// stale time authorizing a sample.
    clock: &'run dyn Fn() -> Instant,
    /// Whether a file exists.
    exists: &'run dyn Fn(&Path) -> bool,
}

/// The plan's checkpoint step as the probe reaches it this turn.
struct CheckpointStep {
    /// The step's position among the plan's checkpoints, from 0; it is the record's index.
    ordinal: usize,
    label: &'static str,
    unix_s: f64,
    /// The record's optional fields; read only on first entry.
    fresh_after_unix_s: Option<f64>,
    frame_texture_bytes: Option<u64>,
    /// The renderer's glyph completeness, for S9's and S12's `end` only.
    completeness: Option<CompletenessRecord>,
}

/// What one turn at a checkpoint step decided.
#[derive(Debug, PartialEq)]
enum CheckpointOutcome {
    /// The footprint and the sampling are settled; the plan moves on.
    Advance,
    /// Something is still pending; the step runs again at the next wake.
    Wait,
    /// The run is invalid for this reason.
    Invalid(String),
}

/// Test-only: one transport a finalization route performed, with the instants it latched and read.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GuardTransportCall {
    phase_index: usize,
    started: Instant,
    counters_end_at: Instant,
    transport_started: Instant,
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
    /// When startup reached its warm-pool barrier: the startup transition's endpoint.
    startup_endpoint: Option<Instant>,
    meter: Option<PhaseMeter>,
    phases: Vec<PhaseRecord>,
    /// The just-finished phase's latched start and counter-end instants, until its transport takes them.
    guard_window: Option<(Instant, Instant)>,
    /// Test-only: replaces the take and the sidecar write, recording each transport the routes perform.
    #[cfg(test)]
    guard_transport_override: Option<Box<dyn FnMut(GuardTransportCall)>>,
    /// Test-only: the clock the finalization reads (window end, phase end, transport start) instead of now.
    #[cfg(test)]
    guard_clock_override: Option<Box<dyn FnMut() -> Instant>>,
    /// Test-only: a delivery problem a phase end reports on any host, to drive the early end_phase → finish path.
    #[cfg(test)]
    forced_delivery_problem: Option<String>,
    /// S10's attribution while its counted phase runs, taken into that phase's record when it ends.
    attribution: Option<attribution::Attribution>,
    role_panes: Vec<u64>,
    /// S6/rest: role 0's pane as the parked phase started, for its progress check.
    park_entry: Option<crate::record::ParkSnapshot>,
    /// Windows: every pane exit before the run finished, so a pane that joins the roles later is judged.
    exited_panes: Vec<(u64, Option<bool>)>,
    go_at: Option<Instant>,
    /// Each role's cursor (absolute row, column) when GO released it, for the first-output check.
    go_cursors: Vec<Option<(u64, u16)>>,
    /// The GO FIFOs written at release, held open so a role that opens its FIFO later still reads GO.
    #[cfg(unix)]
    go_signals: Vec<std::fs::File>,
    sentinel_roles: Vec<usize>,
    sentinel_seen: Vec<Option<Instant>>,
    /// Each role's READY row, lifetime-absolute, kept from the scan that first found it.
    ready_rows: Vec<Option<u64>>,
    /// Each role's sentinel row, lifetime-absolute, from the scan that saw the sentinel.
    sentinel_rows: Vec<Option<u64>>,
    /// The image atlas's retained bytes when startup ended; read only on Windows.
    image_atlas_start: Option<usize>,
    image: ImageState,
    /// The running phase's frame barrier (S11/release's media-free and reshow), if it has one.
    barrier: Option<FrameBarrier>,
    /// Each ended phase's name, end instant and end Unix time, for anchored holds and freshness.
    phase_ends: Vec<(&'static str, Instant, f64)>,
    scan: ScanThrottle,
    driver: DriverState,
    open_sample: Option<OpenSample>,
    samples: Vec<LatencySample>,
    checkpoints: Vec<CheckpointRecord>,
    checkpoint_pending: Option<PendingCheckpoint>,
    grid: Option<(u16, u16)>,
    monitor: Option<MonitorInfo>,
    throughput: Option<Throughput>,
    uncover_ms: Option<f64>,
    scrollback_rows_retained: Option<u64>,
    native_focus_dropped: u64,
    /// The last native pointer position and the at-rest moves dropped; Windows only drops any.
    native_pointer: waits::NativePointer,
    synthetic_occlusion: bool,
    /// What the trim hook reported when the window was covered.
    trim_hook: TrimHookOutcome,
    /// A trim the plan asked for that waits for the App to be covered.
    trim_pending: bool,
    /// The App's trim number the hook's own trim reported.
    trim_seq_after_hook: Option<u64>,
    /// S1/atlas-retry's `atlas_recovery`, once every episode is recorded and validated.
    atlas_recovery: Option<serde_json::Value>,
    /// Dispatch-timeline recorder storage, reused from sample to sample; empty until a sample is armed.
    timeline_storage: TimelineStorage,
    /// The `ApplicationHandler` callback running now, which a sample armed inside it starts clipped in.
    running_outer: Option<OuterKind>,
    first_present_bound: FirstPresentBound,
    /// How the main window presents, recorded at the end of startup on Windows.
    presenter: Option<PresenterRecord>,
    /// The configured software render mode, as the config file spells it.
    software_render_mode: &'static str,
    /// Windows: the note when LockSetForegroundWindow failed as the run started.
    foreground_lock_failure: Option<String>,
    outcome: Option<(Status, Option<String>)>,
    /// Whether this run records frame counters.
    counters_mode: CountersMode,
    /// The first counter snapshot the contract could not hold; it voids the run.
    counter_error: Option<String>,
    /// Tests: the presented-frame count and image atlas reading that stand in for the main renderer.
    #[cfg(test)]
    test_readings: Option<(u64, ResourceAmount)>,
    /// Tests: the echo outcome and timeline the next credited sample's takes return, in place of the App's.
    #[cfg(test)]
    test_take: Option<(EchoOutcome, TimelineTake)>,
}

impl ApplicationHandler<UserEvent> for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.outer_enter(OuterKind::Resumed);
        self.resumed_turn(event_loop);
        self.outer_exit();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.outer_enter(OuterKind::WindowEvent);
        self.window_event_turn(event_loop, window_id, event);
        self.outer_exit();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.outer_enter(OuterKind::UserEvent);
        self.user_event_turn(event_loop, event);
        self.outer_exit();
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.outer_enter(OuterKind::NewEvents);
        self.new_events_turn(event_loop, cause);
        self.outer_exit();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.outer_enter(OuterKind::AboutToWait);
        self.about_to_wait_turn(event_loop);
        self.outer_exit();
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
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

/// Holds the foreground lock for the run: while it is held no process, this one's window included,
/// takes the foreground. Pressing Alt or clicking another window ends it early.
#[cfg(windows)]
struct ForegroundLock;

#[cfg(windows)]
impl ForegroundLock {
    /// Lock foreground changes; the note for `result.json` when the call failed.
    fn acquire() -> (Self, Option<String>) {
        use windows::Win32::UI::WindowsAndMessaging::{LockSetForegroundWindow, LSFW_LOCK};
        let locked =
            // SAFETY: LockSetForegroundWindow takes its lock code by value and touches no caller memory.
            unsafe { LockSetForegroundWindow(LSFW_LOCK) };
        let failure =
            locked.err().map(|error| crate::record::foreground_lock_note(&error.to_string()));
        (Self, failure)
    }
}

// Lifecycle: ForegroundLock unlocks foreground changes when `run` returns on any path; process exit ends it too.
#[cfg(windows)]
impl Drop for ForegroundLock {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{LockSetForegroundWindow, LSFW_UNLOCK};
        // A failed unlock changes nothing the run needs, and process exit ends the lock anyway.
        let _ =
            // SAFETY: LockSetForegroundWindow takes its lock code by value and touches no caller memory.
            unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
    }
}

/// Aborts the process when the event loop has not returned shortly after the run's deadline.
struct Watchdog {
    cancel: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The GO FIFOs of a POSIX run, removed when `run` returns on any path, so nothing that later copies,
/// archives or hashes the scratch meets a named pipe.
#[cfg(unix)]
struct GoFifos {
    /// The run's scratch directory.
    scratch: PathBuf,
    /// How many roles the plan has; each one's `go/<role>` may be a FIFO.
    roles: usize,
}

// Lifecycle: GoFifos unlinks each role's GO FIFO when `run` returns on any path; an open handle stays valid.
#[cfg(unix)]
impl Drop for GoFifos {
    fn drop(&mut self) {
        use std::os::unix::fs::FileTypeExt;
        for role in 0..self.roles {
            let marker = self.scratch.join(format!("go/{role}"));
            let fifo =
                std::fs::symlink_metadata(&marker).is_ok_and(|meta| meta.file_type().is_fifo());
            if fifo {
                // A failed removal leaves the FIFO, which the comparison's scratch copy records and skips.
                let _ = std::fs::remove_file(&marker);
            }
        }
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

/// The most rows above S3's sentinel compared with the end of bulk.txt; about what history retains.
const BULK_TAIL_LINES: usize = 1_000;
/// How many times a delivery check tries a busy parser, and the pause between tries.
const GRID_READ_ATTEMPTS: u32 = 50;
const GRID_READ_PAUSE: Duration = Duration::from_millis(5);

/// What scratch preparation hands the run.
struct Prepared {
    config: Config,
    logging: sonicterm_logging::LoggingGuard,
    nonce: String,
}

impl PhaseMeter {
    fn start(
        name: &'static str,
        kind: scenarios::PhaseKind,
        endpoint: Option<scenarios::Endpoint>,
        counting: bool,
        counters_start: Option<CounterTotals>,
        updates: Option<u32>,
    ) -> Self {
        Self {
            name,
            kind,
            endpoint,
            started: Instant::now(),
            start_unix_s: unix_now(),
            cpu_start: cpu_times(),
            presented_frames: 0,
            redraw_requested: 0,
            dispatch_ms: Vec::new(),
            dispatch_presenting_ms: Vec::new(),
            dispatch_nonpresenting_ms: Vec::new(),
            slow_dispatches: SlowDispatches::default(),
            present_interval_ms: Vec::new(),
            allocations: counting.then(Vec::new),
            updates,
            last_present: None,
            present: crate::transition::PresentTrace::default(),
            counters_start,
        }
    }
}

/// One step of a drag gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DragMotion {
    Press((f64, f64)),
    Move((f64, f64)),
    Release,
}

impl Probe {
    fn invalidate(&mut self, event_loop: &ActiveEventLoop, reason: String) {
        self.finish(event_loop, Status::Invalid, Some(reason));
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod probe_tests;

#[path = "probe_callbacks.rs"]
mod callbacks;

#[path = "probe_bootstrap.rs"]
mod bootstrap;
use bootstrap::cpu_times;
pub(crate) use bootstrap::run;
use bootstrap::{ms_between, unix_now, write_atomic, write_atomic_with};

#[path = "probe_checkpoint.rs"]
mod checkpoint;
use checkpoint::steps_wake;
#[path = "probe_atlas.rs"]
mod atlas;
#[path = "probe_input.rs"]
mod input;
#[path = "probe_occlusion.rs"]
mod occlusion;
#[path = "probe_phases.rs"]
mod phases;
#[path = "probe_sampling.rs"]
mod sampling;
#[path = "probe_scan.rs"]
mod scan;
use occlusion::barrier_phase_deadline;
#[path = "probe_api.rs"]
mod api;
use api::checkpoint_frame_texture_bytes;
use api::renderer_completeness;
use api::sample_checkpoint_memory;
use api::trim_covered;
use api::{atlas_retry_counts, atlas_retry_fallback_applies, atlas_retry_title_counters};
use api::{atlas_retry_scene_parts, change_atlas_during_next_assembly};
use api::{install_counting_taskbar_progress, take_taskbar_deliveries};

#[cfg(test)]
use bootstrap::nonce_seed;

#[cfg(test)]
use checkpoint::attach_atlas_readings;

#[cfg(test)]
use checkpoint::checkpoint_turn;

#[cfg(test)]
use checkpoint::plan_moves_on;
