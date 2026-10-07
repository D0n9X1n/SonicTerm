//! The per-sample echo timeline: the event-loop and worker events between an echo's appearance and the
//! frame that presented it, frozen when `take_echo_watch` takes its token and transferred once.
//!
//! Each window admits one timeline owner, an arming identified by pane and token. The owner's
//! event-loop hooks write into the pane's watch slot under the slot lock, checked against the token
//! like every writer; the window keeps the flood ring and the accepted-tick metadata. The owner's
//! first take freezes the record and copies the ring, and `take_echo_timeline_v1` transfers it once.
//! The `V1` types are the frozen contract; a later change adds a `_v2` accessor with its own types.

use std::{
    sync::{atomic::Ordering, Arc, Weak},
    time::{Duration, Instant},
};

use winit::window::WindowId;

use super::{echo_watch::EchoWatch, ArmToken};

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

/// The clock every event-loop recorder hook reads, only after it found an owner; tests count each read.
pub(super) fn recorder_now() -> Instant {
    #[cfg(test)]
    RECORDER_CLOCK_READS.with(|reads| reads.set(reads.get() + 1));
    Instant::now()
}

#[cfg(test)]
thread_local! {
    /// Test-only: recorder clock reads on this thread.
    static RECORDER_CLOCK_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Test-only: slot-timeline and flood-ring allocations on this thread.
    static RECORDER_ALLOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: (recorder clock reads, recorder allocations) made so far on this thread.
#[cfg(test)]
pub(super) fn recorder_counts() -> (u64, u64) {
    (
        RECORDER_CLOCK_READS.with(std::cell::Cell::get),
        RECORDER_ALLOCATIONS.with(std::cell::Cell::get),
    )
}

/// Count one recorder allocation on this thread, in a test build.
fn note_recorder_allocation() {
    #[cfg(test)]
    RECORDER_ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
}

/// Events one slot's buffer holds; a further event sets `overflow` instead.
pub(super) const TIMELINE_EVENT_CAPACITY: usize = 64;
/// `PaneOutput` services the owner window's flood ring holds; a newer one evicts the oldest.
pub(super) const FLOOD_RING_CAPACITY: usize = 256;

/// One recorded event in its fixed internal form: time since arm, a kind code and three payload
/// slots. A `None` identity has its own code, never a zero slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TraceEvent {
    at_ns: u64,
    code: EventCode,
    slots: [u32; 3],
}

/// The variant, its enum payload and whether an optional tick identity is present.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventCode {
    ChunkRead,
    CheckNative,
    CheckMarked,
    CheckNone,
    TickKnown,
    TickUnknown,
    PermitKnown,
    PermitUnknown,
    Fallback,
    Admitted,
    DeferTimeout,
    DeferContention,
    DeferSync,
    DeferStreaming,
    Held,
    DiscardKnown,
    DiscardUnknown,
    ResetKnown,
    ResetUnknown,
    PausedKnown,
    PausedUnknown,
    DispatchEntry,
    DispatchReturn,
    RenderEnter,
    RenderExit,
}

/// The buffer's unused entries; only the first `len` entries are ever decoded.
const UNUSED_EVENT: TraceEvent = TraceEvent { at_ns: 0, code: EventCode::ChunkRead, slots: [0; 3] };

const _: () = assert!(std::mem::size_of::<TraceEvent>() == 24);
const _: () = assert!(std::mem::size_of::<[TraceEvent; TIMELINE_EVENT_CAPACITY]>() == 1_536);
const _: () = assert!(std::mem::size_of::<FloodEntry>() == 16);
const _: () = assert!(std::mem::size_of::<[FloodEntry; FLOOD_RING_CAPACITY]>() == 4_096);

/// `value`'s low and high halves, for two payload slots; `join_u64` restores it exactly.
fn split_u64(value: u64) -> (u32, u32) {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (low, high)
}

