use sonicterm_grid::grid::Grid;

/// Release-smoke failure boundary and its stable process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSmokeFailure {
    /// The winit event loop could not be constructed or run.
    EventLoop,
    /// The selected X11 or Wayland display could not create a window.
    Display,
    /// GPU initialization did not produce a renderer.
    Gpu,
    /// `/bin/sh` did not spawn as the active pane PTY.
    Pty,
    /// The shell marker could not be queued or observed in the grid.
    Marker,
    /// A marker-bearing frame did not reach native presentation.
    Present,
}

impl RuntimeSmokeFailure {
    /// Stable exit code consumed by Linux package smoke jobs.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::EventLoop => 10,
            Self::Display => 11,
            Self::Gpu => 12,
            Self::Pty => 13,
            Self::Marker => 14,
            Self::Present => 15,
        }
    }
}

impl std::fmt::Display for RuntimeSmokeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let boundary = match self {
            Self::EventLoop => "event loop",
            Self::Display => "display/window",
            Self::Gpu => "GPU renderer",
            Self::Pty => "/bin/sh PTY",
            Self::Marker => "PTY marker",
            Self::Present => "frame presentation",
        };
        write!(formatter, "runtime smoke failed at {boundary}")
    }
}

impl std::error::Error for RuntimeSmokeFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeSmokePhase {
    Display,
    Gpu,
    Pty,
    Marker,
    Present { baseline: u64 },
    Complete,
}

/// State retained by the app while the hidden Linux package smoke is active.
pub(crate) struct RuntimeSmokeState {
    marker: String,
    command: Vec<u8>,
    phase: RuntimeSmokePhase,
    outcome: Option<Result<(), RuntimeSmokeFailure>>,
}

impl RuntimeSmokeState {
    pub(crate) fn new(nonce: u32) -> Self {
        let marker = format!("__SONICTERM_SMOKE_{nonce}__");
        let command = format!("printf '__SONICTERM_SMOKE_%s__\\n' '{nonce}'\n").into_bytes();
        debug_assert!(!String::from_utf8_lossy(&command).contains(&marker));
        Self { marker, command, phase: RuntimeSmokePhase::Display, outcome: None }
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

    pub(crate) fn observe_presented_frame(
        &mut self,
        current: u64,
    ) -> Option<Result<(), RuntimeSmokeFailure>> {
        let RuntimeSmokePhase::Present { baseline } = self.phase else {
            // When: `self.phase` is not `RuntimeSmokePhase::Present`, no marker-bearing frame is pending.
            return None;
        };
        if current <= baseline {
            // When: `current` has not advanced past `baseline`, the marker-bearing frame was not presented.
            return None;
        }
        self.phase = RuntimeSmokePhase::Complete;
        self.outcome = Some(Ok(()));
        self.outcome
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
            RuntimeSmokePhase::Present { .. } | RuntimeSmokePhase::Complete => {
                RuntimeSmokeFailure::Present
            }
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
    pub(crate) fn install_runtime_smoke(&mut self, nonce: u32) {
        self.runtime_smoke = Some(RuntimeSmokeState::new(nonce));
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
