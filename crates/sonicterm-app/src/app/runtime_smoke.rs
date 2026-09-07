use std::path::{Path, PathBuf};

use sonicterm_grid::grid::Grid;

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
        Ok(Self { shell_program, marker, command, config_dir, log_dir })
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
    Complete,
}

/// State retained by the app while a bounded native package smoke is active.
pub(crate) struct RuntimeSmokeState {
    marker: String,
    command: Vec<u8>,
    phase: RuntimeSmokePhase,
    renderer_baseline: usize,
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
            outcome: None,
        }
    }

    pub(crate) fn from_spec(spec: &RuntimeSmokeSpec, renderer_baseline: usize) -> Self {
        Self {
            marker: spec.marker.clone(),
            command: spec.command.clone(),
            phase: RuntimeSmokePhase::Display,
            renderer_baseline,
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
        self.phase = RuntimeSmokePhase::WarmCreate;
        true
    }

    pub(crate) fn should_maintain_warm_pool(&self) -> bool {
        matches!(self.phase, RuntimeSmokePhase::WarmCreate)
    }

    pub(crate) fn renderer_baseline(&self) -> usize {
        self.renderer_baseline
    }

    pub(crate) fn is_waiting_for_adopted_present(&self, child: winit::window::WindowId) -> bool {
        matches!(self.phase, RuntimeSmokePhase::WarmAdopt { child: expected, .. } if expected == child)
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
        let RuntimeSmokePhase::WarmAdopt { child: expected, baseline } = self.phase else {
            // When: no adopted child is awaiting a frame, this presentation belongs to another window.
            return false;
        };
        if child != expected || current <= baseline {
            // When: identity or frame count does not advance the adopted child, keep waiting.
            return false;
        }
        self.phase = RuntimeSmokePhase::WarmRelease { child };
        true
    }

    pub(crate) fn finish_warm_release(
        &mut self,
        child: winit::window::WindowId,
        released: bool,
    ) -> bool {
        if !released
            || !matches!(self.phase, RuntimeSmokePhase::WarmRelease { child: expected } if expected == child)
        {
            // When: the adopted child was not the exact state released, fail closed at the warm lifecycle.
            self.fail(RuntimeSmokeFailure::WarmLifecycle);
            return false;
        }
        self.phase = RuntimeSmokePhase::Complete;
        self.outcome = Some(Ok(()));
        true
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
            RuntimeSmokePhase::WarmCreate
            | RuntimeSmokePhase::WarmAdopt { .. }
            | RuntimeSmokePhase::WarmRelease { .. }
            | RuntimeSmokePhase::Complete => RuntimeSmokeFailure::WarmLifecycle,
        }
    }

    pub(crate) fn outcome(&self) -> Option<Result<(), RuntimeSmokeFailure>> {
        self.outcome
    }
}

pub(crate) fn grid_contains_marker(grid: &Grid, marker: &str) -> bool {
    grid.rows_iter().chain(grid.scrollback_iter()).any(|row| {
        let text = row.iter().map(|cell| cell.ch).collect::<String>();
        text.contains(marker)
    })
}

impl super::App {
    pub(crate) fn install_runtime_smoke(
        &mut self,
        spec: &RuntimeSmokeSpec,
        renderer_baseline: usize,
    ) {
        self.runtime_config_path = Some(spec.config_dir().join("sonicterm.toml"));
        self.runtime_smoke = Some(RuntimeSmokeState::from_spec(spec, renderer_baseline));
    }

    pub(crate) fn runtime_smoke_result(&self) -> Result<(), RuntimeSmokeFailure> {
        let Some(smoke) = self.runtime_smoke.as_ref() else {
            // When: no smoke was installed, a caller cannot claim runtime-smoke completion.
            return Err(RuntimeSmokeFailure::EventLoop);
        };
        smoke.outcome().unwrap_or_else(|| Err(smoke.timeout_failure()))
    }
}

#[cfg(test)]
#[path = "runtime_smoke_tests.rs"]
mod runtime_smoke_tests;