/// The `u64` that `split_u64` split into `low` and `high`.
fn join_u64(low: u32, high: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

impl TraceEvent {
    /// The internal form of `kind` at `at_ns`; `decode` returns exactly `kind`.
    pub(super) fn encode(at_ns: u64, kind: EchoTimelineKindV1) -> Self {
        let (code, slots) = match kind {
            EchoTimelineKindV1::LatestChunkRead => (EventCode::ChunkRead, [0; 3]),
            EchoTimelineKindV1::OutputCheck { loop_seq, generation, outcome } => {
                let code = match outcome {
                    OutputCheckOutcomeV1::NativeRequest => EventCode::CheckNative,
                    OutputCheckOutcomeV1::MarkedInFlight => EventCode::CheckMarked,
                    OutputCheckOutcomeV1::None => EventCode::CheckNone,
                };
                let (low, high) = split_u64(generation);
                (code, [loop_seq, low, high])
            }
            EchoTimelineKindV1::Tick { loop_seq, identity: Some(identity) } => {
                (EventCode::TickKnown, [loop_seq, identity.tick_seq, identity.generation])
            }
            EchoTimelineKindV1::Tick { loop_seq, identity: None } => {
                (EventCode::TickUnknown, [loop_seq, 0, 0])
            }
            EchoTimelineKindV1::Admission { loop_seq, dispatch_seq, decision } => {
                let (code, tick_seq) = match decision {
                    AdmissionDecisionV1::Permit { tick_seq: Some(tick_seq) } => {
                        (EventCode::PermitKnown, tick_seq)
                    }
                    AdmissionDecisionV1::Permit { tick_seq: None } => (EventCode::PermitUnknown, 0),
                    AdmissionDecisionV1::Fallback => (EventCode::Fallback, 0),
                    AdmissionDecisionV1::Admitted => (EventCode::Admitted, 0),
                    AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout) => {
                        (EventCode::DeferTimeout, 0)
                    }
                    AdmissionDecisionV1::Deferred(DeferReasonV1::Contention) => {
                        (EventCode::DeferContention, 0)
                    }
                    AdmissionDecisionV1::Deferred(DeferReasonV1::Sync) => (EventCode::DeferSync, 0),
                    AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming) => {
                        (EventCode::DeferStreaming, 0)
                    }
                    AdmissionDecisionV1::Held => (EventCode::Held, 0),
                };
                (code, [loop_seq, dispatch_seq, tick_seq])
            }
            EchoTimelineKindV1::PermitCleared { loop_seq, tick_seq, cause } => {
                let code = match (cause, tick_seq.is_some()) {
                    (PermitClearCauseV1::AdmissionDiscard, true) => EventCode::DiscardKnown,
                    (PermitClearCauseV1::AdmissionDiscard, false) => EventCode::DiscardUnknown,
                    (PermitClearCauseV1::LinkReset, true) => EventCode::ResetKnown,
                    (PermitClearCauseV1::LinkReset, false) => EventCode::ResetUnknown,
                    (PermitClearCauseV1::LinkPaused, true) => EventCode::PausedKnown,
                    (PermitClearCauseV1::LinkPaused, false) => EventCode::PausedUnknown,
                };
                (code, [loop_seq, tick_seq.unwrap_or_default(), 0])
            }
            EchoTimelineKindV1::DispatchEntry { loop_seq, dispatch_seq } => {
                (EventCode::DispatchEntry, [loop_seq, dispatch_seq, 0])
            }
            EchoTimelineKindV1::DispatchReturn { loop_seq, dispatch_seq } => {
                (EventCode::DispatchReturn, [loop_seq, dispatch_seq, 0])
            }
            EchoTimelineKindV1::RenderEnter { dispatch_seq } => {
                (EventCode::RenderEnter, [dispatch_seq, 0, 0])
            }
            EchoTimelineKindV1::RenderExit { dispatch_seq } => {
                (EventCode::RenderExit, [dispatch_seq, 0, 0])
            }
        };
        Self { at_ns, code, slots }
    }

    /// The v1 event this internal form encodes.
    pub(super) fn decode(self) -> EchoTimelineEventV1 {
        let [first, second, third] = self.slots;
        let check = |outcome: OutputCheckOutcomeV1| EchoTimelineKindV1::OutputCheck {
            loop_seq: first,
            generation: join_u64(second, third),
            outcome,
        };
        let admission = |decision: AdmissionDecisionV1| EchoTimelineKindV1::Admission {
            loop_seq: first,
            dispatch_seq: second,
            decision,
        };
        let cleared = |cause: PermitClearCauseV1, known: bool| EchoTimelineKindV1::PermitCleared {
            loop_seq: first,
            tick_seq: known.then_some(second),
            cause,
        };
        let kind = match self.code {
            EventCode::ChunkRead => EchoTimelineKindV1::LatestChunkRead,
            EventCode::CheckNative => check(OutputCheckOutcomeV1::NativeRequest),
            EventCode::CheckMarked => check(OutputCheckOutcomeV1::MarkedInFlight),
            EventCode::CheckNone => check(OutputCheckOutcomeV1::None),
            EventCode::TickKnown => EchoTimelineKindV1::Tick {
                loop_seq: first,
                identity: Some(TickIdentityV1 { tick_seq: second, generation: third }),
            },
            EventCode::TickUnknown => EchoTimelineKindV1::Tick { loop_seq: first, identity: None },
            EventCode::PermitKnown => {
                admission(AdmissionDecisionV1::Permit { tick_seq: Some(third) })
            }
            EventCode::PermitUnknown => admission(AdmissionDecisionV1::Permit { tick_seq: None }),
            EventCode::Fallback => admission(AdmissionDecisionV1::Fallback),
            EventCode::Admitted => admission(AdmissionDecisionV1::Admitted),
            EventCode::DeferTimeout => {
                admission(AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout))
            }
            EventCode::DeferContention => {
                admission(AdmissionDecisionV1::Deferred(DeferReasonV1::Contention))
            }
            EventCode::DeferSync => admission(AdmissionDecisionV1::Deferred(DeferReasonV1::Sync)),
            EventCode::DeferStreaming => {
                admission(AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming))
            }
            EventCode::Held => admission(AdmissionDecisionV1::Held),
            EventCode::DiscardKnown => cleared(PermitClearCauseV1::AdmissionDiscard, true),
            EventCode::DiscardUnknown => cleared(PermitClearCauseV1::AdmissionDiscard, false),
            EventCode::ResetKnown => cleared(PermitClearCauseV1::LinkReset, true),
            EventCode::ResetUnknown => cleared(PermitClearCauseV1::LinkReset, false),
            EventCode::PausedKnown => cleared(PermitClearCauseV1::LinkPaused, true),
            EventCode::PausedUnknown => cleared(PermitClearCauseV1::LinkPaused, false),
            EventCode::DispatchEntry => {
                EchoTimelineKindV1::DispatchEntry { loop_seq: first, dispatch_seq: second }
            }
            EventCode::DispatchReturn => {
                EchoTimelineKindV1::DispatchReturn { loop_seq: first, dispatch_seq: second }
            }
            EventCode::RenderEnter => EchoTimelineKindV1::RenderEnter { dispatch_seq: first },
            EventCode::RenderExit => EchoTimelineKindV1::RenderExit { dispatch_seq: first },
        };
        EchoTimelineEventV1 { at_ns: self.at_ns, kind }
    }
}

