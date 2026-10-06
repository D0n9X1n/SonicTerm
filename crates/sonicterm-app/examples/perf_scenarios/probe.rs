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
#[cfg(perf_completeness_api)]
use crate::record::completeness_from_checkpoint;
use crate::record::{
    attribute_dispatch, bulk_tail_mismatch, echo_api, echo_outcome, echo_target,
    line_row_near_cursor, missing_wide_tokens, planned_rows, presenter_blocked,
    presenter_record_for, prompt_identity, prompt_origin, protocol_rows, retained_text,
    row_count_mismatch, snapshot_echo, snapshot_identity_changed, split_in_scope,
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
    /// How the sample's echo watch was armed; an armed token is taken at every close.
    arm: ArmState,
    /// The row identity the sample was armed with.
    identity: RowIdentity,
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

/// Take attempt `attempt` at checkpoint `index`'s memory sample; true when every pane was measured.
#[cfg(feature = "perf-hook-checkpoint-memory")]
fn sample_checkpoint_memory(app: &mut App, index: usize, label: &str, attempt: u32) -> bool {
    app.__perf_checkpoint_memory(index, label, attempt).complete
}

/// This build has no checkpoint-memory hook, so its checkpoints never sample and this is never called.
#[cfg(not(feature = "perf-hook-checkpoint-memory"))]
fn sample_checkpoint_memory(_app: &mut App, _index: usize, _label: &str, _attempt: u32) -> bool {
    false
}

/// The main renderer's attempt, presentation, row-cache and shaping counts from its window's counter
/// record, its glyph atlas resets and its atlas dimension; `None` when any counter field is missing.
#[cfg(feature = "perf-counters")]
fn atlas_retry_counts(app: &App) -> Option<Counts> {
    let window_id = app.main_window()?.id();
    let snapshot = app.frame_counters_snapshot()?;
    let record = &snapshot.windows.iter().find(|(id, _)| *id == window_id)?.1;
    let renderer = app.main_renderer()?;
    Some(Counts {
        attempts: record.count("render_attempts")?,
        presented: record.count("render_attempts_presented")?,
        resets: renderer.__test_glyph_atlas_resets(),
        hits: record.count("row_cache_hits")?,
        misses: record.count("row_cache_misses")?,
        shapes: record.count("shape_requests")?,
        atlas_dim: renderer.glyph_atlas_facts().dim,
    })
}

/// This build has no counter API, so the episodes cannot be observed.
#[cfg(not(feature = "perf-counters"))]
fn atlas_retry_counts(_app: &App) -> Option<Counts> {
    None
}

/// The counters a tab-title change moves: the main window's title-cache misses, and the App's completed
/// foreground-worker batches and stale foreground results; each `None` when it cannot be read.
#[cfg(feature = "perf-counters")]
fn atlas_retry_title_counters(app: &App) -> (Option<u64>, Option<u64>, Option<u64>) {
    let Some(snapshot) = app.frame_counters_snapshot() else {
        return (None, None, None);
    };
    let window = app.main_window().map(|window| window.id());
    let prepares = snapshot
        .windows
        .iter()
        .find(|(id, _)| Some(*id) == window)
        .and_then(|(_, record)| record.count("tab_title_prepares"));
    (prepares, snapshot.app.count("fg_worker_probes"), snapshot.app.count("fg_results_stale"))
}

/// This build has no counter API, so none of the title counters can be read.
#[cfg(not(feature = "perf-counters"))]
fn atlas_retry_title_counters(_app: &App) -> (Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None)
}

/// Why S1/atlas-retry is blocked in a build without `perf_atlas_retry_api`.
const ATLAS_RETRY_UNAVAILABLE: &str =
    "S1/atlas-retry is unavailable: this build lacks perf_atlas_retry_api, \
     so its App cannot change the glyph atlas during assembly or read the scene";

/// Change the glyph atlas during `renderer`'s next assembly, so that frame retries.
#[cfg(perf_atlas_retry_api)]
fn change_atlas_during_next_assembly(renderer: &mut GpuRenderer) {
    renderer.__change_glyph_atlas_during_next_assembly();
}

/// Without `perf_atlas_retry_api` the driver never starts, so nothing is armed.
#[cfg(not(perf_atlas_retry_api))]
fn change_atlas_during_next_assembly(_renderer: &mut GpuRenderer) {}

/// Window `window_id`'s active tab title, its renderer's fallback notice id and how many characters
/// the last frame drew as missing, in the grid and in chrome; `None` when any cannot be read.
#[cfg(perf_atlas_retry_api)]
fn atlas_retry_scene_parts(app: &App, window_id: WindowId) -> Option<(String, u64, usize)> {
    let title = app.__test_window_active_tab_title(window_id)?;
    let renderer = app.main_renderer()?;
    let missing = renderer.last_missing_tofu().len() + renderer.last_missing_chrome().len();
    Some((title, renderer.font_fallback_notice_id()?, missing))
}

/// Without `perf_atlas_retry_api` the scene cannot be read.
#[cfg(not(perf_atlas_retry_api))]
fn atlas_retry_scene_parts(_app: &App, _window_id: WindowId) -> Option<(String, u64, usize)> {
    None
}

/// Frames the main window's renderer applied a font fallback in, cumulative; `None` when the
/// counter cannot be read.
#[cfg(feature = "perf-counters")]
fn atlas_retry_fallback_applies(app: &App) -> Option<u64> {
    let window_id = app.main_window()?.id();
    let snapshot = app.frame_counters_snapshot()?;
    snapshot.windows.iter().find(|(id, _)| *id == window_id)?.1.count("font_fallback_applies")
}

/// This build has no counter API, so the fallback state cannot be read.
#[cfg(not(feature = "perf-counters"))]
fn atlas_retry_fallback_applies(_app: &App) -> Option<u64> {
    None
}

/// Call the App's covered-window trim hook for `window_id`, with the trim number a trim reported;
/// only a tree that declares the hook builds this.
#[cfg(feature = "perf-hook-trim")]
fn trim_covered(app: &mut App, window_id: WindowId) -> (TrimHookOutcome, Option<u64>) {
    use sonicterm_app::app::TrimDecision;
    match app.__trim_covered_now(window_id) {
        TrimDecision::Trimmed { trim_seq } => (TrimHookOutcome::Trimmed, Some(trim_seq)),
        TrimDecision::Skipped(_) => (TrimHookOutcome::Skipped, None),
        TrimDecision::Unsupported => (TrimHookOutcome::Unsupported, None),
    }
}

/// This build has no trim hook, so the covered window is never trimmed and the run is a baseline.
#[cfg(not(feature = "perf-hook-trim"))]
fn trim_covered(_app: &mut App, _window_id: WindowId) -> (TrimHookOutcome, Option<u64>) {
    (TrimHookOutcome::Unsupported, None)
}

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

impl PendingCheckpoint {
    /// Where the footprint wait stands: absent when unmanaged.
    fn footprint_status(&self) -> FootprintStatus {
        match (&self.footprint, self.footprint_answered) {
            (None, _) => FootprintStatus::Absent,
            (Some(_), false) => FootprintStatus::Pending,
            (Some(_), true) => FootprintStatus::Answered,
        }
    }

