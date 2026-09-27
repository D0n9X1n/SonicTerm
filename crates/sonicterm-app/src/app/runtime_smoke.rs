use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sonicterm_gpu::device_errors::{DeviceErrorSnapshot, DeviceState, GpuFaultKind};

use sonicterm_grid::grid::Grid;

#[path = "gpu_recovery_smoke.rs"]
mod gpu_recovery_smoke;

/// Release-smoke failure boundary and its stable process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSmokeFailure {
    /// The winit event loop could not be constructed or run.
    EventLoop,
    /// The native display server could not create a window.
    Display,
    /// GPU initialization did not produce a renderer and device.
    Gpu,
    /// The platform-provided shell did not spawn as the active pane PTY.
    Pty,
    /// The shell marker could not be queued or observed in the grid.
    Marker,
    /// A marker-bearing frame did not reach native presentation.
    Present,
    /// The default warm renderer was not created, reported, adopted, or released.
    WarmLifecycle,
    /// GPU fault containment or shell liveness after a fault was not proven.
    GpuFaultContainment,
    /// Intentional device loss or shell liveness after loss was not proven.
    GpuDeviceLoss,
    /// Shared-device recovery, subsequent presentation, or shell survival was not proven.
    GpuDeviceRecovery,
    /// Owned PTY teardown did not settle before the bounded session shutdown completed.
    NativeTeardown,
}

impl RuntimeSmokeFailure {
    /// Stable exit code consumed by native package smoke jobs.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::EventLoop => 10,
            Self::Display => 11,
            Self::Gpu => 12,
            Self::Pty => 13,
            Self::Marker => 14,
            Self::Present => 15,
            Self::WarmLifecycle => 16,
            Self::GpuFaultContainment => 17,
            Self::GpuDeviceLoss => 18,
            Self::GpuDeviceRecovery => 19,
            Self::NativeTeardown => 20,
        }
    }
}

impl std::fmt::Display for RuntimeSmokeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let boundary = match self {
            Self::EventLoop => "event loop",
            Self::Display => "display/window",
            Self::Gpu => "GPU renderer",
            Self::Pty => "platform shell PTY",
            Self::Marker => "PTY marker",
            Self::Present => "frame presentation",
            Self::WarmLifecycle => "warm renderer lifecycle",
            Self::GpuFaultContainment => "GPU fault containment",
            Self::GpuDeviceLoss => "GPU device loss",
            Self::GpuDeviceRecovery => "shared GPU device recovery",
            Self::NativeTeardown => "native PTY teardown",
        };
        write!(formatter, "runtime smoke failed at {boundary}")
    }
}

impl std::error::Error for RuntimeSmokeFailure {}

/// Platform-provided inputs for one isolated native runtime smoke.
///
/// The executable and marker command remain platform-owned because Unix shell,
/// PowerShell, and `cmd.exe` use different quoting and expansion rules. Config
/// and log roots are explicit so the smoke never needs to replace `HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSmokeSpec {
    shell_program: String,
    marker: String,
    command: Vec<u8>,
    config_dir: PathBuf,
    log_dir: PathBuf,
    scenario: Option<RuntimeSmokeScenario>,
}

impl RuntimeSmokeSpec {
    /// Build a smoke contract after validating its proof and isolation boundaries.
    pub fn new(
        shell_program: impl Into<String>,
        marker: impl Into<String>,
        command: Vec<u8>,
        config_dir: PathBuf,
        log_dir: PathBuf,
    ) -> Result<Self, RuntimeSmokeFailure> {
        let shell_program = shell_program.into();
        let marker = marker.into();
        if shell_program.trim().is_empty()
            || marker.is_empty()
            || command.is_empty()
            || String::from_utf8_lossy(&command).contains(&marker)
            || config_dir.as_os_str().is_empty()
            || log_dir.as_os_str().is_empty()
            || config_dir == log_dir
        {
            // When: `shell_program`, `marker`, `command`, `config_dir`, or `log_dir` is invalid, reject the smoke contract.
            return Err(RuntimeSmokeFailure::Marker);
        }
        Ok(Self { shell_program, marker, command, config_dir, log_dir, scenario: None })
    }

    /// Pin a scenario selected by a platform's extra smoke-verdict checks.
    #[must_use]
    pub fn with_scenario(mut self, scenario: RuntimeSmokeScenario) -> Self {
        self.scenario = Some(scenario);
        self
    }

    /// Resolve the environment only when the platform has not already selected the scenario.
    pub(crate) fn selected_scenario(&self) -> Result<RuntimeSmokeScenario, RuntimeSmokeFailure> {
        self.scenario.map(Ok).unwrap_or_else(RuntimeSmokeScenario::from_environment)
    }

    /// Executable the platform selected for the smoke PTY.
    #[must_use]
    pub fn shell_program(&self) -> &str {
        &self.shell_program
    }

    /// Complete marker that must be observed in the live terminal grid.
    #[must_use]
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// Input bytes that make the shell construct the marker without echoing it literally.
    #[must_use]
    pub fn command(&self) -> &[u8] {
        &self.command
    }