/// The watched window's permit when an owner arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InitialPermit {
    /// No valid permit was held.
    Absent,
    /// A valid permit was held, stored by this tick.
    Known(PermitSnapshotV1),
    /// A valid permit was held, but its tick's identity is unavailable or not representable.
    Unknown,
}

/// One owner arming's timeline in the pane's watch slot: allocated at arm, frozen by the owner's first
/// take, and freed on transfer, rearm or release.
#[derive(Debug)]
pub(super) struct SlotTimeline {
    events: [TraceEvent; TIMELINE_EVENT_CAPACITY],
    len: usize,
    overflow: bool,
    chunk_timestamps: bool,
    initial_permit: InitialPermit,
    /// The last recorded output check's generation and outcome; a repeat of it is not recorded.
    last_check: Option<(u64, OutputCheckOutcomeV1)>,
    /// The highest generation a recorded check carried; recording stops once it reaches the appearance.
    max_check_generation: Option<u64>,
    /// The owner window's flood entries, copied by the first take; `None` until then.
    flood: Option<FrozenFlood>,
}

impl SlotTimeline {
    /// A fresh, empty timeline for one owner arming.
    pub(super) fn new(chunk_timestamps: bool, initial_permit: InitialPermit) -> Box<Self> {
        note_recorder_allocation();
        Box::new(Self {
            events: [UNUSED_EVENT; TIMELINE_EVENT_CAPACITY],
            len: 0,
            overflow: false,
            chunk_timestamps,
            initial_permit,
            last_check: None,
            max_check_generation: None,
            flood: None,
        })
    }