    /// The probe's next wake for this checkpoint; `next_deadline` uses it while one is pending.
    fn wake(&self, now: Instant) -> Option<Instant> {
        waits::checkpoint_wake(self.footprint_status(), self.sampling.as_ref(), now, POLL_INTERVAL)
    }
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

/// Whether the plan moves past a checkpoint step after `outcome`: only once it advanced. The step
/// loop starts the next phase only when this is true.
fn plan_moves_on(outcome: &CheckpointOutcome) -> bool {
    matches!(outcome, CheckpointOutcome::Advance)
}

/// The probe's next wake while it runs its steps: the pending checkpoint's own wake while one is
/// pending, else the soonest of `phase_wakes`, else `run_deadline`; never later than `run_deadline`.
fn steps_wake(
    pending: Option<&PendingCheckpoint>,
    now: Instant,
    phase_wakes: impl IntoIterator<Item = Option<Instant>>,
    run_deadline: Instant,
) -> Instant {
    let wake = match pending {
        Some(pending) => pending.wake(now).unwrap_or(now + POLL_INTERVAL),
        None => phase_wakes.into_iter().flatten().min().unwrap_or(run_deadline),
    };
    wake.min(run_deadline)
}

/// One turn at a checkpoint step. The first turn writes the record and the request once, then
/// every turn polls the footprint and takes a memory sample when one is due, recording each
/// attempt on the record. `sample` takes attempt `attempt` at checkpoint `index` (`label`) and
/// returns whether it measured every pane. The step advances only when `checkpoint_ready` holds.
fn checkpoint_turn(
    pending: &mut Option<PendingCheckpoint>,
    records: &mut Vec<CheckpointRecord>,
    site: &CheckpointSite<'_>,
    step: &CheckpointStep,
    mut sample: impl FnMut(usize, &str, u32) -> bool,
) -> CheckpointOutcome {
    let mut current = match pending.take() {
        Some(current) if current.index == step.ordinal && current.label == step.label => current,
        Some(current) => {
            // When: the pending checkpoint is not this step's, two checkpoints would mix; stop.
            return CheckpointOutcome::Invalid(format!(
                "checkpoint identity changed: {}-{} was pending when the plan reached {}-{}",
                current.index, current.label, step.ordinal, step.label
            ));
        }
        None => open_checkpoint(records, site, step, (site.clock)()),
    };
    if !current.footprint_answered {
        if let Some(wait) = &current.footprint {
            let done = (site.exists)(&wait.done);
            // The footprint file is looked at only once `.done` says it is complete.
            let footprint = done && (site.exists)(&wait.json);
            match waits::checkpoint_progress(done, footprint, (site.clock)(), wait.deadline) {
                CheckpointProgress::Waiting => {}
                CheckpointProgress::Answered { footprint } => {
                    current.footprint_answered = true;
                    if footprint {
                        if let Some(record) =
                            records.iter_mut().rev().find(|record| record.index == current.index)
                        {
                            record.footprint_file = Some(wait.relative_json.clone());
                        }
                    }
                }
                CheckpointProgress::Expired => {
                    return CheckpointOutcome::Invalid(waits::checkpoint_expired_reason(
                        &wait.stem,
                        site.footprint_wait,
                    ));
                }
            }
        }
    }
    let (index, label) = (current.index, current.label);
    if site.sampling {
        // The clock is read here, after the request write and the footprint checks, so the decision
        // to sample is made at the time it is taken. The sampling state is created from this same
        // read on the first turn, so its window opens at its first attempt, never before file I/O.
        let now = (site.clock)();
        let sampling = current.sampling.get_or_insert_with(|| CheckpointSampling::new(index, now));
        waits::sampling_turn(sampling, now, |attempt| sample(index, label, attempt));
        if let Some(record) = records.iter_mut().rev().find(|record| record.index == index) {
            record.sampling = Some(sampling.state.as_str());
            record.attempts = Some(sampling.attempts);
            record.last_attempt_complete = Some(sampling.last_attempt_complete);
        }
    }
    if waits::checkpoint_ready(current.footprint_status(), current.sampling.as_ref()) {
        CheckpointOutcome::Advance
    } else {
        // When: the footprint or the sampling is still pending, keep the checkpoint for the next wake.
        *pending = Some(current);
        CheckpointOutcome::Wait
    }
}

/// Append each `(checkpoint index, reading)` to the latest record of that checkpoint, in the order the
/// attempts were taken.
fn attach_atlas_readings(records: &mut [CheckpointRecord], readings: Vec<(usize, AtlasReading)>) {
    for (index, reading) in readings {
        if let Some(record) = records.iter_mut().rev().find(|record| record.index == index) {
            record.atlas_readings.push(reading);
        }
    }
}

/// A checkpoint's first turn: write its request, push its record, and start its footprint wait
/// (managed runs) from `now`. Its sampling (builds with the hook) is created later in the turn, from
/// a clock read taken after this I/O, immediately before its first attempt.
fn open_checkpoint(
    records: &mut Vec<CheckpointRecord>,
    site: &CheckpointSite<'_>,
    step: &CheckpointStep,
    now: Instant,
) -> PendingCheckpoint {
    let index = step.ordinal;
    let label = step.label;
    let stem = format!("{index}-{label}");
    let request =
        serde_json::json!({"label": label, "unix_s": step.unix_s, "pid": std::process::id()});
    let path = site.scratch.join(format!("checkpoints/{stem}.request"));
    if let Err(error) = write_atomic(&path, request.to_string().as_bytes()) {
        tracing::warn!(target: LOG_TARGET, %error, label, "cannot write the checkpoint request");
    }
    tracing::info!(target: LOG_TARGET, index, label, "perf_scenarios checkpoint");
    records.push(CheckpointRecord {
        index,
        label,
        unix_s: step.unix_s,
        footprint_file: None,
        fresh_after_unix_s: step.fresh_after_unix_s,
        frame_texture_bytes: step.frame_texture_bytes,
        completeness: step.completeness.clone(),
        ..CheckpointRecord::default()
    });
    let footprint = site.managed.then(|| CheckpointWait {
        done: site.scratch.join(format!("checkpoints/{stem}.done")),
        json: site.scratch.join(format!("checkpoints/{stem}.json")),
        relative_json: format!("checkpoints/{stem}.json"),
        deadline: now + site.footprint_wait,
        stem,
    });
    PendingCheckpoint { index, label, footprint, footprint_answered: false, sampling: None }
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
    /// S10's attribution while its counted phase runs, taken into that phase's record when it ends.
    attribution: Option<attribution::Attribution>,
    role_panes: Vec<u64>,
    /// Windows: every pane exit before the run finished, so a pane that joins the roles later is judged.
    exited_panes: Vec<(u64, Option<bool>)>,
    go_at: Option<Instant>,
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
            // Only native events reach this handler; the probe's synthetic moves go to the App directly.
            WindowEvent::CursorMoved { position, .. }
                if is_main
                    && self.native_pointer.arrive(
                        scenarios::BUILD_HOST,
                        (position.x, position.y),
                        self.go_at.is_some(),
                    ) =>
            {
                Arrival::PointerRest
            }
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
            Arrival::PointerRest => {
                // When: the pointer rests where the window opened under it; it is counted, not input.
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
        if let UserEvent::PaneProcessExited { pane_id, was_clean } = &event {
            if scenarios::BUILD_HOST == Host::Windows && self.outcome.is_none() {
                // When: on Windows each exit is kept, so a pane that becomes a role pane later is still judged.
                self.exited_panes.push((*pane_id, *was_clean));
                // The first role pane is recorded as startup ends, so an empty list means startup is running.
                let startup_ended = !self.role_panes.is_empty();
                let reason = waits::role_exit_reason(
                    *pane_id,
                    *was_clean,
                    &self.role_panes,
                    startup_ended,
                    false,
                );
                if let Some(reason) = reason {
                    // When: a role's program exited mid-run; the App keeps the pane, so a Hold phase would pass.
                    self.invalidate(event_loop, reason);
                }
            }
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
                scenarios::BUILD_HOST,
            );
            self.invalidate(event_loop, reason);
            return;
        }
        self.deliver_overdue_occlusion(event_loop, now);
        self.service_trim(false);
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
            // The end of the first about_to_wait after a present is the warm-pool barrier; its instant
            // is latched before the startup meter finishes and the progress file is written.
            self.startup_endpoint.get_or_insert(Instant::now());
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
    // The right to lock lasts only until user input arrives, so the lock comes before anything else;
    // the guard is dropped as `run` returns, on every path.
    #[cfg(windows)]
    let (_foreground_lock, foreground_lock_failure) = ForegroundLock::acquire();
    #[cfg(not(windows))]
    let foreground_lock_failure: Option<String> = None;
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
    if let Some(note) = &foreground_lock_failure {
        // When: the lock failed, the window may take the foreground as it opens; the run goes on.
        tracing::warn!(target: LOG_TARGET, "{note}");
    }
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
    let meter = PhaseMeter::start(
        "startup",
        scenarios::STARTUP_KIND,
        Some(scenarios::STARTUP_ENDPOINT),
        allocation_counter.is_some(),
        None,
        None,
    );
    // Read before the App takes the config, so the presenter record can name the configured mode.
    let software_render_mode = render_mode_text(prepared.config.appearance.software_render_mode);
    let app =
        App::new_with_proxy(Theme::default(), prepared.config, Keymap::default(), Some(proxy));
    let mut probe = Probe {
        sentinels,
        allocation_counter,
        meter: Some(meter),
        software_render_mode,
        foreground_lock_failure,
        ..Probe::new(app, plan, request.clone(), scratch, run_deadline)
    };
    // --counters opens the App's counter gate before its first window; startup's start is the
    // empty baseline read right after, since nothing counted before the gate opened.
    if let Err(reason) = probe.enable_counters() {
        // When: `enable_counters` failed, the run is refused before any window exists.
        eprintln!("perf_scenarios: refused: --counters: {reason}");
        return REFUSED;
    }
    let baseline = probe.counter_totals();
    if let Some(meter) = probe.meter.as_mut() {
        meter.counters_start = baseline;
    }
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

/// Start logging in `dir`. A laps run without `--counters` uses a filter that keeps the counter
/// gate off, so the App counts nothing in a run that reports `off`.
#[cfg(feature = "perf-counters")]
fn start_logging(
    config: &sonicterm_logging::LoggingConfig,
    dir: &Path,
    request: &RunArgs,
) -> std::io::Result<sonicterm_logging::LoggingGuard> {
    match workload::logging_filter(request.laps, request.counters) {
        Some(filter) => sonicterm_logging::init_in_with_filter(dir, &filter),
        None => sonicterm_logging::init_in(config, dir),
    }
}

/// Start logging in `dir` at the configured level; this build cannot read the counter gate.
#[cfg(not(feature = "perf-counters"))]
fn start_logging(
    config: &sonicterm_logging::LoggingConfig,
    dir: &Path,
    _request: &RunArgs,
) -> std::io::Result<sonicterm_logging::LoggingGuard> {
    sonicterm_logging::init_in(config, dir)
}

/// The most rows above S3's sentinel compared with the end of bulk.txt; about what history retains.
const BULK_TAIL_LINES: usize = 1_000;
/// How many times a delivery check tries a busy parser, and the pause between tries.
const GRID_READ_ATTEMPTS: u32 = 50;
const GRID_READ_PAUSE: Duration = Duration::from_millis(5);

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
    let logging = start_logging(&config.logging, &scratch.join("logs"), request)
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
            slow_dispatches: SlowDispatches::default(),
            present_interval_ms: Vec::new(),
            allocations: counting.then(Vec::new),
            updates,
            last_present: None,
            present: crate::transition::PresentTrace::default(),
            counters_start,
        }
    }

    /// Count one forwarded dispatch that ran from `started` to `ended` and moved the main renderer's presented
    /// count from `frames.0` to `frames.1`: presented frames and intervals for any dispatch, and for a
    /// main-window redraw its duration, span, allocations and presentation trace.
    fn observe_dispatch(
        &mut self,
        kind: Dispatch,
        started: Instant,
        ended: Instant,
        frames: (u64, u64),
        allocations: Option<u64>,
    ) {
        let (frames_before, frames_after) = frames;
        let advanced = frames_after > frames_before;
        if advanced {
            self.presented_frames += frames_after - frames_before;
            if let Some(last) = self.last_present {
                self.present_interval_ms.push(ms_between(last, ended));
            }
            self.last_present = Some(ended);
        }
        if kind == Dispatch::Redraw {
            self.redraw_requested += 1;
            let duration_ms = ms_between(started, ended);
            self.dispatch_ms.push(duration_ms);
            // The span in Unix seconds, from the phase's own start, so a log stamp can be matched to it.
            let start_unix_s =
                self.start_unix_s + started.saturating_duration_since(self.started).as_secs_f64();
            self.slow_dispatches.push(SlowDispatch {
                start_unix_s,
                end_unix_s: start_unix_s + duration_ms / 1000.0,
                duration_ms,
            });
            if let (Some(counts), Some(count)) = (self.allocations.as_mut(), allocations) {
                counts.push(count);
            }
            self.present
                .observe_redraw(ms_between(self.started, ended), advanced.then_some(frames_after));
        }
    }

    /// The phase's record; its counter delta is `counters_end` minus the totals at its start.
    fn finish(
        self,
        counters_end: Option<CounterTotals>,
        completion: crate::transition::Completion,
    ) -> PhaseRecord {
        let (user_s, system_s) = cpu_times();
        let frame_counters = self
            .counters_start
            .as_ref()
            .zip(counters_end.as_ref())
            .map(|(start, end)| end.delta_since(start));
        let (slow_dispatches, dispatch_count) = self.slow_dispatches.finish();
        PhaseRecord {
            name: self.name,
            kind: self.kind.name(),
            endpoint: self.endpoint.map(scenarios::Endpoint::name),
            completion_ms: completion.elapsed_ms,
            completion_missing: completion.missing,
            start_unix_s: self.start_unix_s,
            end_unix_s: unix_now(),
            cpu_user_s: user_s - self.cpu_start.0,
            cpu_system_s: system_s - self.cpu_start.1,
            presented_frames: self.presented_frames,
            redraw_requested: self.redraw_requested,
            first_present_ms: self.present.first.map(|(offset_ms, _)| offset_ms),
            last_present_ms: self.present.last.map(|(offset_ms, _)| offset_ms),
            first_present_seq: self.present.first.map(|(_, count)| count),
            last_present_seq: self.present.last.map(|(_, count)| count),
            present_missing: self.present.missing(),
            nonpresenting_redraws: self.present.nonpresenting,
            dispatch_ms: self.dispatch_ms,
            slow_dispatches,
            dispatch_count,
            present_interval_ms: self.present_interval_ms,
            allocations_per_frame: self.allocations,
            frame_counters,
            updates: self.updates,
            s10_attribution: None,
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
    /// A probe around `app` for `plan`, before startup: no window, phase or reading yet.
    ///
    /// The run's sentinels, allocation counter, startup meter, render-mode text and foreground note are
    /// set by the caller, which has them; tests build a headless probe through this same constructor.
    fn new(
        app: App,
        plan: Plan,
        request: RunArgs,
        scratch: PathBuf,
        run_deadline: Instant,
    ) -> Self {
        let roles = plan.roles.len();
        let counters_mode = CountersMode::for_run(request.counters);
        Self {
            app,
            plan: Arc::new(plan),
            request,
            scratch,
            sentinels: Vec::new(),
            allocation_counter: None,
            run_deadline,
            stage: Stage::Startup,
            main_id: None,
            cover: None,
            occlusion: OcclusionState::default(),
            frames: 0,
            first_present: false,
            startup_endpoint: None,
            meter: None,
            phases: Vec::new(),
            attribution: None,
            role_panes: Vec::new(),
            exited_panes: Vec::new(),
            go_at: None,
            sentinel_roles: Vec::new(),
            sentinel_seen: vec![None; roles],
            ready_rows: vec![None; roles],
            sentinel_rows: vec![None; roles],
            image_atlas_start: None,
            image: ImageState::default(),
            barrier: None,
            phase_ends: Vec::new(),
            scan: ScanThrottle::new(SCAN_INTERVAL),
            driver: DriverState::None,
            open_sample: None,
            samples: Vec::new(),
            checkpoints: Vec::new(),
            checkpoint_pending: None,
            grid: None,
            monitor: None,
            throughput: None,
            uncover_ms: None,
            scrollback_rows_retained: None,
            native_focus_dropped: 0,
            native_pointer: waits::NativePointer::default(),
            synthetic_occlusion: false,
            trim_hook: TrimHookOutcome::NotReached,
            trim_pending: false,
            trim_seq_after_hook: None,
            atlas_recovery: None,
            first_present_bound: FirstPresentBound::default(),
            presenter: None,
            software_render_mode: "",
            foreground_lock_failure: None,
            outcome: None,
            counters_mode,
            counter_error: None,
            #[cfg(test)]
            test_readings: None,
        }
    }

    /// Forward one dispatch to the App and account for it: frames, `RedrawRequested` time and
    /// allocations, present intervals, the uncover time and, while a sample is open, attribution.
    fn forward<EventLoopRef: ?Sized>(
        &mut self,
        event_loop: &EventLoopRef,
        kind: Dispatch,
        dispatch: impl FnOnce(&mut App, &EventLoopRef),
    ) {
        let frames_before = self.frame_count();
        // S1/atlas-retry reads the main renderer's counts and the scene around every forwarded dispatch.
        let retry_before = matches!(self.driver, DriverState::AtlasRetry(_)).then(|| {
            (atlas_retry_counts(&self.app), self.atlas_retry_scene(), self.atlas_retry_cached())
        });
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
        if let Some((counts, scene, cached)) = retry_before {
            // When: the atlas-retry driver runs, this dispatch's delta and scene feed its episode machine.
            self.observe_atlas_retry(counts, scene, cached, ended);
        }
        let advanced = frames_after > frames_before;
        self.frames = frames_after;
        if advanced {
            self.first_present = true;
            if let Some(from) = self.occlusion.uncover_from.take() {
                self.uncover_ms = Some(ms_between(from, ended));
            }
            self.image.present.observe_frames(frames_after, ended);
            self.observe_barrier_frame(ended);
        }
        if let Some(meter) = self.meter.as_mut() {
            // The adapter's own readings feed the phase's counts and presentation trace.
            meter.observe_dispatch(
                kind,
                started,
                ended,
                (frames_before, frames_after),
                allocations,
            );
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
                    // The take runs after both snapshots have released the parser lock.
                    let main = self.measurement_window();
                    let outcome = echo_outcome(sample.arm, |token| {
                        echo_api::take(&mut self.app, sample.pane, token, main)
                    });
                    let changed = snapshot_identity_changed(observation, sample.identity);
                    self.samples.push(LatencySample::credited(
                        sample.inject_unix_s,
                        sample.injected,
                        ended,
                        &outcome,
                        changed,
                    ));
                }
            }
            Attribution::Unattributed(reason) => self.close_sample(reason),
        }
    }

    /// Close the open sample, if any, as unattributed; an armed watch is disarmed and its record
    /// discarded, so a closed sample costs the worker nothing more.
    fn close_sample(&mut self, reason: UnattributedReason) {
        if let Some(sample) = self.open_sample.take() {
            if let ArmState::Armed(token) = sample.arm {
                echo_api::discard(&mut self.app, sample.pane, token);
            }
            self.samples.push(LatencySample::uncredited(sample.inject_unix_s, reason.as_str()));
        }
    }

    /// The window the measurement presents in: the real main window, else the App's main entry.
    fn measurement_window(&self) -> Option<WindowId> {
        self.main_id
            .or_else(|| self.app.main().map(|_| sonicterm_app::app::synthetic_main_window_id()))
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
        #[cfg(test)]
        if let Some((frames, _)) = self.test_readings {
            // When: a test stands in for the renderer, its count is the reading.
            return frames;
        }
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
            let found = self
                .role_grid(role, |grid| {
                    line_row_near_cursor(grid, sentinel, protocol_rows(scenarios::BUILD_HOST))
                })
                .flatten();
            if let Some(row) = found {
                self.sentinel_seen[role] = Some(now);
                self.sentinel_rows[role] = Some(row);
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
                    prompt_origin(grid, workload::PROMPT)
                        .map(|origin| (origin, grid.cols, prompt_identity(grid)))
                })
                .flatten();
            if let (Some((origin, cols, identity)), DriverState::Typing(typing)) =
                (found, &mut self.driver)
            {
                typing.origin = Some(origin);
                typing.identity = identity;
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
            frame_counters: self.counters_mode,
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

    /// End the open phase, reading its counter totals at its end, before any progress write.
    fn finish_meter(&mut self) {
        let Some(meter) = self.meter.take() else {
            // When: `meter` is None, no phase is open and nothing ends.
            return;
        };
        let completion = self.completion_of(&meter);
        let counters_end = self.counter_totals();
        let mut record = meter.finish(counters_end, completion);
        record.s10_attribution = self.attribution.take();
        self.phases.push(record);
    }

    /// A transition meter's completion: its endpoint's latched instant, eligible only before the
    /// run's deadline, or why it has none. A hold or sustained meter has none.
    fn completion_of(&self, meter: &PhaseMeter) -> crate::transition::Completion {
        let Some(endpoint) = meter.endpoint else {
            // When: `endpoint` is None, the phase is a hold or sustained one and has no completion.
            return crate::transition::Completion::default();
        };
        let reached = match endpoint {
            scenarios::Endpoint::SentinelParsed => {
                crate::transition::latest_sentinel(&self.sentinel_seen, &self.sentinel_roles)
            }
            scenarios::Endpoint::ImageRegisteredThenPresented => self.image.present.presented_at(),
            scenarios::Endpoint::FirstPresentAfterEntry
            | scenarios::Endpoint::FirstPresentWithImage => {
                self.barrier.as_ref().and_then(FrameBarrier::done_at)
            }
            scenarios::Endpoint::FirstPresentThenAboutToWait => self.startup_endpoint,
        };
        crate::transition::completion(
            meter.started,
            reached,
            self.run_deadline,
            Instant::now(),
            crate::transition::INCOMPLETE,
        )
    }

    /// Before GO of a phase that plays `updates` counted updates: read role 0's pane state, then arm
    /// the App's watch from a closed baseline. An update already open before GO voids the run.
    fn begin_attribution(&mut self, event_loop: &ActiveEventLoop, updates: u32) {
        let pane = self.role_panes.first().copied();
        let counting = self.counters_mode == CountersMode::On;
        // The pane's state is read only once `start` has checked the API and the counters gate.
        let started = attribution::start(counting, attribution::API_ENABLED, || {
            pane.and_then(|pane_id| attribution::api::read_sync(&self.app, pane_id))
        });
        match (pane, started) {
            (_, Start::Skip(reason)) => {
                self.attribution = Some(attribution::Attribution::Unavailable { reason });
            }
            (_, Start::Refuse(reason)) => self.invalidate(event_loop, reason),
            (Some(pane_id), Start::Arm(reading)) => {
                let sentinel = self.sentinels.first().cloned().unwrap_or_default();
                let outcome = attribution::api::arm(
                    &mut self.app,
                    pane_id,
                    &sentinel,
                    workload::PROMPT,
                    updates,
                );
                let seq_start = self.frame_count();
                self.attribution =
                    Some(attribution::armed(outcome, pane_id, updates, reading, seq_start));
            }
            (None, Start::Arm(_)) => {
                // When: `pane` is None, no role pane was read, so `start` cannot have armed; kept total.
                self.attribution =
                    Some(attribution::Attribution::Unavailable { reason: "no-baseline" });
            }
        }
    }

    /// At the end of the counted phase: record the presented count and the pane's state, then disarm.
    fn end_attribution(&mut self) {
        let Some(attribution::Attribution::Armed(record)) = self.attribution.as_ref() else {
            // When: `attribution` holds no armed record, nothing was armed and nothing is read.
            return;
        };
        let pane_id = record.pane;
        let seq_end = self.frame_count();
        let end = attribution::api::read_sync(&self.app, pane_id);
        attribution::api::disarm(&mut self.app, pane_id);
        if let Some(attribution::Attribution::Armed(record)) = self.attribution.as_mut() {
            record.seq_end = Some(seq_end);
            record.end = end;
        }
    }

    /// Open the App's counter gate for a `--counters` run, before any window or pane exists.
    /// Then the App's gate must match the reported mode, or the run is void.
    #[cfg(feature = "perf-counters")]
    fn enable_counters(&mut self) -> Result<(), String> {
        if self.counters_mode == CountersMode::On {
            // the run counts, so the gate is forced on before the first window.
            crate::counters::enable(&mut self.app)?;
        }
        let gate_on = crate::counters::gate_on(&self.app);
        if let Some(reason) = crate::counters::gate_mismatch(self.counters_mode, gate_on) {
            // the App's gate disagrees with the report, so the run's numbers are not its own.
            self.counter_error.get_or_insert(reason);
        }
        Ok(())
    }

    /// Without the counter API, `--counters` was already refused while parsing.
    #[cfg(not(feature = "perf-counters"))]
    fn enable_counters(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// The App's counter totals now, when this run counts. A snapshot the contract cannot hold
    /// becomes the run's invalid reason instead of a reported value.
    #[cfg(feature = "perf-counters")]
    fn counter_totals(&mut self) -> Option<CounterTotals> {
        if self.counters_mode != CountersMode::On {
            // When: `counters_mode` is not On, this run reports no counters.
            return None;
        }
        match crate::counters::snapshot_totals(&self.app)? {
            Ok(totals) => Some(totals),
            Err(reason) => {
                self.counter_error.get_or_insert(reason);
                None
            }
        }
    }

    /// Without the counter API a run reports no counters.
    #[cfg(not(feature = "perf-counters"))]
    fn counter_totals(&mut self) -> Option<CounterTotals> {
        None
    }

    /// The run's result; a run that ended early keeps its partial phases and samples.
    fn result(&mut self, settled: bool) -> RunResult {
        self.close_sample(UnattributedReason::NoCandidate);
        self.finish_meter();
        let (mut status, mut invalid_reason) = self.outcome.take().unwrap_or_else(|| {
            (Status::Invalid, Some("the event loop ended before the run finished".to_owned()))
        });
        if let (Status::Valid, Some(reason)) = (status, self.counter_error.take()) {
            // a counter snapshot the contract cannot hold voids the run rather than report a guess.
            status = Status::Invalid;
            invalid_reason = Some(format!("frame counters: {reason}"));
        }
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
            trim_hook: self.trim_hook,
            trim_experiment: self.plan.trim_experiment,
            trim_seq_after_hook: self.trim_seq_after_hook,
            atlas_recovery: self.atlas_recovery.clone(),
            native_focus_events_dropped: self.native_focus_dropped,
            native_cursor_rest_events_dropped: self.native_pointer.rest_dropped(),
            finish_session_settled: settled,
            frame_counters: self.counters_mode,
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
            notes.push(if waits::occlusion_wait_applies(scenarios::BUILD_HOST) {
                "No native occlusion change arrived within 2 s, so a synthetic one was delivered."
                    .to_owned()
            } else {
                "This host reports no occlusion, so the trim experiment delivered Occluded(true) \
                 and Occluded(false) synthetically."
                    .to_owned()
            });
        }
        if let Some(note) = &self.foreground_lock_failure {
            notes.push(note.clone());
        }
        notes
    }
}

