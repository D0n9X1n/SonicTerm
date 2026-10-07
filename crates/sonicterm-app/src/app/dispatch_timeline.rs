//! The per-sample dispatch timeline: every App dispatch, publication, output check and renderer attempt of
//! one window between an arming and its take, with an optional appearance witness for one target cell.
//!
//! This build records no timeline: arming answers `GateOff` while the frame-counter gate is off and
//! `NotRecorded` otherwise, and take answers the same. The types are the frozen v1 contract; a later
//! change adds a `_v2` method with its own types rather than reshaping these.

use std::time::Instant;

use super::OutputCheckOutcomeV1;

/// An arming's opaque identity: nonzero and never reused within an App.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
// Only the recording build reads the value; its identity is compared through the derives.
#[allow(dead_code)]
pub struct DispatchTimelineToken(u64);

/// What `App::arm_dispatch_timeline_v1` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchTimelineArmV1 {
    /// A timeline is armed under this token.
    Armed(DispatchTimelineToken),
    /// This build records no timeline.
    NotRecorded,
    /// The frame-counter gate is off.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// A timeline is already armed under this token; it is left as it is.
    StillArmed(DispatchTimelineToken),
    /// No token can be issued any more.
    Exhausted,
}

/// What `App::take_dispatch_timeline_v1` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchTimelineTakeV1 {
    /// The frozen record; the App keeps nothing of it.
    Timeline(DispatchTimelineV1),
    /// This build records no timeline, or recorded none for this token.
    NotRecorded,
    /// The frame-counter gate is off.
    GateOff,
    /// The App holds another arming, or none.
    Mismatch,
    /// This token's timeline was already transferred.
    AlreadyTaken,
}

/// One App dispatch within a timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
// Only the recording build reads the value; ids are joined through the derives.
#[allow(dead_code)]
pub struct DispatchId(u32);

/// One renderer call within a timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
// Only the recording build reads the value; ids are joined through the derives.
#[allow(dead_code)]
pub struct AttemptId(u32);

/// One output publication within a timeline, linking its observed and decided records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
// Only the recording build reads the value; ids are joined through the derives.
#[allow(dead_code)]
pub struct PublicationSeq(u32);

/// The `ApplicationHandler` callback an App dispatch ran in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackKindV1 {
    /// `user_event`.
    UserEvent,
    /// `window_event`; `redraw` when the event was `RedrawRequested`.
    WindowEvent {
        /// Whether the event was `RedrawRequested`.
        redraw: bool,
    },
    /// `new_events`.
    NewEvents,
    /// `about_to_wait`.
    AboutToWait,
    /// `resumed`.
    Resumed,
}

/// The branch an output publication took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationDecisionV1 {
    /// A `PaneOutput` was sent to the window.
    Sent,
    /// One was already outstanding for the window, so none was sent.
    Suppressed,
    /// The send was refused.
    Refused,
    /// The pane had no redraw target.
    Untargeted,
}

/// One recorded event and what it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchEventKindV1 {
    /// Before the outstanding-token decision (or at the untargeted branch): time and covered generation.
    PublicationObserved {
        /// Links this record to its decision.
        publication: PublicationSeq,
        /// The output generation the publication covers.
        covered_generation: u64,
    },
    /// The branch actually taken for that publication; `window` is `None` only for `Untargeted`.
    PublicationDecided {
        /// Links this record to its observation.
        publication: PublicationSeq,
        /// The branch taken.
        decision: PublicationDecisionV1,
        /// The target window, as `u64::from(WindowId)`.
        window: Option<u64>,
    },
    /// A visible-output check.
    OutputCheck {
        /// The enclosing dispatch.
        dispatch: DispatchId,
        /// The output generation checked.
        generation: u64,
        /// What the check did.
        outcome: OutputCheckOutcomeV1,
    },
    /// An App dispatch opened.
    DispatchEntry {
        /// The dispatch.
        dispatch: DispatchId,
        /// The callback it ran in.
        callback: CallbackKindV1,
        /// Its window, as `u64::from(WindowId)`, when the callback names one.
        window: Option<u64>,
    },
    /// That App dispatch returned.
    DispatchReturn {
        /// The dispatch.
        dispatch: DispatchId,
    },
    /// The adapter is about to call `render_releasing` (its own reading); marks that a call was attempted.
    RenderCall {
        /// The attempt.
        attempt: AttemptId,
        /// The enclosing dispatch.
        parent: DispatchId,
    },
    /// The renderer root's opening reading (`AttemptScope::enter`), published through the collector.
    RenderEnter {
        /// The attempt.
        attempt: AttemptId,
        /// The enclosing dispatch.
        parent: DispatchId,
    },
    /// The renderer root's closing reading (`AttemptScope::drop`), published through the collector.
    RenderExit {
        /// The attempt.
        attempt: AttemptId,
    },
    /// The adapter's reading after `render_releasing` returned.
    RenderReturned {
        /// The attempt.
        attempt: AttemptId,
    },
    /// The root never opened or never closed: no `RenderExit` and no `AttemptRecordV1` exist for this attempt.
    AttemptIncomplete {
        /// The attempt.
        attempt: AttemptId,
        /// The enclosing dispatch.
        parent: DispatchId,
        /// Why it is incomplete.
        reason: AttemptIncompleteReasonV1,
    },
    /// The armed pane left the window, or the window closed.
    ScopeLost {
        /// Whether the window closed.
        closed: bool,
    },
    /// A redraw dispatch of the armed window passed `begin_window_redraw` (the window `attempts` increment).
    Admitted {
        /// The admitted dispatch.
        dispatch: DispatchId,
    },
    /// That admitted dispatch returned without calling the renderer.
    AdmittedNotRendered {
        /// The admitted dispatch.
        dispatch: DispatchId,
        /// The path it returned through.
        reason: NotRenderedReasonV1,
    },
    /// An observed check or renderer call with no open App dispatch scope (for example inside `RunAction`).
    /// No `DispatchId` is fabricated; the sample is incomplete.
    Unscoped {
        /// What was observed.
        what: UnscopedKindV1,
    },
}