    /// Append `kind` at `elapsed` since arm. A `None` kind (a sequence that did not fit `u32`), an
    /// elapsed time that does not fit `u64` nanoseconds, or a full buffer drops it and sets `overflow`.
    pub(super) fn record(&mut self, elapsed: Duration, kind: Option<EchoTimelineKindV1>) {
        let (Some(kind), Ok(at_ns)) = (kind, u64::try_from(elapsed.as_nanos())) else {
            // When: `kind` or `at_ns` is not representable, the event is dropped and the sample disqualified.
            self.overflow = true;
            return;
        };
        let Some(slot) = self.events.get_mut(self.len) else {
            // When: `len` reached the capacity, the event is dropped and the sample disqualified.
            self.overflow = true;
            return;
        };
        *slot = TraceEvent::encode(at_ns, kind);
        self.len += 1;
    }

    /// Record a visible-output check of the watched pane, compressed losslessly: the first check
    /// since arm and every change of `(generation, outcome)` are kept, a repeat never replaces the
    /// first occurrence, and recording stops only once `appearance` is known and a kept check reached it.
    pub(super) fn note_check(
        &mut self,
        elapsed: Duration,
        loop_seq: Option<u32>,
        generation: u64,
        outcome: OutputCheckOutcomeV1,
        appearance: Option<u64>,
    ) {
        let qualified = appearance
            .zip(self.max_check_generation)
            .is_some_and(|(appeared, reached)| reached >= appeared);
        if qualified {
            // When: `qualified`, the earliest check at or past the appearance is already kept.
            return;
        }
        if self.last_check == Some((generation, outcome)) {
            // When: the check repeats `last_check`, the first occurrence's time stands.
            return;
        }
        self.last_check = Some((generation, outcome));
        self.max_check_generation =
            Some(self.max_check_generation.map_or(generation, |reached| reached.max(generation)));
        let kind = loop_seq.map(|loop_seq| EchoTimelineKindV1::OutputCheck {
            loop_seq,
            generation,
            outcome,
        });
        self.record(elapsed, kind);
    }

    /// Keep the owner window's flood entries with the record; the owner's take calls this once.
    pub(super) fn freeze(&mut self, flood: FrozenFlood) {
        self.flood = Some(flood);
    }

    /// Whether the owner's take froze this record.
    pub(super) fn frozen(&self) -> bool {
        self.flood.is_some()
    }

    /// The frozen record as the v1 contract, or `None` when no owner's take froze it.
    pub(super) fn transfer(self: Box<Self>) -> Option<EchoTimelineV1> {
        let timeline = *self;
        let flood = timeline.flood?;
        let (initial_permit, initial_permit_unknown) = match timeline.initial_permit {
            InitialPermit::Absent => (None, false),
            InitialPermit::Known(snapshot) => (Some(snapshot), false),
            InitialPermit::Unknown => (None, true),
        };
        Some(EchoTimelineV1 {
            chunk_timestamps: timeline.chunk_timestamps,
            initial_permit,
            initial_permit_unknown,
            events: timeline.events[..timeline.len].iter().map(|event| event.decode()).collect(),
            overflow: timeline.overflow,
            flood_services: flood.services,
            flood_evicted_through: flood.evicted_through,
        })
    }
}

/// The owner window's flood entries, oldest first, as copied by the owner's take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FrozenFlood {
    services: Vec<FloodServiceV1>,
    evicted_through: Option<u32>,
}

#[cfg(test)]
impl FrozenFlood {
    /// Test-only: no flood entries, for a worker-level test that freezes a slot without a window.
    pub(super) fn empty() -> Self {
        Self { services: Vec::new(), evicted_through: None }
    }
}

/// One completed `PaneOutput` service in the owner window.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FloodEntry {
    loop_seq: u32,
    pane_id: u64,
}

/// The owner window's last `FLOOD_RING_CAPACITY` pane services since arm; eviction is recorded.
#[derive(Debug)]
pub(super) struct FloodRing {
    entries: [FloodEntry; FLOOD_RING_CAPACITY],
    /// The slot the next service overwrites.
    next: usize,
    /// Every slot holds a service, so `next` names the oldest.
    wrapped: bool,
    evicted_through: Option<u32>,
}

impl FloodRing {
    /// An empty ring, allocated when its window's owner arms.
    fn new() -> Box<Self> {
        note_recorder_allocation();
        Box::new(Self {
            entries: [FloodEntry { loop_seq: 0, pane_id: 0 }; FLOOD_RING_CAPACITY],
            next: 0,
            wrapped: false,
            evicted_through: None,
        })
    }