impl Probe {
    /// End startup at the warm-pool barrier, record the grid and the display, then focus.
    fn end_startup(&mut self, event_loop: &ActiveEventLoop) {
        self.finish_meter();
        self.record_progress();
        let Some(pane) = self.active_pane() else {
            self.invalidate(event_loop, "no active pane after startup".to_owned());
            return;
        };
        self.grid = self.app.__test_pane_grid_size(pane);
        self.monitor = self.monitor_info();
        // Every host records how it presented, so a macOS row can show it stayed on the hardware path.
        self.presenter = self.presenter_record();
        let blocked = self.presenter.as_ref().and_then(|presenter| {
            presenter_blocked(self.plan.presentation, presenter, scenarios::BUILD_HOST)
        });
        if let Some(reason) = blocked {
            // When: the window did not present through its variant's presenter, the run measures nothing it names.
            self.finish(event_loop, Status::Blocked, Some(reason));
            return;
        }
        if scenarios::BUILD_HOST == Host::Windows {
            // When: only Windows judges S11 by the image atlas, so only it reads the atlas's size here.
            self.image_atlas_start = self.image_atlas_bytes();
        }
        self.role_panes.push(pane);
        if self.recorded_exit_invalidates(event_loop, pane) {
            // When: the first role's program exited before its pane was recorded, the run is already invalid.
            return;
        }
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
        Some(presenter_record_for(
            scenarios::BUILD_HOST,
            self.software_render_mode,
            renderer.is_software_rendering(),
            renderer.is_software_render_degraded(),
        ))
    }