/// Why a renderer call left no complete attempt record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AttemptIncompleteReasonV1 {
    /// The call was attempted but no root opening reading was published (for example the renderer does not count).
    RootNeverOpened,
    /// The root opened but no closing reading was staged before the stage was dropped.
    RootNeverClosed,
    /// The stage was bound while another binding was active; its writes were suppressed.
    NestedSuppressed,
    /// A nested binding ran while this attempt's root was open; its phase partition is not trustworthy.
    NestedInvalidated,
}

/// Frozen: every path in the main and child adapters that admits and then returns before `render_releasing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NotRenderedReasonV1 {
    /// The frame had no coherent layout; it settles `Settled`.
    NoLayout,
    /// The frame's structure was invalid (for example a missing pane); it parks with no settlement.
    StructuralInvalid,
    /// A parser or image lock was contended; it counts contention, with no settlement.
    Contended,
    /// A synchronized update held the frame; it counts `defer_sync`, with no settlement.
    SyncHeld,
    /// There was no renderer, before or after collection; no settlement.
    NoRenderer,
    /// The window closed during the dispatch; no settlement.
    WindowGone,
    /// The adapter unwound after admission and before `RenderCall`; no settlement.
    AdapterUnwound,
}

/// What an `Unscoped` event observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum UnscopedKindV1 {
    /// A visible-output check.
    OutputCheck,
    /// A renderer root opening.
    RenderEnter,
}

/// The witness's resolution state, frozen at take together with the buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WitnessResolutionV1 {
    /// A captured witness observation that was neither committed nor abandoned when take froze the
    /// record: its section sequence.
    pub unresolved_at_take: Option<u64>,
    /// Witness observations captured under this arming and abandoned (unwind) before their commit.
    pub abandoned: u32,
}

/// One recorded event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchEventV1 {
    /// Nanoseconds after `armed_at`.
    pub at_ns: u64,
    /// What happened.
    pub kind: DispatchEventKindV1,
}

/// The phase partition of one attempt; `Blit` and `GpuSubmit` are separate, and `Other` is the root's own
/// exclusive time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PhaseV1 {
    /// Before frame assembly.
    PreAssembly,
    /// Assembly outside ink and atlas upload.
    AssemblyOther,
    /// Glyph ink.
    Ink,
    /// Glyph atlas upload.
    AtlasUpload,
    /// Present preparation.
    PresentPrep,
    /// Composition.
    Compose,
    /// The blit.
    Blit,
    /// GPU submission.
    GpuSubmit,
    /// Present work outside blit and submission.
    PresentOther,
    /// Settlement.
    Settle,
    /// The root's own exclusive time.
    Other,
}

/// Nanoseconds per phase, indexed by `PhaseV1`; the sum equals the attempt's `attempt_ns`.
// The frozen v1 contract names the one field `ns`; clippy ignores an allow on the field itself.
#[allow(clippy::min_ident_chars)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttemptPhasesV1 {
    /// Nanoseconds per phase, indexed by `PhaseV1`.
    pub ns: [u64; 11],
}

/// Why the renderer skipped a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SkipReasonV1 {
    /// No pane was visible.
    NoPanes,
    /// Nothing changed.
    Unchanged,
    /// The frame was a no-op.
    Noop,
}

/// Exhaustive over `PresentOutcome`, plus `Unwound` for a call that never returned one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptOutcomeV1 {
    /// Skipped, for this reason.
    Skipped(SkipReasonV1),
    /// The retained frame was reblitted.
    CachedReblit,
    /// The glyph atlas changed during assembly, so the frame retries.
    AtlasRetry,
    /// The surface asked for a retry.
    SurfaceRetry,
    /// Rendering was unavailable.
    Unavailable,
    /// A frame was presented.
    Presented,
    /// The frame failed.
    Failed,
    /// The call unwound without returning an outcome.
    Unwound,
}