    /// Scratch root reserved for config and reload operations.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Scratch root reserved for logs, breadcrumbs, and crash evidence.
    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }
}

/// Fault sequence selected only by the explicit runtime-smoke entry point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RuntimeSmokeScenario {
    /// Exercise warm lifecycle, isolated and retained-resource faults, then device loss.
    #[default]
    Default,
    /// Exercise a persistent frame fault in a separate process on a fresh device.
    FrameValidation,
    /// Recover one shared device across two windows and a warm renderer while preserving their shells.
    DeviceRecovery,
}

impl RuntimeSmokeScenario {
    /// Select the smoke-only scenario from the environment, rejecting invalid encodings and names.
    pub fn from_environment() -> Result<Self, RuntimeSmokeFailure> {
        match std::env::var("SONICTERM_RUNTIME_SMOKE_SCENARIO") {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(Self::Default),
            Err(std::env::VarError::NotUnicode(_)) => Err(RuntimeSmokeFailure::EventLoop),
        }
    }

    /// Reject unknown scenarios before constructing the event loop.
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, RuntimeSmokeFailure> {
        match value {
            None | Some("default") => Ok(Self::Default),
            Some("frame-validation") => Ok(Self::FrameValidation),
            Some("device-recovery") => Ok(Self::DeviceRecovery),
            _ => Err(RuntimeSmokeFailure::EventLoop),
        }
    }
}

/// Both totals matter: an unacknowledged present is still a containment failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SmokeFrameCounts {
    presents: u64,
    successful: u64,
}

/// Identity of a stopped RedrawRequested boundary, not an actual renderer call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoppedRedrawIdentity {
    window: winit::window::WindowId,
    generation: u64,
    state: DeviceState,
    destroy_requested: bool,
}

/// A monotonic observation on the exact stopped boundary required by a fault phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoppedRedrawObservation {
    identity: StoppedRedrawIdentity,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FaultBaseline {
    frames: SmokeFrameCounts,
    marker_rows: usize,
    render_attempts: u64,
    refusal: StoppedRedrawObservation,
}

// A fresh shell marker and a matching stopped-boundary observation are required for retained/loss proof.
// FrameValidation separately requires a real renderer call; refusals cannot spend that requirement.
const FAULT_OBSERVATION: Duration = Duration::from_millis(250);
const FAULT_DEADLINE: Duration = Duration::from_secs(5);
const FAULT_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeSmokePhase {
    Display,
    Gpu,
    Pty,
    Marker,
    Present { baseline: u64 },
    WarmCreate,
    WarmAdopt { child: winit::window::WindowId, baseline: u64 },
    WarmRelease { child: winit::window::WindowId },
    FreshCreate,
    FreshPresent { child: winit::window::WindowId, baseline: u64 },
    FreshRelease { child: winit::window::WindowId },
    FaultPrecondition,
    FaultIsolated { baseline: FaultBaseline },
    FaultRetainedReady,
    FaultRetained { baseline: FaultBaseline },
    FaultDestroyReady,
    FaultDestroy { baseline: FaultBaseline },
    FrameValidationReady,
    FrameValidationArmed { baseline: FaultBaseline },
    FrameValidationMarkerReady { baseline: FaultBaseline },
    FrameValidation { baseline: FaultBaseline, quiet_started: Instant },
    RecoveryReady,
    Complete,
}

/// State retained by the app while a bounded native package smoke is active.
pub(crate) struct RuntimeSmokeState {
    marker: String,
    command: Vec<u8>,
    phase: RuntimeSmokePhase,
    renderer_baseline: usize,
    verify_fresh_drop_target: bool,
    scenario: RuntimeSmokeScenario,
    recovery_probe: Option<gpu_recovery_smoke::RecoveryProbe>,
    render_attempts: u64,
    stopped_redraw_count: u64,
    expected_refusal: Option<StoppedRedrawIdentity>,
    stopped_redraw: Option<StoppedRedrawObservation>,
    fault_started: Option<Instant>,
    next_probe_at: Option<Instant>,
    outcome: Option<Result<(), RuntimeSmokeFailure>>,
}

impl RuntimeSmokeState {
    #[cfg(test)]
    pub(crate) fn new(nonce: u32) -> Self {
        let marker = format!("__SONICTERM_SMOKE_{nonce}__");
        let command = format!("printf '__SONICTERM_SMOKE_%s__\\n' '{nonce}'\n").into_bytes();
        debug_assert!(!String::from_utf8_lossy(&command).contains(&marker));
        Self {
            marker,
            command,
            phase: RuntimeSmokePhase::Display,
            renderer_baseline: sonicterm_gpu::core::live_renderer_count(),
            verify_fresh_drop_target: false,
            scenario: RuntimeSmokeScenario::Default,
            recovery_probe: None,
            render_attempts: 0,
            stopped_redraw_count: 0,
            expected_refusal: None,
            stopped_redraw: None,
            fault_started: None,
            next_probe_at: None,
            outcome: None,
        }
    }