    /// Whether `pane`, just recorded as a role pane, had already exited; if so the run is invalidated.
    fn recorded_exit_invalidates(&mut self, event_loop: &ActiveEventLoop, pane: u64) -> bool {
        let exit = self.exited_panes.iter().find(|(exited, _)| *exited == pane).copied();
        let reason = exit.and_then(|(pane_id, was_clean)| {
            let finished = self.outcome.is_some();
            waits::role_exit_reason(pane_id, was_clean, &self.role_panes, true, finished)
        });
        let Some(reason) = reason else {
            // When: the pane has not exited, or the run already has its outcome, nothing changes.
            return false;
        };
        self.invalidate(event_loop, reason);
        true
    }

    /// The main renderer's retained image-atlas bytes; `None` before a renderer exists.
    fn image_atlas_bytes(&self) -> Option<usize> {
        Some(self.app.main_renderer()?.retained_amounts().image_atlas.bytes)
    }

    /// Whether the image atlas grew since startup ended; `None` when either reading is missing.
    fn image_atlas_grew(&self) -> Option<bool> {
        let start = self.image_atlas_start?;
        Some(self.image_atlas_bytes()? > start)
    }

    /// When S11's image must have registered on Windows: `IMAGE_REGISTER_WAIT` after its phase began.
    fn image_register_deadline(&self) -> Instant {
        self.image.started.map_or(self.run_deadline, |started| started + IMAGE_REGISTER_WAIT)
    }