    /// Append one service, evicting the oldest once the ring is full.
    fn push(&mut self, loop_seq: u32, pane_id: u64) {
        if self.wrapped {
            // A full ring's slot at `next` holds the oldest service; its `loop_seq` marks the eviction.
            self.evicted_through = Some(self.entries[self.next].loop_seq);
        }
        self.entries[self.next] = FloodEntry { loop_seq, pane_id };
        self.next = (self.next + 1) % FLOOD_RING_CAPACITY;
        self.wrapped |= self.next == 0;
    }

    /// A copy of the retained services, oldest first, with the eviction mark.
    fn freeze(&self) -> FrozenFlood {
        let (older, newer) = self.entries.split_at(self.next);
        // Before the ring wraps, the slots from `next` on were never written.
        let oldest_first: &[FloodEntry] = Some(newer).filter(|_| self.wrapped).unwrap_or(&[]);
        let services = oldest_first
            .iter()
            .chain(older)
            .map(|entry| FloodServiceV1 { loop_seq: entry.loop_seq, pane_id: entry.pane_id })
            .collect();
        FrozenFlood { services, evicted_through: self.evicted_through }
    }
}

/// An accepted display-link tick's diagnostic metadata, kept apart from the production permit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TickMeta {
    /// The window's accepted-tick count, never reset at arm.
    tick_seq: u64,
    /// The link generation the tick was accepted under.
    generation: u64,
    /// The accepted tick's dispatch instant, also its `Tick` event's instant.
    delivered_at: Instant,
}

impl TickMeta {
    /// The tick's identity, or `None` when either number does not fit `u32`; never truncated.
    fn identity(self) -> Option<TickIdentityV1> {
        Some(TickIdentityV1 {
            tick_seq: u32::try_from(self.tick_seq).ok()?,
            generation: u32::try_from(self.generation).ok()?,
        })
    }
}

/// One explicit observation of the watched pane: the owner's token, the generation loaded and the
/// instant of that load, which is the check's time whatever work follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WatchedObservation {
    token: ArmToken,
    generation: u64,
    observed_at: Instant,
}

/// A window's timeline state: accepted-tick metadata while the gate is on, and at most one owner.
#[derive(Debug, Default)]
pub(super) struct WindowTimeline {
    tick_seq: u64,
    last_tick: Option<TickMeta>,
    owner: Option<TimelineOwner>,
}

/// The arming that owns its window's timeline: the watched pane, its token and watch, the
/// per-window sequences reset at arm, and the flood ring. The watch is held weakly, so an owner
/// never extends a removed pane's watch beyond its pane and worker handles.
#[derive(Debug)]
struct TimelineOwner {
    pane_id: u64,
    token: ArmToken,
    watch: Weak<EchoWatch>,
    loop_seq: u64,
    dispatch_seq: u64,
    /// The watched `RedrawRequested` dispatch in progress.
    current_dispatch: Option<u64>,
    flood: Box<FloodRing>,
}

// Lifecycle: TimelineOwner drop frees its unfrozen slot timeline, so an ended owner or closed window
// leaves no diagnostic buffer.
impl Drop for TimelineOwner {
    fn drop(&mut self) {
        if let Some(watch) = self.watch.upgrade() {
            // A live watch's unfrozen timeline is released under the slot lock.
            watch.end_timeline(self.token);
        }
    }
}

impl TimelineOwner {
    /// The next per-window event-loop sequence; `None` once it no longer fits `u32`.
    fn next_loop_seq(&mut self) -> Option<u32> {
        self.loop_seq = self.loop_seq.saturating_add(1);
        u32::try_from(self.loop_seq).ok()
    }

    /// Write the slot timeline at the current instant, under the slot lock and the token check.
    fn write(&self, change: impl FnOnce(&mut SlotTimeline, Option<u64>, Duration)) {
        self.write_at(recorder_now(), change);
    }

    /// Record an event whose `loop_seq` did not fit `u32` as lost, whatever else the hook kept or
    /// compressed away: the timeline's overflow is set at the hook's own instant.
    fn mark_lost(&self, loop_seq: Option<u32>) {
        if loop_seq.is_none() {
            // A lost sequence disqualifies the sample.
            self.write(|timeline, _, elapsed| timeline.record(elapsed, None));
        }
    }

    /// Write the slot timeline at `at`; a watch already freed records nothing.
    fn write_at(&self, at: Instant, change: impl FnOnce(&mut SlotTimeline, Option<u64>, Duration)) {
        if let Some(watch) = self.watch.upgrade() {
            // A live watch takes the write through its slot lock and token check.
            watch.write_timeline(self.token, at, change);
        }
    }