    pub(crate) fn from_spec(spec: &RuntimeSmokeSpec, renderer_baseline: usize) -> Self {
        Self {
            marker: spec.marker.clone(),
            command: spec.command.clone(),
            phase: RuntimeSmokePhase::Display,
            renderer_baseline,
            verify_fresh_drop_target: false,
            scenario: RuntimeSmokeScenario::Default,
            recovery_probe: None,
            render_attempts: 0,
            stopped_redraw_count: 0,
            expected_refusal: None,
            stopped_redraw: None,
            fault_started: None,
            next_probe_at: None,
            outcome: None,
        }
    }

    pub(crate) fn marker(&self) -> &str {
        &self.marker
    }

    pub(crate) fn command(&self) -> &[u8] {
        &self.command
    }

    pub(crate) fn begin_gpu(&mut self) {
        self.phase = RuntimeSmokePhase::Gpu;
    }

    pub(crate) fn begin_pty(&mut self) {
        self.phase = RuntimeSmokePhase::Pty;
    }

    pub(crate) fn begin_marker_wait(&mut self) {
        self.phase = RuntimeSmokePhase::Marker;
    }

    pub(crate) fn is_waiting_for_marker(&self) -> bool {
        self.phase == RuntimeSmokePhase::Marker
    }

    pub(crate) fn begin_present_wait(&mut self, baseline: u64) {
        if self.phase == RuntimeSmokePhase::Marker {
            self.phase = RuntimeSmokePhase::Present { baseline };
        }
    }

    pub(crate) fn is_waiting_for_present(&self) -> bool {
        matches!(self.phase, RuntimeSmokePhase::Present { .. })
    }

    pub(crate) fn observe_presented_frame(&mut self, current: u64) -> bool {
        let RuntimeSmokePhase::Present { baseline } = self.phase else {
            // When: `self.phase` is not `RuntimeSmokePhase::Present`, no marker-bearing frame is pending.
            return false;
        };
        if current <= baseline {
            // When: `current` has not advanced past `baseline`, the marker-bearing frame was not presented.
            return false;
        }
        self.phase = match self.scenario {
            RuntimeSmokeScenario::Default => RuntimeSmokePhase::WarmCreate,
            RuntimeSmokeScenario::FrameValidation => RuntimeSmokePhase::FrameValidationReady,
            RuntimeSmokeScenario::DeviceRecovery => RuntimeSmokePhase::RecoveryReady,
        };
        true
    }