/// One renderer call's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptRecordV1 {
    /// The attempt.
    pub attempt: AttemptId,
    /// The enclosing dispatch.
    pub parent: DispatchId,
    /// The root's closing reading minus its opening reading.
    pub attempt_ns: u64,
    /// The phase partition of `attempt_ns`.
    pub phases: AttemptPhasesV1,
    /// What the call returned.
    pub outcome: AttemptOutcomeV1,
    /// Whether assembly ran in this call (any pass).
    pub assembled: bool,
    /// Partial-fallback passes in this call; independent of `outcome`.
    pub partial_fallbacks: u8,
    /// This attempt's transitions in `DispatchTimelineV1::transitions` as `(start, len)`; `None` when the
    /// buffer was full for it.
    pub transitions: Option<(u16, u16)>,
}

/// What a phase transition opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionTargetV1 {
    /// This phase.
    Phase(PhaseV1),
    /// The attempt's root closed.
    Closed,
}

/// One phase transition of one attempt, at the phase stack's own reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseTransitionV1 {
    /// The attempt.
    pub attempt: AttemptId,
    /// Nanoseconds after `armed_at`.
    pub at_ns: u64,
    /// What the transition opened.
    pub opened: TransitionTargetV1,
}

/// One arming's frozen timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchTimelineV1 {
    /// When the timeline was armed; every `at_ns` is relative to it.
    pub armed_at: Instant,
    /// The armed pane.
    pub pane_id: u64,
    /// The pane's window at arm, as `u64::from(WindowId)`.
    pub window: u64,
    /// Recorded events, at most 256.
    pub events: Vec<DispatchEventV1>,
    /// Renderer attempts, at most 64.
    pub attempts: Vec<AttemptRecordV1>,
    /// Phase transitions, at most 1024.
    pub transitions: Vec<PhaseTransitionV1>,
    /// The appearance witness's record, when one was armed.
    pub witness: Option<AppearanceRecordV1>,
    /// The witness's resolution state at take.
    pub witness_resolution: WitnessResolutionV1,
    /// Whether any buffer or sequence overflowed.
    pub overflow: bool,
}

/// The cell an appearance witness watches, and the identities it must keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearanceTargetV1 {
    /// The absolute row.
    pub abs_row: u64,
    /// The column.
    pub col: u16,
    /// The character expected at that cell.
    pub character: char,
    /// The scrollback eviction count the row is relative to.
    pub scrollback_evicted: u64,
    /// The screen epoch.
    pub screen_epoch: u64,
    /// The size generation.
    pub size_generation: u64,
}

/// What the appearance witness observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearanceRecordV1 {
    /// The target appeared in an observed section.
    Appeared {
        /// Nanoseconds after `armed_at`.
        at_ns: u64,
        /// The output generation its batch publishes.
        generation: u64,
        /// The first later section that lost it, if any.
        lost: Option<AppearanceLossV1>,
    },
    /// Before any recorded appearance, a section's pre-read already saw the target present: the first
    /// appearance happened unobserved. Terminal: no later absent→present transition is ever selected.
    FirstAppearanceUnobserved {
        /// Nanoseconds after `armed_at`.
        at_ns: u64,
    },
    /// Arm's own read saw the target present.
    AlreadyPresent,
    /// An identity the target depends on changed.
    IdentityChanged {
        /// Nanoseconds after `armed_at`.
        at_ns: u64,
    },
    /// A section captured before activation was not covered by arm's read.
    ArmGap,
    /// The target was never seen.
    NotSeen,
    /// Arm could not read the parser.
    ArmUnread,
}

/// When and in which generation an appeared target was lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearanceLossV1 {
    /// Nanoseconds after `armed_at`.
    pub at_ns: u64,
    /// The output generation its batch publishes.
    pub generation: u64,
}

impl super::App {
    /// Arm the dispatch timeline for the window that holds `pane_id`, with an optional appearance witness,
    /// before the measured phase or entry action. This build records nothing: it answers `GateOff` while
    /// the frame-counter gate is off and `NotRecorded` otherwise, reads no parser and allocates nothing.
    #[doc(hidden)]
    pub fn arm_dispatch_timeline_v1(
        &mut self,
        _pane_id: u64,
        _witness: Option<AppearanceTargetV1>,
    ) -> DispatchTimelineArmV1 {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is none, the gate is off and no build could record anything.
            return DispatchTimelineArmV1::GateOff;
        }
        DispatchTimelineArmV1::NotRecorded
    }

    /// Freeze, disarm and transfer what `token` recorded, exactly once. This build records nothing: it
    /// answers `GateOff` while the frame-counter gate is off and `NotRecorded` otherwise.
    #[doc(hidden)]
    pub fn take_dispatch_timeline_v1(
        &mut self,
        _token: DispatchTimelineToken,
    ) -> DispatchTimelineTakeV1 {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is none, the gate is off and no build could have recorded anything.
            return DispatchTimelineTakeV1::GateOff;
        }
        DispatchTimelineTakeV1::NotRecorded
    }
}

#[cfg(test)]
#[path = "dispatch_timeline_tests.rs"]
mod dispatch_timeline_tests;