    /// The current dispatch's `dispatch_seq`: `None` outside a watched dispatch, `Some(None)` when
    /// it does not fit `u32`.
    fn dispatch(&self) -> Option<Option<u32>> {
        self.current_dispatch.map(|dispatch_seq| u32::try_from(dispatch_seq).ok())
    }
}

impl WindowTimeline {
    /// The pane this window's timeline owner watches.
    pub(super) fn watched_pane(&self) -> Option<u64> {
        self.owner.as_ref().map(|owner| owner.pane_id)
    }

    /// The identity of the tick that stored a permit of `generation`, when the last accepted tick
    /// did and its identity is representable.
    fn identity_for(&self, generation: u64) -> Option<TickIdentityV1> {
        self.last_tick.filter(|tick| tick.generation == generation).and_then(TickMeta::identity)
    }

    /// The `tick_seq` an admission consuming a permit of `generation` reports.
    pub(super) fn permit_tick_seq(&self, generation: Option<u64>) -> Option<u32> {
        generation.and_then(|generation| self.identity_for(generation)).map(|tick| tick.tick_seq)
    }

    /// Make `pane_id`'s arming this window's owner and return its slot timeline. While another
    /// pane's owner is active, the arming records nothing and that owner is untouched.
    pub(super) fn admit(
        &mut self,
        pane_id: u64,
        token: ArmToken,
        watch: &Arc<EchoWatch>,
        chunk_timestamps: bool,
        valid_permit: Option<u64>,
    ) -> Option<Box<SlotTimeline>> {
        if self.owner.as_ref().is_some_and(|owner| owner.pane_id != pane_id) {
            // When: another pane owns the timeline, this arming gets none; the owner keeps its ring.
            return None;
        }
        let initial_permit = match valid_permit {
            None => InitialPermit::Absent,
            Some(generation) => self
                .last_tick
                .filter(|tick| tick.generation == generation)
                .and_then(|tick| {
                    let identity = tick.identity()?;
                    Some(PermitSnapshotV1 { identity, delivered_at: tick.delivered_at })
                })
                .map_or(InitialPermit::Unknown, InitialPermit::Known),
        };
        // A same-pane rearm drops the previous owner, which releases its unfrozen slot timeline.
        self.owner = Some(TimelineOwner {
            pane_id,
            token,
            watch: Arc::downgrade(watch),
            loop_seq: 0,
            dispatch_seq: 0,
            current_dispatch: None,
            flood: FloodRing::new(),
        });
        Some(SlotTimeline::new(chunk_timestamps, initial_permit))
    }

    /// The owner's flood entries, copied, when `pane_id` and `token` name this window's owner.
    pub(super) fn frozen_flood_for(&self, pane_id: u64, token: ArmToken) -> Option<FrozenFlood> {
        self.owner
            .as_ref()
            .filter(|owner| owner.pane_id == pane_id && owner.token == token)
            .map(|owner| owner.flood.freeze())
    }

    /// End the owner when it watches pane `pane_id`; its drop releases an unfrozen slot timeline.
    pub(super) fn end_for_pane(&mut self, pane_id: u64) {
        if self.watched_pane() == Some(pane_id) {
            self.owner = None;
        }
    }

    /// End the owner when it watches `watch`; its drop releases an unfrozen slot timeline.
    pub(super) fn end_for_watch(&mut self, watch: &Arc<EchoWatch>) {
        if self.owner.as_ref().is_some_and(|owner| owner.watch.as_ptr() == Arc::as_ptr(watch)) {
            self.owner = None;
        }
    }

    /// Keep an accepted tick's metadata and, for an owner, record its `Tick` at the same instant.
    /// The window calls this only while the App's gate is on.
    pub(super) fn accept_tick(&mut self, generation: u64, delivered_at: Instant) {
        self.tick_seq = self.tick_seq.saturating_add(1);
        let tick = TickMeta { tick_seq: self.tick_seq, generation, delivered_at };
        self.last_tick = Some(tick);
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, only the metadata is kept.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        let identity = tick.identity();
        owner.write_at(delivered_at, |timeline, _, elapsed| {
            timeline.record(
                elapsed,
                loop_seq.map(|loop_seq| EchoTimelineKindV1::Tick { loop_seq, identity }),
            );
        });
    }