    /// Record a marker-bearing native present while the matching pane's parser guard remains held.
    pub(super) fn observe_recovery_frame(
        &mut self,
        window: winit::window::WindowId,
        generation: u64,
        panes: &[sonicterm_render_model::PaneRender<'_>],
        outcome: &sonicterm_gpu::core::PresentOutcome,
    ) {
        if let Some(probe) = self.recovery_probe.as_mut() {
            probe.observe_frame(window, generation, panes, outcome, &self.marker);
        }
    }

    /// Record the event-loop delivery of a queued old-generation wake during the recovery oracle.
    pub(super) fn observe_recovery_device_event(&mut self, generation: u64) {
        if let Some(probe) = self.recovery_probe.as_mut() {
            probe.observe_device_event(generation);
        }
    }

    /// Preserve watchdog identity even when startup consumed the recovery phase's remaining time.
    pub(super) fn fail_from_watchdog(&mut self, now: Instant) {
        if self.recovery_enabled() {
            if let Some(probe) = &self.recovery_probe {
                probe.report_timeout(now, "watchdog");
            } else {
                // When: `recovery_probe` is absent, the watchdog expired before the recovery phase began.
                tracing::error!(target: "sonic::gpu::recovery", cause = "watchdog", phase = ?self.phase, "runtime smoke recovery startup timeout");
            }
        }
        self.fail(self.timeout_failure());
    }

    /// Keep containment-only smokes stopped while allowing the explicit recovery oracle.
    pub(super) fn recovery_enabled(&self) -> bool {
        self.scenario == RuntimeSmokeScenario::DeviceRecovery
    }

    pub(crate) fn should_maintain_warm_pool(&self) -> bool {
        matches!(self.phase, RuntimeSmokePhase::WarmCreate)
    }

    pub(crate) fn renderer_baseline(&self) -> usize {
        self.renderer_baseline
    }

    pub(crate) fn is_waiting_for_adopted_present(&self, child: winit::window::WindowId) -> bool {
        matches!(
            self.phase,
            RuntimeSmokePhase::WarmAdopt { child: expected, .. }
                | RuntimeSmokePhase::FreshPresent { child: expected, .. } if expected == child
        )
    }

    pub(crate) fn needs_fresh_window(&self) -> bool {
        self.phase == RuntimeSmokePhase::FreshCreate
    }

    pub(crate) fn begin_fresh_window(
        &mut self,
        child: winit::window::WindowId,
        baseline: u64,
    ) -> bool {
        if !self.needs_fresh_window() {
            // When: needs_fresh_window is false, a new window cannot replace the pending smoke identity.
            return false;
        }
        self.phase = RuntimeSmokePhase::FreshPresent { child, baseline };
        true
    }

    pub(crate) fn begin_warm_adoption(
        &mut self,
        child: winit::window::WindowId,
        baseline: u64,
    ) -> bool {
        let RuntimeSmokePhase::WarmCreate = self.phase else {
            // When: `self.phase` is not `WarmCreate`, adoption cannot be credited.
            return false;
        };
        self.phase = RuntimeSmokePhase::WarmAdopt { child, baseline };
        true
    }

    pub(crate) fn observe_adopted_present(
        &mut self,
        child: winit::window::WindowId,
        current: u64,
    ) -> bool {
        let (expected, baseline, fresh) = match self.phase {
            RuntimeSmokePhase::WarmAdopt { child, baseline } => (child, baseline, false),
            RuntimeSmokePhase::FreshPresent { child, baseline } => (child, baseline, true),
            _ => {
                // When: phase has no child presentation pending, an unrelated frame cannot advance the smoke.
                return false;
            }
        };
        if child != expected || current <= baseline {
            // When: identity or frame count does not advance the named child, keep waiting.
            return false;
        }
        self.phase = if fresh {
            RuntimeSmokePhase::FreshRelease { child }
        } else {
            // When: fresh is false, only the warm-adopted child's release can advance the smoke.
            RuntimeSmokePhase::WarmRelease { child }
        };
        true
    }

    pub(crate) fn finish_warm_release(
        &mut self,
        child: winit::window::WindowId,
        released: bool,
    ) -> bool {
        if !released
            || !matches!(
                self.phase,
                RuntimeSmokePhase::WarmRelease { child: expected }
                    | RuntimeSmokePhase::FreshRelease { child: expected } if expected == child
            )
        {
            // When: the pending child was not the exact state released, fail closed at the window lifecycle.
            self.fail(RuntimeSmokeFailure::WarmLifecycle);
            return false;
        }
        if self.verify_fresh_drop_target
            && matches!(self.phase, RuntimeSmokePhase::WarmRelease { .. })
        {
            // A custom drop owner must also register an independently created HWND after warm adoption.
            self.phase = RuntimeSmokePhase::FreshCreate;
        } else {
            // When: verify_fresh_drop_target is false or phase is FreshRelease, fault proof still remains.
            self.phase = RuntimeSmokePhase::FaultPrecondition;
        }
        true
    }

    /// Count a real renderer call, including one that discovers a stop; pre-render refusals do not count.
    pub(crate) fn note_render_attempt(&mut self) {
        self.render_attempts = self.render_attempts.saturating_add(1);
    }

    /// Observe an actual stopped redraw boundary even after its one-time error report was consumed.
    ///
    /// Only runtime smoke calls this. Unrelated windows or device states cannot replace
    /// matching proof, and this never counts as a renderer invocation.
    pub(crate) fn note_stopped_redraw_refusal(
        &mut self,
        window: winit::window::WindowId,
        snapshot: &DeviceErrorSnapshot,
    ) {
        if snapshot.state == DeviceState::Usable && !snapshot.destroy_requested {
            // When: `snapshot` still accepts work, it cannot prove a stopped redraw refusal.
            return;
        }
        self.stopped_redraw_count = self.stopped_redraw_count.saturating_add(1);
        let identity = StoppedRedrawIdentity {
            window,
            generation: snapshot.generation,
            state: snapshot.state,
            destroy_requested: snapshot.destroy_requested,
        };
        if self.expected_refusal == Some(identity) {
            self.stopped_redraw =
                Some(StoppedRedrawObservation { identity, count: self.stopped_redraw_count });
        }
    }

    /// Freeze the faulted main identity and refusal counter before injection changes device state.
    fn capture_fault_baseline(
        &self,
        window: winit::window::WindowId,
        kind: GpuFaultKind,
        snapshot: &DeviceErrorSnapshot,
        frames: SmokeFrameCounts,
        marker_rows: usize,
    ) -> FaultBaseline {
        let destroy_requested = kind == GpuFaultKind::DestroyDevice;
        FaultBaseline {
            frames,
            marker_rows,
            render_attempts: self.render_attempts,
            refusal: StoppedRedrawObservation {
                identity: StoppedRedrawIdentity {
                    window,
                    generation: snapshot.generation,
                    state: if destroy_requested {
                        DeviceState::Lost
                    } else {
                        DeviceState::Unusable
                    },
                    destroy_requested,
                },
                count: self.stopped_redraw_count,
            },
        }
    }

    /// A refusal proves only its own window/device state and only after the captured baseline.
    fn observed_fresh_refusal(
        &self,
        baseline: FaultBaseline,
        snapshot: &DeviceErrorSnapshot,
    ) -> bool {
        self.stopped_redraw.is_some_and(|observed| {
            observed.count > baseline.refusal.count
                && observed.identity == baseline.refusal.identity
                && snapshot.generation == observed.identity.generation
                && snapshot.state == observed.identity.state
                && snapshot.destroy_requested == observed.identity.destroy_requested
        })
    }

    fn fault_pending(&self) -> bool {
        matches!(
            self.phase,
            RuntimeSmokePhase::FaultPrecondition
                | RuntimeSmokePhase::FaultIsolated { .. }
                | RuntimeSmokePhase::FaultRetainedReady
                | RuntimeSmokePhase::FaultRetained { .. }
                | RuntimeSmokePhase::FaultDestroyReady
                | RuntimeSmokePhase::FaultDestroy { .. }
                | RuntimeSmokePhase::FrameValidationReady
                | RuntimeSmokePhase::FrameValidationArmed { .. }
                | RuntimeSmokePhase::FrameValidationMarkerReady { .. }
                | RuntimeSmokePhase::FrameValidation { .. }
        )
    }

    fn next_fault(&self) -> Option<GpuFaultKind> {
        match self.phase {
            RuntimeSmokePhase::FaultPrecondition => Some(GpuFaultKind::IsolatedOperation),
            RuntimeSmokePhase::FaultRetainedReady => Some(GpuFaultKind::RetainedResourceCreation),
            RuntimeSmokePhase::FaultDestroyReady => Some(GpuFaultKind::DestroyDevice),
            RuntimeSmokePhase::FrameValidationReady => Some(GpuFaultKind::FrameValidation),
            _ => None,
        }
    }

    fn begin_fault(&mut self, kind: GpuFaultKind, baseline: FaultBaseline, now: Instant) {
        self.expected_refusal = Some(baseline.refusal.identity);
        self.fault_started = Some(now);
        self.phase = match kind {
            GpuFaultKind::IsolatedOperation => RuntimeSmokePhase::FaultIsolated { baseline },
            GpuFaultKind::RetainedResourceCreation => RuntimeSmokePhase::FaultRetained { baseline },
            GpuFaultKind::DestroyDevice => RuntimeSmokePhase::FaultDestroy { baseline },
            GpuFaultKind::FrameValidation => RuntimeSmokePhase::FrameValidationArmed { baseline },
        };
    }

    fn frame_marker_pending(&self) -> bool {
        matches!(self.phase, RuntimeSmokePhase::FrameValidationMarkerReady { .. })
    }

    /// Start the quiet interval only after a marker command was queued on the stopped device.
    /// The independent fault_started deadline remains anchored to arming.
    fn begin_frame_marker_wait(&mut self, now: Instant) {
        if let RuntimeSmokePhase::FrameValidationMarkerReady { baseline } = self.phase {
            self.phase = RuntimeSmokePhase::FrameValidation { baseline, quiet_started: now };
        }
    }

    /// Evaluate fault evidence with explicit clocks/counters and exact stopped-boundary identity.
    fn observe_fault(
        &mut self,
        snapshot: &DeviceErrorSnapshot,
        frames: SmokeFrameCounts,
        marker_rows: usize,
        now: Instant,
    ) {
        let Some(started) = self.fault_started else {
            // When: fault_started is absent, no injection has supplied a baseline yet.
            return;
        };
        let elapsed = now.saturating_duration_since(started);
        let (baseline, required, loss, quiet_elapsed) = match self.phase {
            RuntimeSmokePhase::FaultIsolated { baseline } => {
                // When: phase is FaultIsolated, require the scoped error and a later acknowledged frame.
                if snapshot.state != DeviceState::Usable || snapshot.counts.isolated != 1 {
                    self.fail(RuntimeSmokeFailure::GpuFaultContainment);
                } else if frames.successful > baseline.frames.successful {
                    // When: frames.successful advances beyond baseline, isolated containment is proven.
                    self.phase = RuntimeSmokePhase::FaultRetainedReady;
                    self.fault_started = None;
                } else if elapsed >= FAULT_DEADLINE {
                    // When: elapsed reaches FAULT_DEADLINE, an isolated fault failed to permit presentation.
                    self.fail(RuntimeSmokeFailure::GpuFaultContainment);
                }
                return;
            }
            RuntimeSmokePhase::FaultRetained { baseline } => {
                (baseline, DeviceState::Unusable, false, elapsed)
            }
            RuntimeSmokePhase::FaultDestroy { baseline } => {
                (baseline, DeviceState::Lost, true, elapsed)
            }
            RuntimeSmokePhase::FrameValidationArmed { baseline } => {
                // When: phase is FrameValidationArmed, pre-stop marker output proves no post-fault shell liveness.
                if frames != baseline.frames || elapsed >= FAULT_DEADLINE {
                    // When: frames advance or elapsed reaches the arming deadline, delayed rendering cannot rescue proof.
                    self.fail(RuntimeSmokeFailure::GpuFaultContainment);
                    return;
                }
                if self.render_attempts <= baseline.render_attempts {
                    // When: render_attempts has not advanced, the persistent frame fault is still only armed.
                    return;
                }
                if snapshot.state != DeviceState::Unusable {
                    // When: snapshot stayed usable after a render attempt, the persistent frame fault was not exercised.
                    self.fail(RuntimeSmokeFailure::GpuFaultContainment);
                    return;
                }
                // Snapshot all marker rows only after a real render has stopped the device.
                self.phase = RuntimeSmokePhase::FrameValidationMarkerReady {
                    baseline: FaultBaseline { marker_rows, ..baseline },
                };
                return;
            }
            RuntimeSmokePhase::FrameValidationMarkerReady { baseline } => {
                // When: phase is FrameValidationMarkerReady, no quiet interval exists until the resend succeeds.
                if snapshot.state != DeviceState::Unusable
                    || frames != baseline.frames
                    || elapsed >= FAULT_DEADLINE
                {
                    self.fail(RuntimeSmokeFailure::GpuFaultContainment);
                }
                return;
            }
            RuntimeSmokePhase::FrameValidation { baseline, quiet_started } => (
                baseline,
                DeviceState::Unusable,
                false,
                now.saturating_duration_since(quiet_started),
            ),
            _ => {
                // When: phase is not waiting for a fault, this observation belongs to another boundary.
                return;
            }
        };
        let failure = if loss {
            RuntimeSmokeFailure::GpuDeviceLoss
        } else {
            RuntimeSmokeFailure::GpuFaultContainment
        };
        if snapshot.state != required
            || (loss && snapshot.lost.is_none())
            || frames != baseline.frames
            || elapsed >= FAULT_DEADLINE
        {
            // When: snapshot, frames, or elapsed contradict containment, an old marker cannot rescue it.
            self.fail(failure);
            return;
        }
        let exercised = if matches!(
            self.phase,
            RuntimeSmokePhase::FaultRetained { .. } | RuntimeSmokePhase::FaultDestroy { .. }
        ) {
            self.observed_fresh_refusal(baseline, snapshot)
        } else {
            // When: `matches!` excludes retained/destroy phases, FrameValidation still requires an actual renderer call.
            self.render_attempts > baseline.render_attempts
        };
        if marker_rows <= baseline.marker_rows || !exercised || quiet_elapsed < FAULT_OBSERVATION {
            // When: `marker_rows`, `exercised`, or `quiet_elapsed` lacks fresh proof, retain the original deadline.
            return;
        }
        if matches!(self.phase, RuntimeSmokePhase::FaultRetained { .. }) {
            self.phase = RuntimeSmokePhase::FaultDestroyReady;
            self.fault_started = None;
        } else {
            // When: matches! excludes FaultRetained, fresh shell output completes the final fault proof.
            self.phase = RuntimeSmokePhase::Complete;
            self.outcome = Some(Ok(()));
        }
    }

    pub(crate) fn fail(&mut self, failure: RuntimeSmokeFailure) {
        self.phase = RuntimeSmokePhase::Complete;
        self.outcome = Some(Err(failure));
    }

    pub(crate) fn timeout_failure(&self) -> RuntimeSmokeFailure {
        match self.phase {
            RuntimeSmokePhase::Display => RuntimeSmokeFailure::Display,
            RuntimeSmokePhase::Gpu => RuntimeSmokeFailure::Gpu,
            RuntimeSmokePhase::Pty => RuntimeSmokeFailure::Pty,
            RuntimeSmokePhase::Marker => RuntimeSmokeFailure::Marker,
            RuntimeSmokePhase::Present { .. } => RuntimeSmokeFailure::Present,
            RuntimeSmokePhase::RecoveryReady => RuntimeSmokeFailure::GpuDeviceRecovery,
            RuntimeSmokePhase::FaultPrecondition
            | RuntimeSmokePhase::FaultIsolated { .. }
            | RuntimeSmokePhase::FaultRetainedReady
            | RuntimeSmokePhase::FaultRetained { .. }
            | RuntimeSmokePhase::FrameValidationReady
            | RuntimeSmokePhase::FrameValidationArmed { .. }
            | RuntimeSmokePhase::FrameValidationMarkerReady { .. }
            | RuntimeSmokePhase::FrameValidation { .. } => RuntimeSmokeFailure::GpuFaultContainment,
            RuntimeSmokePhase::FaultDestroyReady | RuntimeSmokePhase::FaultDestroy { .. } => {
                RuntimeSmokeFailure::GpuDeviceLoss
            }
            RuntimeSmokePhase::WarmCreate
            | RuntimeSmokePhase::WarmAdopt { .. }
            | RuntimeSmokePhase::WarmRelease { .. }
            | RuntimeSmokePhase::FreshCreate
            | RuntimeSmokePhase::FreshPresent { .. }
            | RuntimeSmokePhase::FreshRelease { .. }
            | RuntimeSmokePhase::Complete => RuntimeSmokeFailure::WarmLifecycle,
        }
    }

    pub(crate) fn outcome(&self) -> Option<Result<(), RuntimeSmokeFailure>> {
        self.outcome
    }
}

pub(crate) fn grid_contains_marker(grid: &Grid, marker: &str) -> bool {
    grid_marker_rows(grid, marker) > 0
}

/// Count marker-bearing live and scrollback rows, so a repeated command needs fresh output.
fn grid_marker_rows(grid: &Grid, marker: &str) -> usize {
    grid.rows_iter()
        .chain(grid.scrollback_iter())
        .filter(|row| row.iter().map(|cell| cell.ch).collect::<String>().contains(marker))
        .count()
}

impl super::App {
    pub(crate) fn install_runtime_smoke(
        &mut self,
        spec: &RuntimeSmokeSpec,
        renderer_baseline: usize,
        scenario: RuntimeSmokeScenario,
    ) {
        self.runtime_config_path = Some(spec.config_dir().join("sonicterm.toml"));
        let mut smoke = RuntimeSmokeState::from_spec(spec, renderer_baseline);
        smoke.verify_fresh_drop_target = self.owns_native_drop_target();
        smoke.scenario = scenario;
        self.runtime_smoke = Some(smoke);
    }

