//! Perf control phase signalling: the harness tells the App which phase runs, and only a build whose
//! compile-time C5 control is on pauses the flood pane's VT worker for the typing phase.
//!
//! This build has no compile-time control, so every call answers `NoControl` and keeps no state.
//! The types are the frozen v1 contract; a later change adds a `_v2` method with its own types.

use std::time::Instant;

/// The phase the harness is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfControlPhaseV1 {
    /// The typing phase; `flood_pane` is the pane C5 pauses.
    Typing {
        /// The pane whose VT worker C5 pauses.
        flood_pane: u64,
    },
    /// Any other phase, and every cleanup or Drop path.
    Other,
}

/// What one phase call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfControlOutcomeV1 {
    /// No control acts: the C5 constant is off, or `Other` found no outstanding request.
    NoControl,
    /// `Typing` named a pane no window holds; nothing was sent.
    NoPane,
    /// `Typing` named another pane while a request for `outstanding` is held; nothing was sent.
    Conflict {
        /// The pane whose request is held.
        outstanding: u64,
    },
    /// A Pause was sent at `requested_at` and is not acknowledged yet. A repeated same-pane `Typing`
    /// returns this again, with the original instant, and sends nothing.
    PauseRequested {
        /// When the first `Typing` sent Pause.
        requested_at: Instant,
    },
    /// The worker acknowledged the pause. A repeated same-pane `Typing` returns this, with the original
    /// instants, and sends nothing.
    Paused {
        /// When the first `Typing` sent Pause.
        requested_at: Instant,
        /// The acknowledgement boundary the worker recorded.
        acknowledged_at: Instant,
    },
    /// `Other` recorded the resume cutoff and sent Resume for the held request. This is a request, not
    /// proof that the worker resumed.
    ResumeRequested(PerfControlPauseV1),
    /// The worker stopped, or a Pause or Resume send failed, before a normal resume. A same-pane `Typing`
    /// returns this; `Other` returns it once and clears the request without sending Resume.
    WorkerStopped(PerfControlStopV1),
}

/// The evidence of a pause that `Other` asked to resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerfControlPauseV1 {
    /// The pane that was paused.
    pub flood_pane: u64,
    /// When the first `Typing` sent Pause.
    pub requested_at: Instant,
    /// The acknowledgement boundary, recorded by the worker under the observation lock; `None` if never.
    pub acknowledged_at: Option<Instant>,
    /// The resume cutoff, recorded by `Other` under the observation lock before Resume is sent.
    pub resume_requested_at: Instant,
    /// The exact number of read-completion observations between the two boundaries; 0 when
    /// `acknowledged_at` is `None`.
    pub reads_after_ack: u64,
}

/// The evidence of a pause whose worker stopped, or whose send failed, before a normal resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerfControlStopV1 {
    /// The pane whose worker was asked to pause.
    pub flood_pane: u64,
    /// When the first `Typing` asked for the pause (the attempted send, when the send itself failed).
    pub requested_at: Instant,
    /// The acknowledgement boundary, if one was recorded before the stop.
    pub acknowledged_at: Option<Instant>,
    /// When the stop was latched under the observation lock: worker exit, or the failed send.
    pub stopped_at: Instant,
}

impl super::App {
    /// Tell the App which harness phase runs. Only a build whose compile-time C5 constant is on acts:
    /// `Typing` requests (once) or queries a pause of `flood_pane`'s VT worker, and `Other` requests its
    /// resume. Every other build, this one included, returns `NoControl` and keeps no state.
    #[doc(hidden)]
    pub fn perf_control_phase(&mut self, _phase: PerfControlPhaseV1) -> PerfControlOutcomeV1 {
        PerfControlOutcomeV1::NoControl
    }
}

#[cfg(test)]
#[path = "perf_control_tests.rs"]
mod perf_control_tests;