    /// Record that a held permit of `generation` was cleared for `cause`.
    pub(super) fn note_permit_cleared(&mut self, generation: u64, cause: PermitClearCauseV1) {
        let tick_seq = self.identity_for(generation).map(|identity| identity.tick_seq);
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, no timeline records the clear.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.map(|loop_seq| EchoTimelineKindV1::PermitCleared {
                loop_seq,
                tick_seq,
                cause,
            });
            timeline.record(elapsed, kind);
        });
    }

    /// Record one completed `PaneOutput` service of `serviced_pane`: its flood entry, and the
    /// watched pane's check at `watched_generation` with the Output request's `outcome`.
    pub(super) fn note_output_service(
        &mut self,
        serviced_pane: u64,
        observation: Option<WatchedObservation>,
        outcome: OutputCheckOutcomeV1,
    ) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, nothing is recorded and no lock is taken.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        if let Some(loop_seq) = loop_seq {
            owner.flood.push(loop_seq, serviced_pane);
        }
        if let Some(observation) = observation.filter(|seen| seen.token == owner.token) {
            // The check is written at its observation's instant, not after the request work that followed.
            owner.write_at(observation.observed_at, |timeline, appearance, elapsed| {
                let generation = observation.generation;
                timeline.note_check(elapsed, loop_seq, generation, outcome, appearance);
            });
        }
        // A lost flood entry sets overflow even when its check was compressed or recording had stopped.
        owner.mark_lost(loop_seq);
    }

    /// Record one admission decision of the watched dispatch, the streaming check that preceded it
    /// when it ran (`check`, outcome `None`), and a held permit it discarded.
    pub(super) fn note_admission(
        &mut self,
        decision: AdmissionDecisionV1,
        discarded: Option<u64>,
        check: Option<WatchedObservation>,
    ) {
        let discard_tick =
            discarded.map(|generation| self.identity_for(generation).map(|tick| tick.tick_seq));
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, nothing is recorded and no lock is taken.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        let dispatch = owner.dispatch();
        if let Some(observation) = check.filter(|seen| seen.token == owner.token) {
            // The streaming check is written at its observation's instant, before the decision.
            owner.write_at(observation.observed_at, |timeline, appearance, elapsed| {
                let (generation, outcome) = (observation.generation, OutputCheckOutcomeV1::None);
                timeline.note_check(elapsed, loop_seq, generation, outcome, appearance);
            });
        }
        owner.mark_lost(loop_seq);
        owner.write(|timeline, _, elapsed| {
            if let Some(dispatch_seq) = dispatch {
                // Inside a watched dispatch: an unrepresentable sequence drops the decision.
                let kind = loop_seq.zip(dispatch_seq).map(|(loop_seq, dispatch_seq)| {
                    EchoTimelineKindV1::Admission { loop_seq, dispatch_seq, decision }
                });
                timeline.record(elapsed, kind);
            }
            if let Some(tick_seq) = discard_tick {
                let cause = PermitClearCauseV1::AdmissionDiscard;
                let kind = loop_seq.map(|loop_seq| EchoTimelineKindV1::PermitCleared {
                    loop_seq,
                    tick_seq,
                    cause,
                });
                timeline.record(elapsed, kind);
            }
        });
    }

    /// Start one watched `RedrawRequested` dispatch and record its entry.
    pub(super) fn note_dispatch_entry(&mut self) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, the dispatch is not watched.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.dispatch_seq = owner.dispatch_seq.saturating_add(1);
        owner.current_dispatch = Some(owner.dispatch_seq);
        let dispatch = owner.dispatch().flatten();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.zip(dispatch).map(|(loop_seq, dispatch_seq)| {
                EchoTimelineKindV1::DispatchEntry { loop_seq, dispatch_seq }
            });
            timeline.record(elapsed, kind);
        });
    }

    /// End the watched dispatch in progress, if any, and record its return.
    pub(super) fn note_dispatch_return(&mut self) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, no watched dispatch is in progress.
            return;
        };
        let dispatch = owner.dispatch();
        owner.current_dispatch = None;
        let Some(dispatch_seq) = dispatch else {
            // When: `dispatch` is None, the owner armed during this dispatch; it has no entry.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.zip(dispatch_seq).map(|(loop_seq, dispatch_seq)| {
                EchoTimelineKindV1::DispatchReturn { loop_seq, dispatch_seq }
            });
            timeline.record(elapsed, kind);
        });
    }

    /// The marker the renderer call of the watched dispatch in progress records through.
    pub(super) fn render_marker(&self) -> Option<RenderMarker> {
        let owner = self.owner.as_ref()?;
        let dispatch_seq = owner.dispatch()?;
        Some(RenderMarker { watch: owner.watch.upgrade()?, token: owner.token, dispatch_seq })
    }
}