    pub(super) fn smoke_check_native_title(
        &mut self,
        window: &winit::window::Window,
        expected: &str,
    ) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let observed = window.title();
            let matches = observed == expected;
            tracing::warn!(?observed, ?expected, matches, "runtime smoke native title readback");
            if !matches {
                // Native readback disproves the title write even if the model is correct.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::Display);
                }
            }
            matches
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = (window, expected);
            // X11 winit title() is unimplemented; Wayland caches locally, so neither proves compositor state.
            tracing::warn!(
                "runtime smoke native title readback unsupported; desktop evidence required"
            );
            true
        }
    }

    pub(super) fn smoke_exercise_window_name(&mut self, id: winit::window::WindowId) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(native) = self.windows.get(&id).and_then(|state| state.window.clone()) else {
            // When: id lacks a native handle, model-only assertions cannot establish this smoke contract.
            return false;
        };
        for name in ["Work 工作", ""] {
            self.start_rename_window(id);
            self.command_palette.set_query(name);
            self.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
            let expected = super::compose_window_title(self.window_keys.get(id).unwrap(), name);
            if !self.smoke_check_native_title(&native, &expected) {
                // When: smoke_check_native_title fails after a real editor commit, stop before claiming presentation success.
                return false;
            }
        }
        true
    }

    pub(crate) fn runtime_smoke_result(&self) -> Result<(), RuntimeSmokeFailure> {
        let Some(smoke) = self.runtime_smoke.as_ref() else {
            // When: no smoke was installed, a caller cannot claim runtime-smoke completion.
            return Err(RuntimeSmokeFailure::EventLoop);
        };
        smoke.outcome().unwrap_or_else(|| Err(smoke.timeout_failure()))
    }
}