    /// What a print phase's roles failed to deliver, on Windows: S3's rows and S9's wide tokens.
    fn delivery_problem(&self) -> Option<String> {
        let checked: Vec<(usize, Workload)> = self
            .sentinel_roles
            .iter()
            .filter_map(|role| self.plan.roles.get(*role).map(|workload| (*role, *workload)))
            .filter(|(_, workload)| {
                matches!(
                    workload,
                    Workload::Flood { .. } | Workload::PrintThenShell(Fixture::EmojiCjk)
                )
            })
            .collect();
        if checked.is_empty() {
            // When: no checked workload ran, the fixtures need not be generated.
            return None;
        }
        let files = workload::fixtures(&self.plan);
        checked.into_iter().find_map(|(role, workload)| match workload {
            Workload::Flood { lines, .. } => self.flood_problem(role, lines, &files),
            _ => self.wide_token_problem(role, &files),
        })
    }

    /// S3: whether the rows between READY and the sentinel are exactly the planned lines, and the
    /// retained rows above the sentinel are the end of `bulk.txt`.
    fn flood_problem(
        &self,
        role: usize,
        lines: u32,
        files: &[workload::FixtureFile],
    ) -> Option<String> {
        let bulk = files.iter().find(|file| file.relative_path == "bulk.txt");
        let Some(workload::FixtureBody::Repeated { block, count }) = bulk.map(|file| &file.body)
        else {
            return Some(format!("role {role}: S3 has no repeated bulk.txt fixture to count"));
        };
        let (Some(ready), Some(sentinel)) = (self.ready_rows[role], self.sentinel_rows[role])
        else {
            return Some(format!("role {role}'s READY or sentinel row was never located, so its rows cannot be counted"));
        };
        // bulk.txt repeats one block, so its lines, and its end, are that block's.
        let text = String::from_utf8_lossy(block);
        let block_lines: Vec<&str> = text.lines().collect();
        let tail: &[&str] = if *count == 0 {
            // When: no bulk block was printed, the rows above the sentinel are `y` lines, not bulk.txt's.
            &[]
        } else {
            &block_lines[block_lines.len().saturating_sub(BULK_TAIL_LINES)..]
        };
        // The count uses the pane's own width. Every line is ASCII, so its width is its length, and
        // a `y` line is one cell wide, so it takes one row at any width.
        let checked = self.read_role_grid(role, |grid| {
            let block_rows = planned_rows(block_lines.iter().map(|line| line.len()), grid.cols);
            let planned = u64::from(lines) + block_rows * *count as u64;
            row_count_mismatch(ready, sentinel, planned)
                .or_else(|| bulk_tail_mismatch(grid, sentinel, tail))
        });
        match checked {
            None => Some(format!("role {role}'s grid could not be read to count its rows")),
            Some(reason) => reason.map(|reason| format!("role {role}: {reason}")),
        }
    }

