//! The per-sample echo timeline: the event-loop and worker events between an echo's appearance and the
//! frame that presented it, frozen when `take_echo_watch` takes its token and transferred once.
//!
//! This build records no timeline, so the accessor answers `NotRecorded` and a harness reports the
//! timeline unavailable. The types are the frozen v1 contract; a later change adds a `_v2` accessor
//! with its own types.

use std::time::Instant;

use super::ArmToken;

/// What `App::take_echo_timeline_v1` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EchoTimelineTakeV1 {
    /// The frozen record; the App keeps nothing of it.
    Timeline(EchoTimelineV1),
    /// This build records no timeline, or recorded none for this arming.
    NotRecorded,
    /// The frame-counter gate is off.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// The watch holds another token, or none.
    Mismatch,
    /// The watch is still armed: `take_echo_watch` freezes the record first.
    NotTaken,
    /// This token's timeline was already transferred.
    AlreadyTaken,
}

/// One arming's frozen timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoTimelineV1 {
    /// Whether the pane's PTY stamped `read_at` while armed; false means no `LatestChunkRead` can appear.
    pub chunk_timestamps: bool,
    /// The watched window's valid permit at arm, with its tick's metadata.
    pub initial_permit: Option<PermitSnapshotV1>,
    /// A valid permit was held at arm but its identity is unavailable or not representable. Never set
    /// together with `initial_permit` (the sample is malformed if both are).
    pub initial_permit_unknown: bool,
    /// Events of the window that held the pane at arm, in record order; at most 64.
    pub events: Vec<EchoTimelineEventV1>,
    /// An event was dropped (the buffer was full, or a `loop_seq`/`dispatch_seq` did not fit `u32`):
    /// every event-dependent analysis of this sample is disqualified.
    pub overflow: bool,
    /// The watched window's `PaneOutput` service completions since arm, oldest first; at most 256.
    pub flood_services: Vec<FloodServiceV1>,
    /// The highest `loop_seq` evicted from the flood ring; `None` when nothing was evicted. This is the
    /// ring's explicit loss metadata; it affects only the flood-competition analysis.
    pub flood_evicted_through: Option<u32>,
}

/// An accepted display-link tick's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickIdentityV1 {
    /// The accepted tick's per-window diagnostic sequence; not reset at arm.
    pub tick_seq: u32,
    /// The link generation it was accepted under.
    pub generation: u32,
}

/// A permit held at arm, with the tick that stored it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermitSnapshotV1 {
    /// The tick that stored the permit.
    pub identity: TickIdentityV1,
    /// When the display-link tick was accepted (its delivery).
    pub delivered_at: Instant,
}

/// One timeline event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoTimelineEventV1 {
    /// Integer nanoseconds since `EchoTrace::armed_at`; an event before the arm is never recorded.
    pub at_ns: u64,
    /// What happened.
    pub kind: EchoTimelineKindV1,
}

/// What one timeline event records. `loop_seq` is one per-window event-loop sequence, reset at arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoTimelineKindV1 {
    /// The latest `read_at` among the chunks of the section where the echo appeared (a lower bound).
    LatestChunkRead,
    /// A visible-output check that evaluated the watched pane explicitly.
    OutputCheck {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The watched pane's output generation the check read.
        generation: u64,
        /// What the check did.
        outcome: OutputCheckOutcomeV1,
    },
    /// An accepted display-link tick stored a permit; a tick while a permit is held replaces it.
    Tick {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The tick's identity; `None` when it is not representable.
        identity: Option<TickIdentityV1>,
    },
    /// One redraw admission decision for the watched window.
    Admission {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The per-window dispatch this decision belongs to.
        dispatch_seq: u32,
        /// The decision.
        decision: AdmissionDecisionV1,
    },
    /// A held permit stopped being held for a reason other than its own consumption.
    PermitCleared {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The cleared permit's tick; `None` when its identity is unavailable.
        tick_seq: Option<u32>,
        /// Why it was cleared.
        cause: PermitClearCauseV1,
    },
    /// Start of the outer window-event handler for the watched window's `RedrawRequested`.
    DispatchEntry {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The per-window dispatch.
        dispatch_seq: u32,
    },
    /// End of that handler, after its counter and frame-line bookkeeping.
    DispatchReturn {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The per-window dispatch.
        dispatch_seq: u32,
    },
    /// Immediately before that dispatch's renderer call.
    RenderEnter {
        /// The per-window dispatch.
        dispatch_seq: u32,
    },
    /// Immediately after that dispatch's renderer call.
    RenderExit {
        /// The per-window dispatch.
        dispatch_seq: u32,
    },
}

/// What a visible-output check did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputCheckOutcomeV1 {
    /// A new native redraw request was issued.
    NativeRequest,
    /// The output cause was marked while a request was already in flight.
    MarkedInFlight,
    /// Nothing was requested.
    None,
}

/// One redraw admission decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecisionV1 {
    /// A link-paced streaming admission on a valid permit. It consumes that permit; `tick_seq` is
    /// `None` when the permit's identity is unavailable.
    Permit {
        /// The consumed permit's tick.
        tick_seq: Option<u32>,
    },
    /// A link-paced streaming admission with no valid permit, by the fallback ceiling.
    Fallback,
    /// Any other admission: the timer path or no streaming work. A permit it discards is a separate
    /// `PermitCleared { cause: AdmissionDiscard }`.
    Admitted,
    /// Deferred by the first rule that held; the permit is untouched.
    Deferred(DeferReasonV1),
    /// Refused before pacing (hidden, occluded, parked, stopped, or device-refused); the permit is untouched.
    Held,
}

/// The rule that deferred an admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferReasonV1 {
    /// A surface timeout within the frame period.
    Timeout,
    /// The window's lock-contention retry floor.
    Contention,
    /// A visible pane's synchronized update is still open.
    Sync,
    /// Streaming output paced to the frame period.
    Streaming,
}

/// Why a held permit was cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermitClearCauseV1 {
    /// An `Admitted` or `Fallback` admission discarded a held permit.
    AdmissionDiscard,
    /// Link pacing was invalidated on a pause, suppression or restart.
    LinkReset,
    /// The live display-link interval was paused.
    LinkPaused,
}

/// One `PaneOutput` service completion in the watched window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloodServiceV1 {
    /// Event-loop sequence.
    pub loop_seq: u32,
    /// The pane whose output was serviced.
    pub pane_id: u64,
}

impl super::App {
    /// Transfer, exactly once, the timeline frozen when `take_echo_watch` took `token` for pane
    /// `pane_id`. Takes no parser lock. This build records no timeline, so it answers `NotRecorded`.
    #[doc(hidden)]
    pub fn take_echo_timeline_v1(&mut self, _pane_id: u64, _token: ArmToken) -> EchoTimelineTakeV1 {
        EchoTimelineTakeV1::NotRecorded
    }
}

#[cfg(test)]
#[path = "echo_timeline_tests.rs"]
mod echo_timeline_tests;