impl super::App {
    /// Only an installed fault smoke contributes periodic wakeups; normal sessions stay idle.
    pub(super) fn gpu_fault_smoke_deadline(&self) -> Option<Instant> {
        self.runtime_smoke
            .as_ref()
            .filter(|smoke| smoke.fault_pending())
            .map(|smoke| smoke.next_probe_at.unwrap_or_else(Instant::now))
    }

    /// Advance fault injection on the event-loop thread, outside every parser/render borrow.
    pub(super) fn drive_gpu_fault_smoke(&mut self, now: Instant) -> bool {
        if !self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.fault_pending()) {
            // When: runtime_smoke is absent or not in a fault phase, no fault or polling is enabled.
            return false;
        }
        if self
            .runtime_smoke
            .as_ref()
            .and_then(|smoke| smoke.next_probe_at)
            .is_some_and(|due| now < due)
        {
            // When: next_probe_at is still ahead of now, avoid a redraw busy-loop during observation.
            return false;
        }
        let Some(mut smoke) = self.runtime_smoke.take() else {
            // When: runtime_smoke disappeared, no smoke-owned action may run.
            return false;
        };
        if let Err(failure) = self.tick_gpu_fault_smoke(&mut smoke, now) {
            smoke.fail(failure);
        }
        smoke.next_probe_at = Some(Instant::now() + FAULT_POLL_INTERVAL);
        let finished = smoke.outcome().is_some();
        if finished {
            tracing::warn!(outcome = ?smoke.outcome(), "runtime smoke GPU fault verdict");
        }
        self.runtime_smoke = Some(smoke);
        finished
    }

    /// Queue the smoke marker in the original main shell without embedding it literally in input.
    fn queue_gpu_fault_smoke_marker(
        &self,
        smoke: &RuntimeSmokeState,
        failure: RuntimeSmokeFailure,
    ) -> Result<(), RuntimeSmokeFailure> {
        let pane = self
            .main_active_pane_id()
            .and_then(|id| self.main().and_then(|window| window.panes.get(&id)))
            .ok_or(failure)?;
        pane.pty
            .as_ref()
            .ok_or(failure)?
            .send_input_nonblocking(smoke.command().to_vec())
            .map_err(|_| failure)
    }

    fn tick_gpu_fault_smoke(
        &mut self,
        smoke: &mut RuntimeSmokeState,
        now: Instant,
    ) -> Result<(), RuntimeSmokeFailure> {
        let failure = smoke.timeout_failure();
        if smoke
            .fault_started
            .is_some_and(|started| now.saturating_duration_since(started) >= FAULT_DEADLINE)
        {
            // When: fault_started exceeds FAULT_DEADLINE, even parser contention must fail closed.
            return Err(failure);
        }
        let snapshot = self.main_renderer().ok_or(failure)?.device_error_snapshot();
        let shared = self.windows.values().all(|window| {
            window
                .renderer
                .as_ref()
                .is_some_and(|renderer| renderer.device_generation() == snapshot.generation)
        }) && self
            .warm_window_pool
            .iter()
            .all(|warm| warm.renderer.device_generation() == snapshot.generation);
        if !shared {
            // When: shared is false, a fault cannot establish process-wide containment on this topology.
            return Err(RuntimeSmokeFailure::GpuFaultContainment);
        }
        if matches!(
            smoke.phase,
            RuntimeSmokePhase::FaultPrecondition | RuntimeSmokePhase::FrameValidationReady
        ) {
            // When: matches! selects a starting phase, freeze renderer population before sampling totals.
            self.config.window.warm_window_pool = 0;
            self.warm_window_pool.clear();
            if matches!(smoke.phase, RuntimeSmokePhase::FaultPrecondition)
                && sonicterm_gpu::core::live_renderer_count() != smoke.renderer_baseline + 1
            {
                // When: live_renderer_count exceeds the main renderer, warm release was not complete.
                return Err(RuntimeSmokeFailure::WarmLifecycle);
            }
        }
        let frames = self.windows.values().filter_map(|window| window.renderer.as_ref()).fold(
            SmokeFrameCounts::default(),
            |mut total, renderer| {
                total.presents = total.presents.saturating_add(renderer.present_call_count());
                total.successful =
                    total.successful.saturating_add(renderer.successful_frame_count());
                total
            },
        );
        let pane = self
            .main_active_pane_id()
            .and_then(|id| self.main().and_then(|window| window.panes.get(&id)))
            .ok_or(failure)?;
        let marker_rows = {
            let Some(parser) = pane.parser.try_lock() else {
                // When: parser is busy, defer observation rather than blocking the event loop.
                return Ok(());
            };
            grid_marker_rows(parser.grid(), smoke.marker())
        };
        if let Some(kind) = smoke.next_fault() {
            // When: next_fault supplies kind, freeze evidence before the one injection for that phase.
            if matches!(kind, GpuFaultKind::IsolatedOperation | GpuFaultKind::FrameValidation)
                && (snapshot.state != DeviceState::Usable || snapshot.destroy_requested)
            {
                // When: snapshot is already stopped, a fresh fault cannot prove the intended transition.
                return Err(RuntimeSmokeFailure::GpuFaultContainment);
            }
            let baseline = smoke.capture_fault_baseline(
                self.main_window_id.ok_or(failure)?,
                kind,
                &snapshot,
                frames,
                marker_rows,
            );
            smoke.begin_fault(kind, baseline, now);
            tracing::warn!(
                ?kind,
                generation = snapshot.generation,
                ?frames,
                marker_rows,
                "runtime smoke inject GPU fault"
            );
            let renderer = self.main_renderer_mut().ok_or(failure)?;
            renderer.__inject_gpu_fault(kind);
            if kind == GpuFaultKind::RetainedResourceCreation {
                renderer.force_rebuild_for_scale(renderer.scale_factor());
            }
            if !matches!(kind, GpuFaultKind::IsolatedOperation | GpuFaultKind::FrameValidation) {
                self.queue_gpu_fault_smoke_marker(smoke, failure)?;
            }
            if kind != GpuFaultKind::FrameValidation {
                // Preserve retained/loss timing: the bounded destroy poll does not consume the marker deadline.
                smoke.fault_started = Some(Instant::now());
            }
            // FrameValidation retains begin_fault's five-second arming deadline, even if render is delayed.
            self.mark_all_window_inputs();
            self.request_redraw_all_terminal_windows();
        } else {
            // When: next_fault is absent, sample the already-injected fault until proof or its deadline.
            smoke.observe_fault(&snapshot, frames, marker_rows, now);
            if smoke.frame_marker_pending() {
                // frame_marker_pending confirms Unusable; resend after the fresh baseline before starting quiet time.
                tracing::warn!(
                    generation = snapshot.generation,
                    marker_rows,
                    ?frames,
                    "runtime smoke frame device stopped; resend shell marker"
                );
                self.queue_gpu_fault_smoke_marker(smoke, failure)?;
                smoke.begin_frame_marker_wait(Instant::now());
            }
            // Exercise stopped redraw refusal throughout the bounded interval, not merely an idle device snapshot.
            self.mark_all_window_inputs();
            self.request_redraw_all_terminal_windows();
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "runtime_smoke_tests.rs"]
mod runtime_smoke_tests;