/// What a render adapter holds across its renderer call: an owned handle to the watched slot and
/// the dispatch's sequence. It holds no lock and no borrow, and its drop only releases the handle.
#[derive(Debug)]
pub(super) struct RenderMarker {
    watch: Arc<EchoWatch>,
    token: ArmToken,
    dispatch_seq: Option<u32>,
}

impl RenderMarker {
    /// Record `RenderEnter`, immediately before the renderer call.
    pub(super) fn enter(&self) {
        let kind =
            self.dispatch_seq.map(|dispatch_seq| EchoTimelineKindV1::RenderEnter { dispatch_seq });
        self.record(recorder_now(), kind);
    }

    /// Record `RenderExit` at `returned_at`, the renderer's return as the adapter read it once.
    pub(super) fn exit_at(&self, returned_at: Instant) {
        let kind =
            self.dispatch_seq.map(|dispatch_seq| EchoTimelineKindV1::RenderExit { dispatch_seq });
        self.record(returned_at, kind);
    }

    /// Write `kind` at `at` under the slot lock and token check.
    fn record(&self, at: Instant, kind: Option<EchoTimelineKindV1>) {
        self.watch.write_timeline(self.token, at, |timeline, _, elapsed| {
            timeline.record(elapsed, kind);
        });
    }
}

/// The v1 reason for a winning deferral rule.
pub(super) fn defer_reason(rule: super::frame_counters::DeferRule) -> DeferReasonV1 {
    match rule {
        super::frame_counters::DeferRule::Timeout => DeferReasonV1::Timeout,
        super::frame_counters::DeferRule::Contention => DeferReasonV1::Contention,
        super::frame_counters::DeferRule::Sync => DeferReasonV1::Sync,
        super::frame_counters::DeferRule::Streaming => DeferReasonV1::Streaming,
    }
}

impl super::WindowState {
    /// The watched pane's output generation, loaded explicitly (Acquire) whether or not a
    /// short-circuiting visibility check reaches that pane, with the owner's token and the load's
    /// instant; `None`, with no clock read, without an owner.
    // Ordering: output_generation Acquire pairs with the worker's Release publication, as in visible_output_advanced.
    pub(super) fn watched_observation(&self) -> Option<WatchedObservation> {
        let owner = self.redraw.timeline.owner.as_ref()?;
        let pane = self.panes.get(&owner.pane_id)?;
        let generation = pane.output_generation.load(Ordering::Acquire);
        Some(WatchedObservation { token: owner.token, generation, observed_at: recorder_now() })
    }
}

impl super::App {
    /// Transfer, exactly once, the timeline frozen when `take_echo_watch` took `token` for pane
    /// `pane_id`. Takes no parser lock. An arming that did not own its window's timeline, or whose
    /// timeline ended before its take, answers `NotRecorded`.
    #[doc(hidden)]
    pub fn take_echo_timeline_v1(&mut self, pane_id: u64, token: ArmToken) -> EchoTimelineTakeV1 {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is None, the gate is off and no pane has a watch.
            return EchoTimelineTakeV1::GateOff;
        }
        let Some(pane) = self.find_pane(pane_id) else {
            // When: no window holds pane_id, its record is unreachable.
            return EchoTimelineTakeV1::NoPane;
        };
        let Some(counters) = pane.frame_counters.as_ref() else {
            // When: the pane has no counter handles, it has no watch.
            return EchoTimelineTakeV1::GateOff;
        };
        counters.echo.transfer_timeline(token)
    }

    /// The render marker of window `id`'s watched dispatch in progress.
    pub(super) fn render_marker(&self, id: WindowId) -> Option<RenderMarker> {
        self.windows.get(&id).and_then(|window| window.redraw.timeline.render_marker())
    }

    /// Record the entry of window `id`'s `RedrawRequested` dispatch when its timeline has an owner.
    pub(super) fn note_dispatch_entry(&mut self, id: WindowId) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.redraw.timeline.note_dispatch_entry();
        }
    }

    /// Record the return of window `id`'s watched dispatch, if one is in progress.
    pub(super) fn note_dispatch_return(&mut self, id: WindowId) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.redraw.timeline.note_dispatch_return();
        }
    }
}

#[cfg(test)]
#[path = "echo_timeline_tests.rs"]
mod echo_timeline_tests;