    /// S9: whether the retained rows hold every wide token the emoji-cjk fixture printed.
    fn wide_token_problem(&self, role: usize, files: &[workload::FixtureFile]) -> Option<String> {
        // emoji-cjk.txt is the file Fixture::EmojiCjk is written to.
        let file = files.iter().find(|file| file.relative_path == "emoji-cjk.txt");
        let Some(workload::FixtureBody::Bytes(bytes)) = file.map(|file| &file.body) else {
            return Some(format!("role {role}: S9 has no emoji-cjk.txt fixture to check"));
        };
        let text = String::from_utf8_lossy(bytes);
        let tokens = wide_tokens(&text);
        let missing = self.read_role_grid(role, |grid| {
            let retained = retained_text(grid);
            missing_wide_tokens(&retained, &tokens)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        match missing {
            None => Some(format!("role {role}'s grid could not be read to check its wide tokens")),
            Some(missing) if missing.is_empty() => None,
            Some(missing) => Some(format!(
                "role {role}'s grid lacks {} wide token(s) its fixture printed: {}",
                missing.len(),
                missing.join(" ")
            )),
        }
    }

    /// Read `role`'s grid outside every measured phase, retrying a busy parser briefly; `None` when
    /// the pane is gone or its parser stayed busy.
    fn read_role_grid<Output>(
        &self,
        role: usize,
        read: impl Fn(&Grid) -> Output,
    ) -> Option<Output> {
        for _ in 0..GRID_READ_ATTEMPTS {
            if let Some(output) = self.role_grid(role, &read) {
                return Some(output);
            }
            std::thread::sleep(GRID_READ_PAUSE);
        }
        None
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
                    if self.recorded_exit_invalidates(event_loop, pane) {
                        // When: the new role's program exited before its pane was recorded, the run is invalid.
                        return;
                    }
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
            let found = self
                .role_grid(role, |grid| {
                    line_row_near_cursor(grid, &ready, protocol_rows(scenarios::BUILD_HOST))
                })
                .flatten();
            let Some(row) = found else {
                // When: a role's READY is not in its grid yet, setup keeps waiting for it.
                return;
            };
            // The first sighting is kept; a row's lifetime number never changes once printed.
            self.ready_rows[role].get_or_insert(row);
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
                    if self.image.role.is_some() {
                        let now = Instant::now();
                        let verdict = waits::image_verdict(
                            scenarios::BUILD_HOST,
                            self.image.present.seen(),
                            self.image_register_deadline(),
                            self.image_atlas_grew(),
                            self.image.present.progress(now),
                            now,
                        );
                        match verdict {
                            ImageVerdict::Blocked(reason) => {
                                // When: on Windows the image never registered, or the atlas never took it.
                                self.finish(event_loop, Status::Blocked, Some(reason));
                                return;
                            }
                            ImageVerdict::Invalid(reason) => {
                                self.invalidate(event_loop, reason);
                                return;
                            }
                            ImageVerdict::Valid | ImageVerdict::Waiting => {}
                        }
                    }
                    if let Some(reason) = self.expired_barrier_reason(phase, Instant::now()) {
                        // When: no qualifying frame presented within the barrier's own bound.
                        self.invalidate(event_loop, reason);
                        return;
                    }
                    if !self.phase_done(phase, Instant::now()) {
                        self.stage = Stage::Steps(index);
                        return;
                    }
                    self.end_phase(event_loop, phase);
                    if self.stage == Stage::Done {
                        // When: the phase's delivery check ended the run, no later step runs.
                        return;
                    }
                }
                Step::Act(act) => {
                    self.perform(event_loop, *act);
                    if self.stage == Stage::Done {
                        return;
                    }
                }
                Step::Checkpoint(label) => {
                    if !self.advance_checkpoint(event_loop, index, label) {
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

    /// Take one turn at the checkpoint step `step_index` (`label`): the first turn writes the request
    /// and the record; a managed run then waits up to `CHECKPOINT_WAIT`, without blocking the event
    /// loop, for the comparison script's `.done`, and a build with the checkpoint-memory hook samples
    /// memory until a sample is complete or its window runs out. With no `.done` by then the run
    /// ends invalid and the next phase never starts. Returns whether the plan may move on.
    fn advance_checkpoint(
        &mut self,
        event_loop: &ActiveEventLoop,
        step_index: usize,
        label: &'static str,
    ) -> bool {
        let ordinal = self.plan.steps[..step_index]
            .iter()
            .filter(|step| matches!(step, Step::Checkpoint(_)))
            .count();
        // The record's optional fields are read once, when the checkpoint is first reached.
        let (fresh_after_unix_s, frame_texture_bytes, completeness) = if self
            .checkpoint_pending
            .is_none()
        {
            let fresh_after_unix_s =
                self.plan.fresh_after.filter(|rule| rule.checkpoint == label).and_then(|rule| {
                    self.phase_ends.iter().rev().find(|(name, ..)| *name == rule.anchor).map(
                        |(_, _, ended_unix_s)| {
                            waits::fresh_after_unix_s(
                                *ended_unix_s,
                                Duration::from_millis(rule.delay_ms),
                            )
                        },
                    )
                });
            let completeness = takes_completeness(self.plan.scenario, label)
                .then(|| self.app.main_renderer().map(renderer_completeness))
                .flatten();
            (
                fresh_after_unix_s,
                checkpoint_frame_texture_bytes(label, self.frame_texture_extent()),
                completeness,
            )
        } else {
            // When: the checkpoint is pending, its record already holds these fields.
            (None, None, None)
        };
        let step = CheckpointStep {
            ordinal,
            label,
            unix_s: unix_now(),
            fresh_after_unix_s,
            frame_texture_bytes,
            completeness,
        };
        let clock = Instant::now;
        let exists = |path: &Path| path.exists();
        let site = CheckpointSite {
            scratch: &self.scratch,
            managed: self.request.managed,
            sampling: CHECKPOINT_MEMORY,
            footprint_wait: CHECKPOINT_WAIT,
            clock: &clock,
            exists: &exists,
        };
        let app = &mut self.app;
        // Each attempt's atlas reading is taken right after its memory line, with no frame between.
        let mut readings = Vec::new();
        let outcome = checkpoint_turn(
            &mut self.checkpoint_pending,
            &mut self.checkpoints,
            &site,
            &step,
            |index, label, attempt| {
                let complete = sample_checkpoint_memory(app, index, label, attempt);
                readings.push((index, crate::counters::atlas_reading(app, attempt)));
                complete
            },
        );
        attach_atlas_readings(&mut self.checkpoints, readings);
        let moves_on = plan_moves_on(&outcome);
        if let CheckpointOutcome::Invalid(reason) = outcome {
            self.invalidate(event_loop, reason);
        }
        moves_on
    }
}

impl Probe {
    /// Start a measured phase; the first one writes GO for every role just before it starts.
    fn begin_phase(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) {
        if let Some(updates) = phase.updates {
            self.begin_attribution(event_loop, updates);
            if self.stage == Stage::Done {
                return;
            }
        }
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
        let counters_start = self.counter_totals();
        let counting = self.allocation_counter.is_some();
        self.meter = Some(PhaseMeter::start(
            phase.name,
            phase.kind,
            phase.endpoint(),
            counting,
            counters_start,
            phase.updates,
        ));
        self.sentinel_roles = match &phase.end {
            PhaseEnd::Sentinels(roles) => roles.clone(),
            _ => Vec::new(),
        };
        let image_role = match phase.end {
            PhaseEnd::ImageRegistered(role) => Some(role),
            _ => None,
        };
        self.image =
            ImageState { role: image_role, started: Some(Instant::now()), ..ImageState::default() };
        // A barrier's baseline is the presented-frame count at its own act, just before the act runs.
        self.start_barrier(&phase.end, Instant::now());
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
        // A trim still waiting for the cover lapses with the phase that asked for it.
        self.service_trim(true);
        if matches!(&self.driver, DriverState::Drag(drag) if drag.pressed) {
            self.button(event_loop, ElementState::Released);
        }
        self.close_sample(UnattributedReason::NoCandidate);
        self.driver = DriverState::None;
        self.end_attribution();
        self.finish_meter();
        // A barrier phase ended when its qualifying frame presented, however late this runs.
        let now = Instant::now();
        let ended_at = self.barrier.and_then(|barrier| barrier.done_at()).unwrap_or(now);
        let ended_unix_s = unix_now() - now.saturating_duration_since(ended_at).as_secs_f64();
        self.phase_ends.push((phase.name, ended_at, ended_unix_s));
        self.barrier = None;
        tracing::info!(target: LOG_TARGET, phase = phase.name, "perf_scenarios phase ended");
        if let (Some(bytes), Some(go_at)) = (phase.throughput_bytes, self.go_at) {
            let seen =
                self.sentinel_roles.iter().filter_map(|role| self.sentinel_seen[*role]).max();
            if let Some(seen) = seen {
                let seconds = seen.saturating_duration_since(go_at).as_secs_f64();
                self.throughput = Some(Throughput { bytes, seconds });
            }
        }
        if scenarios::BUILD_HOST == Host::Windows {
            if let Some(reason) = self.delivery_problem() {
                // When: ConPTY did not deliver what the role printed, the phase measured something else.
                self.finish(event_loop, Status::Blocked, Some(reason));
                return;
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
            PhaseEnd::MediaFree | PhaseEnd::Reshow => {
                self.barrier.is_some_and(|barrier| barrier.progress(now) == BarrierProgress::Done)
            }
            PhaseEnd::HoldFrom { .. } => {
                self.phase_deadline(phase).is_some_and(|deadline| now >= deadline)
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
                DriverState::AtlasRetry(retry) => {
                    retry.machine.is_done() && retry.failure.is_none()
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
            // Windows also wakes at the registration bound, since an image that never registers
            // leaves nothing else to wake the loop before the run's deadline.
            PhaseEnd::ImageRegistered(_) => self.image.present.deadline().or_else(|| {
                (scenarios::BUILD_HOST == Host::Windows).then(|| self.image_register_deadline())
            }),
            PhaseEnd::MediaFree | PhaseEnd::Reshow | PhaseEnd::HoldFrom { .. } => {
                let started = self.meter.as_ref().map_or_else(Instant::now, |meter| meter.started);
                let anchor = match &phase.end {
                    PhaseEnd::HoldFrom { anchor, .. } => self.phase_end_instant(anchor),
                    _ => None,
                };
                barrier_phase_deadline(&phase.end, self.barrier.as_ref(), started, anchor)
            }
            PhaseEnd::Sentinels(_) | PhaseEnd::DriverDone => None,
        }
    }

    /// When the most recent phase named `name` ended, if one has.
    fn phase_end_instant(&self, name: &str) -> Option<Instant> {
        self.phase_ends.iter().rev().find(|(ended, ..)| *ended == name).map(|(_, at, _)| *at)
    }

    /// Items in the main renderer's image atlas, for the reshow barrier; capacity is never read.
    fn image_atlas_items(&self) -> usize {
        self.image_atlas_amount().items
    }

    /// The main renderer's image atlas reading: its capacity in bytes and the images it holds.
    fn image_atlas_amount(&self) -> ResourceAmount {
        #[cfg(test)]
        if let Some((_, amount)) = self.test_readings {
            // When: a test stands in for the renderer, its atlas reading is the reading.
            return amount;
        }
        self.app.main_renderer().map_or_else(ResourceAmount::default, |renderer| {
            renderer.retained_amounts().image_atlas
        })
    }

    /// Start `end`'s frame barrier at its act, `act_at`, with the renderer's presented frames as its
    /// baseline; a phase without one clears the barrier.
    fn start_barrier(&mut self, end: &PhaseEnd, act_at: Instant) {
        let barrier_wait = match end {
            PhaseEnd::MediaFree => Some((waits::MEDIA_FREE_WAIT, false)),
            PhaseEnd::Reshow => Some((waits::RESHOW_WAIT, true)),
            _ => None,
        };
        self.barrier = barrier_wait.map(|(wait, needs_image_item)| {
            FrameBarrier::new(act_at, self.frame_count(), wait, needs_image_item)
        });
    }

    /// Report a presenting dispatch that ended at `ended` to the running barrier, with the renderer's
    /// frame count and image atlas items.
    fn observe_barrier_frame(&mut self, ended: Instant) {
        if self.barrier.is_none() {
            // When: no barrier phase runs, the atlas is not read.
            return;
        }
        let frames = self.frame_count();
        let image_items = self.image_atlas_items();
        if let Some(barrier) = self.barrier.as_mut() {
            barrier.observe(ended, frames, image_items);
        }
    }

    /// The invalid reason when `phase`'s barrier has expired at `now`, else `None`.
    fn expired_barrier_reason(&self, phase: &PhaseSpec, now: Instant) -> Option<String> {
        let barrier = self.barrier?;
        (barrier.progress(now) == BarrierProgress::Expired)
            .then(|| waits::barrier_expired_reason(phase.name, barrier.wait()))
    }

    /// The main renderer's frame texture extent, for S11's `end` checkpoint.
    #[cfg(feature = "perf-frame-texture")]
    fn frame_texture_extent(&self) -> Option<(u32, u32)> {
        if self.plan.scenario != "S11" {
            // When: another scenario runs, its `end` reports no frame texture.
            return None;
        }
        self.app.main_renderer().map(GpuRenderer::frame_texture_extent)
    }

    /// Without the feature the tree's renderer may have no extent reading, so none is taken.
    #[cfg(not(feature = "perf-frame-texture"))]
    fn frame_texture_extent(&self) -> Option<(u32, u32)> {
        None
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
            Act::Uncover => {
                self.uncover();
                if !waits::occlusion_wait_applies(scenarios::BUILD_HOST)
                    && self.occlusion.delivered == Some(true)
                {
                    // When: this host reports no occlusion and the trim delivered `Occluded(true)`, the
                    // probe reveals the window to the App itself, which starts the uncover time.
                    self.deliver_occlusion(event_loop, false);
                }
            }
            Act::TrimCovered => self.request_trim(event_loop),
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
        self.finish_meter();
        self.cover = None;
        self.checkpoint_pending = None;
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
                    identity: RowIdentity::default(),
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
            Driver::AtlasRetry => self.start_atlas_retry(event_loop),
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

    /// Start S1/atlas-retry's driver: settle the scene first. It measures only the counters set,
    /// so a run without counters is refused rather than measured without them.
    fn start_atlas_retry(&mut self, event_loop: &ActiveEventLoop) -> DriverState {
        if !cfg!(perf_atlas_retry_api) {
            // When: the build lacks `perf_atlas_retry_api`, its App cannot drive the episodes: blocked, not invalid.
            self.finish(event_loop, Status::Blocked, Some(ATLAS_RETRY_UNAVAILABLE.to_owned()));
            return DriverState::None;
        }
        if self.counters_mode != CountersMode::On {
            // When: counters are off or unsupported, the episodes cannot be observed at all.
            self.invalidate(event_loop, "S1/atlas-retry runs only with --counters".to_owned());
            return DriverState::None;
        }
        let now = Instant::now();
        let mut retry = AtlasRetryDriver {
            machine: RecoveryEpisodes::new(now),
            redraw_pending: false,
            armed_at: now,
            failure: None,
            started: now,
        };
        self.arm_atlas_retry(&mut retry, Arm::Redraw);
        DriverState::AtlasRetry(Box::new(retry))
    }

    /// Arm the main renderer for the next frame of `retry`'s machine, at once, before another
    /// dispatch is forwarded.
    fn arm_atlas_retry(&mut self, retry: &mut AtlasRetryDriver, arm: Arm) {
        let Some(renderer) = self.app.main_renderer_mut() else {
            // When: there is no main renderer, nothing can be armed or measured.
            retry.failure = Some("no main renderer to arm".to_owned());
            return;
        };
        if arm == Arm::ChangeAtlas {
            // When: frame A is next, its assembly changes the atlas so the frame retries.
            change_atlas_during_next_assembly(renderer);
        }
        renderer.invalidate_retained_frame();
        // B is redrawn by the retry's own request; every other frame asks for one.
        retry.redraw_pending = arm != Arm::InvalidateOnly;
        retry.armed_at = Instant::now();
    }

    /// Feed one forwarded dispatch to the episode machine: the counts it moved from `before`, the
    /// scene read before it (`scene_before`) and after it, and `now`, when it completed. The
    /// machine qualifies the scene; the probe only reads it.
    fn observe_atlas_retry(
        &mut self,
        before: Option<Counts>,
        scene_before: Option<Scene>,
        cached_before: Option<atlas_retry::AppliedSample>,
        now: Instant,
    ) {
        if !matches!(self.driver, DriverState::AtlasRetry(_)) {
            // When: the driver ended during the dispatch, there is no machine to feed.
            return;
        }
        let after = atlas_retry_counts(&self.app);
        let delta = before.zip(after).map(|(earlier, later)| later.since(earlier));
        // Each side's applied foreground sample is read with that side's scene, at the same boundary.
        let reading = SceneReading {
            before: scene_before,
            after: self.atlas_retry_scene(),
            cached_before,
            cached_after: self.atlas_retry_cached(),
        };
        let DriverState::AtlasRetry(mut retry) =
            std::mem::replace(&mut self.driver, DriverState::None)
        else {
            return;
        };
        let was_settled = retry.machine.scene().is_some();
        let progress = retry.machine.observe(delta, &reading, now);
        if let (false, Some(settled)) = (was_settled, retry.machine.scene()) {
            // When: this dispatch settled the scene, its identity is logged once, before the episodes.
            let settle_ms = u64::try_from(now.saturating_duration_since(retry.started).as_millis())
                .unwrap_or(u64::MAX);
            let observed = atlas_retry::settle_observation(settled, &reading);
            self.log_atlas_retry_evidence("settle", observed, retry.started, Some(settle_ms));
        }
        match progress {
            Progress::Waiting => {}
            Progress::Arm(arm) => self.arm_atlas_retry(&mut retry, arm),
            Progress::Done => match atlas_retry::records_problem(retry.machine.records()) {
                Some(problem) => retry.failure = Some(problem),
                None => {
                    let distinct = retry
                        .machine
                        .scene()
                        .map_or(0, |scene| atlas_retry::distinct_keys(&scene.rows));
                    self.atlas_recovery =
                        Some(atlas_retry::recovery_json(retry.machine.records(), distinct));
                }
            },
            Progress::Invalid(reason) => retry.failure = Some(reason),
        }
        self.driver = DriverState::AtlasRetry(retry);
    }

    /// End the run on a failure or an expired step, else forward a pending redraw request.
    fn drive_atlas_retry(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        let DriverState::AtlasRetry(retry) = &mut self.driver else {
            return;
        };
        if let Some(reason) = retry.failure.take().or_else(|| retry.machine.expire(now)) {
            // When: the run failed or a step was not attempted in time, it cannot be read.
            // The failure line comes from the reading the machine kept when it refused it, never a reread.
            let observed = atlas_retry::failure_observation(&retry.machine);
            let started = retry.started;
            self.log_atlas_retry_evidence("failure", observed, started, None);
            self.invalidate(event_loop, format!("S1/atlas-retry: {reason}"));
            return;
        }
        if !std::mem::take(&mut retry.redraw_pending) {
            // When: no redraw is pending, the drive waits for the step's attempt.
            return;
        }
        let Some(window_id) = self.main_id else {
            return;
        };
        self.forward(event_loop, Dispatch::Harness, |app, active| {
            app.user_event(active, UserEvent::RequestRedraw(window_id))
        });
    }

    /// Log S1/atlas-retry's identity line for `event` from `observed`, the reading the judge saw, with its
    /// titles and the App's applied foreground sample as they were then; nothing of either is reread here.
    /// The line adds the role session (a bounded file read), the cumulative title and foreground counters
    /// read now, and one foreground lookup the harness makes itself, labelled apart with its own time. The
    /// judge's rules are unchanged, but this runs on the event loop: the lookup is a synchronous native
    /// process-table walk with no latency bound, and a failed run makes two (settle and failure).
    fn log_atlas_retry_evidence(
        &self,
        event: &'static str,
        observed: atlas_retry::Observed,
        started: Instant,
        settle_ms: Option<u64>,
    ) {
        let app_cached = observed.cached.map(|sample| atlas_retry::CachedForeground {
            process: sample.process,
            sampled_rel_ms: atlas_retry::relative_ms(sample.sampled_at, started),
        });
        let session = atlas_retry::read_session(&self.scratch.join("sessions/0.json"));
        let (tab_title_prepares, fg_worker_probes, fg_results_stale) =
            atlas_retry_title_counters(&self.app);
        let harness_lookup = session.map(|identity| {
            let process = sonicterm_io::proc_info::foreground_process(identity.leader_pid);
            atlas_retry::HarnessLookup {
                process,
                observed_rel_ms: atlas_retry::relative_ms(Instant::now(), started),
            }
        });
        let evidence = atlas_retry::Evidence {
            event,
            observation: observed.observation,
            settled_title: observed.settled_title,
            read_title: observed.read_title,
            app_cached,
            session,
            tab_title_prepares,
            fg_worker_probes,
            fg_results_stale,
            settle_ms,
            harness_lookup,
        };
        tracing::info!(target: LOG_TARGET, "perf_scenarios atlas-retry evidence {}", evidence.line());
    }

    /// The foreground sample the App has applied for the active pane now, read alongside a scene reading;
    /// `None` when the pane has no cache entry or cannot be found.
    fn atlas_retry_cached(&self) -> Option<atlas_retry::AppliedSample> {
        let pane = self.active_pane()?;
        let (sampled_at, process) = self.app.main_panes()?.get(&pane)?.fg_proc_cache.clone()?;
        Some(atlas_retry::AppliedSample { sampled_at, process: process.map(|found| found.name) })
    }

    /// What the main window shows for S1/atlas-retry: the active tab's title, the font fallback
    /// state, the active pane's grid size, cursor and visible rows (trailing blanks trimmed).
    /// `None` when any part cannot be read. The title, notice id and chrome reads exist only in a
    /// tree with `perf_atlas_retry_api`; without it they read `None`, and the driver never starts.
    fn atlas_retry_scene(&self) -> Option<Scene> {
        let window_id = self.main_id?;
        let (title, notice_id, missing) = atlas_retry_scene_parts(&self.app, window_id)?;
        let fallback = (notice_id, atlas_retry_fallback_applies(&self.app)?, missing);
        let pane = self.active_pane()?;
        let state = self.app.main_panes()?.get(&pane)?;
        let parser = state.parser.lock();
        let grid = parser.grid();
        let rows = (0..grid.rows)
            .map(|row| {
                let text: String = grid.row(row).iter().map(|cell| cell.ch).collect();
                text.trim_end().to_owned()
            })
            .collect();
        Some(Scene {
            title,
            fallback,
            grid: (grid.cols, grid.rows),
            cursor: (grid.cursor.row, grid.cursor.col),
            rows,
        })
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
            DriverState::AtlasRetry(retry) => {
                if retry.failure.is_some() || retry.redraw_pending {
                    // When: a failure or a redraw waits, the drive is due from when it was armed.
                    Some(retry.armed_at)
                } else {
                    retry.machine.deadline()
                }
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
            DriverState::AtlasRetry(_) => self.drive_atlas_retry(event_loop, now),
        }
    }

    /// Type the next character: the previous sample closes and this character's sample opens.
    fn inject_typing(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(character) = self.open_typing_sample(Instant::now()) {
            self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(character.to_string())));
        }
    }

    /// Close the previous sample and open the next character's, injected at `injected`, arming its
    /// echo watch only for S2/default; returns the character to type, or `None` when typing is done
    /// or the prompt is not yet found. Needs no event loop.
    fn open_typing_sample(&mut self, injected: Instant) -> Option<char> {
        let DriverState::Typing(typing) = &mut self.driver else {
            return None;
        };
        let origin = typing.origin?;
        if typing.typed >= typing.chars {
            return None;
        }
        let target = echo_target(origin, typing.cols, typing.typed);
        let (pane, identity) = (typing.pane, typing.identity);
        typing.typed += 1;
        if typing.typed == typing.chars {
            typing.done_at = Some(injected);
        }
        // A sample with no candidate by the next injection is unattributed.
        self.close_sample(UnattributedReason::NoCandidate);
        // Every parser guard is released here, and the character is not yet injected.
        let arm = if split_in_scope(self.plan.scenario, self.plan.variant) {
            echo_api::arm(&mut self.app, pane, &target, identity)
        } else {
            // When: the plan is not S2/default, its samples are never armed and read unsupported.
            ArmState::OutOfScope
        };
        let inject_unix_s = unix_now();
        self.open_sample =
            Some(OpenSample { pane, target, injected, inject_unix_s, arm, identity });
        Some(target.character)
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

    /// Ask for the covered-window trim. On a host without native occlusion the probe delivers
    /// `Occluded(true)` itself first; elsewhere the trim waits for the native event or its fallback.
    /// Both sides of a comparison run this same protocol whatever their hook reports.
    fn request_trim(&mut self, event_loop: &ActiveEventLoop) {
        if self.cover.is_none() {
            // When: cover is None, no cover opened, so there is no covered window to trim.
            return;
        }
        self.trim_pending = true;
        if !waits::occlusion_wait_applies(scenarios::BUILD_HOST)
            && self.occlusion.delivered != Some(true)
        {
            // When: this host reports no occlusion, the App learns of the cover only from the probe.
            self.synthetic_occlusion = true;
            self.deliver_occlusion(event_loop, true);
        }
        self.service_trim(false);
    }

    /// Call the trim hook on the first turn the App holds `Occluded(true)`, once; when the covered
    /// hold ends first (`hold_over`), the hook stays unreached. It never changes the plan's steps.
    fn service_trim(&mut self, hold_over: bool) {
        match waits::trim_dispatch(self.trim_pending, self.occlusion.delivered, hold_over) {
            TrimDispatch::Idle | TrimDispatch::Wait => {}
            TrimDispatch::Call => {
                self.trim_pending = false;
                if let Some(window_id) = self.main_id {
                    // When: main_id names the measurement window, the hook is asked about it.
                    (self.trim_hook, self.trim_seq_after_hook) =
                        trim_covered(&mut self.app, window_id);
                }
            }
            TrimDispatch::Lapsed => {
                self.trim_pending = false;
                self.trim_hook = TrimHookOutcome::NotReached;
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
        if waits::occlusion_wait_applies(scenarios::BUILD_HOST) {
            // When: the host reports occlusion natively (macOS), the 2 s fallback covers a missing event.
            // Windows reports none: only the trim experiment delivers occlusion there, itself.
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
            Stage::Steps(_) => steps_wake(
                self.checkpoint_pending.as_ref(),
                now,
                [
                    self.driver_due(),
                    self.current_phase().and_then(|phase| self.phase_deadline(phase)),
                    self.scan.trailing(),
                    self.occlusion.wait.map(|(_, at)| at),
                ],
                self.run_deadline,
            ),
        };
        Some(deadline.min(self.run_deadline))
    }
}

/// When a barrier or anchored-hold phase ends by the clock: a pending barrier's bound (a met one adds
/// none), or a hold counted from `anchor` (else from `started`). Other phases are not handled here.
fn barrier_phase_deadline(
    end: &PhaseEnd,
    barrier: Option<&FrameBarrier>,
    started: Instant,
    anchor: Option<Instant>,
) -> Option<Instant> {
    match end {
        PhaseEnd::MediaFree | PhaseEnd::Reshow => barrier.and_then(FrameBarrier::deadline),
        PhaseEnd::HoldFrom { hold_ms, .. } => {
            Some(waits::anchored_hold_end(anchor, started, *hold_ms))
        }
        _ => None,
    }
}

/// The main renderer's glyph completeness checkpoint, in a build with `perf_completeness_api`.
#[cfg(perf_completeness_api)]
fn renderer_completeness(renderer: &GpuRenderer) -> CompletenessRecord {
    let scale = f64::from(renderer.scale_factor());
    completeness_from_checkpoint(renderer.completeness_checkpoint(), scale)
}

/// Without `perf_completeness_api` the renderer may have no checkpoint, so none is read.
#[cfg(not(perf_completeness_api))]
fn renderer_completeness(renderer: &GpuRenderer) -> CompletenessRecord {
    crate::record::completeness_api_disabled(f64::from(renderer.scale_factor()))
}

/// The frame texture's bytes for a checkpoint: width x height x 4 at `end`, only with
/// `perf-frame-texture`; any other checkpoint, or a build without the feature, records none.
fn checkpoint_frame_texture_bytes(label: &str, extent: Option<(u32, u32)>) -> Option<u64> {
    if !cfg!(feature = "perf-frame-texture") || label != "end" {
        // When: the feature is off or the label is not `end`, the field is left out.
        return None;
    }
    extent.map(|(width, height)| u64::from(width) * u64::from(height) * 4)
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod probe_tests;
