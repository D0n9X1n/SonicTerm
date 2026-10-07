//! The per-sample echo timeline of S2's credited samples, `echo_timeline` schema 1.
//!
//! The App's frozen V1 record is mirrored in the harness's own types, so the analysis and its tests build
//! in every tree. Only the adapter names the V1 accessor, behind
//! `#[cfg(all(feature = "perf-echo-trace", perf_echo_timeline_api))]`; the complementary build reports the
//! timeline `unavailable` with reason `cfg-off`. Each credited sample is bound to the dispatch pair of its
//! credited forward and reduced to the frozen per-sample fields; the populations are recomputed from them.
//! Every distinction reported is observable through V1 and the harness's own context: an unavailable
//! record never invents a token, window, dispatch sequence, permit absence or read-stamp state.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::record::{duration_ns, ArmState, Split};

/// The `echo_timeline` schema version written beside each credited S2 sample.
pub(crate) const TIMELINE_SCHEMA: u32 = 1;

/// Whether this build names the App's V1 timeline accessor.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const API_ENABLED: bool = cfg!(all(feature = "perf-echo-trace", perf_echo_timeline_api));

/// The names of the seven additive parts, in order; they sum exactly to the credited latency.
pub(crate) const PART_NAMES: [&str; 7] = [
    "input_to_parse",
    "parse_to_publication",
    "publication_to_frame_request",
    "frame_request_to_entry",
    "entry_to_render",
    "render",
    "render_exit_to_credited_end",
];

/// What a visible-output check did.
// Constructed only by the timeline_api adapter, which exists only with its cfg; tests build every variant.
#[cfg_attr(not(all(feature = "perf-echo-trace", perf_echo_timeline_api)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    /// A new native redraw request was issued.
    NativeRequest,
    /// The Output cause was marked onto a request in flight.
    MarkedInFlight,
    /// No Output-caused request.
    None,
}

impl CheckOutcome {
    /// The outcome's name in `result.json`.
    fn as_str(self) -> &'static str {
        match self {
            Self::NativeRequest => "native_request",
            Self::MarkedInFlight => "marked_in_flight",
            Self::None => "none",
        }
    }
}

/// One admission decision; deferral reasons are not needed by the analysis.
// Constructed only by the timeline_api adapter, which exists only with its cfg; tests build every variant.
#[cfg_attr(not(all(feature = "perf-echo-trace", perf_echo_timeline_api)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// A tick's permit was consumed; `tick_seq` is `None` when its identity is unavailable.
    Permit {
        /// The consumed permit's tick.
        tick_seq: Option<u32>,
    },
    /// The fallback ceiling admitted a link-paced frame.
    Fallback,
    /// Any other admission.
    Admitted,
    /// Deferred by a rule.
    Deferred,
    /// Refused before pacing.
    Held,
}

/// An accepted tick's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TickId {
    /// The window's accepted-tick sequence.
    pub(crate) tick_seq: u32,
    /// The link generation.
    pub(crate) generation: u32,
}

/// What one recorded event is.
// Constructed only by the timeline_api adapter, which exists only with its cfg; tests build every variant.
#[cfg_attr(not(all(feature = "perf-echo-trace", perf_echo_timeline_api)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    /// The appearing section's latest chunk read.
    ChunkRead,
    /// A visible-output check of the watched pane.
    Check {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The generation the check read.
        generation: u64,
        /// What the check did.
        outcome: CheckOutcome,
    },
    /// An accepted tick stored a permit.
    Tick {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The tick's identity, when representable.
        identity: Option<TickId>,
    },
    /// One admission decision.
    Admission {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The dispatch it belongs to.
        dispatch_seq: u32,
        /// The decision.
        decision: Decision,
    },
    /// A held permit was cleared without being consumed.
    Cleared {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The cleared permit's tick, when known.
        tick_seq: Option<u32>,
    },
    /// A watched `RedrawRequested` dispatch began.
    Entry {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The dispatch.
        dispatch_seq: u32,
    },
    /// That dispatch returned.
    Return {
        /// Event-loop sequence.
        loop_seq: u32,
        /// The dispatch.
        dispatch_seq: u32,
    },
    /// Immediately before the dispatch's renderer call.
    RenderEnter {
        /// The dispatch.
        dispatch_seq: u32,
    },
    /// The renderer call's return.
    RenderExit {
        /// The dispatch.
        dispatch_seq: u32,
    },
}

/// One recorded event: nanoseconds after the arm, and what it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    /// Integer nanoseconds since the arm.
    pub(crate) at_ns: u64,
    /// What happened.
    pub(crate) kind: EventKind,
}

/// A permit held at arm, with its tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldAtArm {
    /// The tick that stored it.
    pub(crate) identity: TickId,
    /// When that tick was accepted.
    pub(crate) delivered_at: Instant,
}

/// One arming's frozen timeline, in the harness's types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// The watch's arm instant, from the taken echo record; every `at_ns` counts from it.
    pub(crate) armed_at: Instant,
    /// Whether the pane's PTY stamped its reads.
    pub(crate) chunk_timestamps: bool,
    /// The permit held at arm, when its identity is known.
    pub(crate) initial_permit: Option<HeldAtArm>,
    /// A permit was held at arm with an unavailable identity.
    pub(crate) initial_permit_unknown: bool,
    /// The events, in record order.
    pub(crate) events: Vec<Event>,
    /// An event was dropped.
    pub(crate) overflow: bool,
    /// The owner window's `PaneOutput` services as (`loop_seq`, pane), oldest first.
    pub(crate) flood: Vec<(u32, u64)>,
    /// The highest evicted `loop_seq`.
    pub(crate) flood_evicted_through: Option<u32>,
}

/// What the timeline accessor gave for one credited sample.
// Constructed only by the timeline_api adapter, which exists only with its cfg; tests build every variant.
#[cfg_attr(not(all(feature = "perf-echo-trace", perf_echo_timeline_api)), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimelineTake {
    /// The frozen record.
    Recorded(Record),
    /// No record exists for a reason V1 reports: `cfg-off`, `not-recorded`, `gate-off`, `no-pane`, `exhausted`.
    Unavailable(&'static str),
    /// The accessor answered inconsistently with the harness's own protocol.
    Rejected(&'static str),
}

/// The harness's context of one credited sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Credited {
    /// When the character was injected.
    pub(crate) injected: Instant,
    /// When the credited forward began.
    pub(crate) forward_started: Instant,
    /// When the credited forward ended: the credited end.
    pub(crate) ended: Instant,
    /// The existing split, present only for split reason `split`.
    pub(crate) split: Option<Split>,
    /// The existing split reason.
    pub(crate) split_reason: &'static str,
    /// The arm token, when one was issued.
    pub(crate) token: Option<u64>,
    /// The measurement window, when known.
    pub(crate) window: Option<u64>,
    /// The typed pane.
    pub(crate) pane: u64,
}

/// Whether a record exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    /// A frozen record was transferred.
    Recorded,
    /// No record exists.
    Unavailable,
    /// The record or the protocol was malformed.
    Rejected,
}

/// A credited sample's ordering status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderingStatus {
    /// The full additive order holds.
    Ordered,
    /// The publication fell inside the credited dispatch; context only.
    PublishedDuringDispatch,
    /// A named event or relation is missing.
    SplitOnly,
    /// Malformed ordering.
    ClockOrder,
}

impl OrderingStatus {
    /// The status's name in `result.json`.
    fn as_str(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::PublishedDuringDispatch => "published_during_dispatch",
            Self::SplitOnly => "split_only",
            Self::ClockOrder => "clock-order",
        }
    }
}

/// Whether the appearing section's chunk read is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadStamp {
    /// A `LatestChunkRead` was retained.
    Stamped,
    /// The PTY did not stamp reads.
    NotConfigured,
    /// Reads were stamped but none was retained; the cause is not observable through V1.
    MissingOrUnrepresentable,
}

/// Which site the readiness check ran at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// The Pane-output service, outside every watched redraw interval.
    OutputService,
    /// The streaming admission check, inside one watched redraw interval.
    Admission,
}

/// The credited dispatch's admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// A tick's permit admitted it.
    Permit,
    /// The fallback ceiling admitted it.
    Fallback,
    /// Any other admission.
    Admitted,
}

/// The consumed permit's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermitIdentity {
    /// The consumed tick and its delivery are known.
    Known,
    /// A permit was consumed but its identity is unavailable.
    Unknown,
    /// No permit was consumed.
    None,
}

/// The readiness check: the earliest retained check at or past the appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Readiness {
    /// What it did.
    pub(crate) outcome: CheckOutcome,
    /// Its event-loop sequence.
    pub(crate) loop_seq: u32,
    /// Its site, when the event-loop sequence decides it uniquely.
    pub(crate) site: Option<Site>,
}

/// The ordered sample's integer-nanosecond decomposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parts {
    /// The seven additive parts, named by `PART_NAMES`.
    pub(crate) additive_ns: [u64; 7],
    /// Render exit to the App's dispatch return.
    pub(crate) render_exit_to_dispatch_return_ns: u64,
    /// The App's dispatch return to the credited end.
    pub(crate) dispatch_return_to_credited_end_ns: u64,
}

/// One credited sample's `echo_timeline`, schema 1. A `None` field is null in `result.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimelineSample {
    /// Whether a record exists.
    pub(crate) availability: Availability,
    /// The unavailable or rejection reason.
    pub(crate) reason: Option<&'static str>,
    /// The ordering status.
    pub(crate) ordering: Option<OrderingStatus>,
    /// The ordering status's reason.
    pub(crate) ordering_reason: Option<&'static str>,
    /// The readiness check preceded the publication.
    pub(crate) ready_before_publication: Option<bool>,
    /// An event was dropped.
    pub(crate) overflow: Option<bool>,
    /// The decomposition, only for `ordered`.
    pub(crate) parts: Option<Parts>,
    /// A lower bound on the echo byte's read to its parse.
    pub(crate) latest_read_to_parse_ns: Option<u64>,
    /// The read stamp's state.
    pub(crate) read_stamp: Option<ReadStamp>,
    /// The readiness check.
    pub(crate) readiness: Option<Readiness>,
    /// The credited dispatch's admission.
    pub(crate) admission: Option<Admission>,
    /// Time a permit was held between readiness and entry.
    pub(crate) permit_present_ns: Option<u64>,
    /// Time no permit was held between readiness and entry.
    pub(crate) permit_absent_ns: Option<u64>,
    /// The consumed permit's identity.
    pub(crate) permit_identity: Option<PermitIdentity>,
    /// A known permit with a known delivery was consumed.
    pub(crate) tick_qualified: Option<bool>,
    /// That delivery to the credited entry.
    pub(crate) tick_to_entry_ns: Option<u64>,
    /// Other panes' services between readiness and entry (M3).
    pub(crate) flood_services: Option<u64>,
    /// The flood ring still covered that interval.
    pub(crate) m3_complete: Option<bool>,
    /// The arm token, when issued.
    pub(crate) token: Option<u64>,
    /// The measurement window, when known.
    pub(crate) window: Option<u64>,
    /// The typed pane.
    pub(crate) pane: Option<u64>,
    /// The credited dispatch, when uniquely bound.
    pub(crate) credited_dispatch_seq: Option<u32>,
}

impl TimelineSample {
    /// A sample carrying only availability, a reason and the identities the harness knows.
    fn bare(availability: Availability, reason: Option<&'static str>, credited: &Credited) -> Self {
        Self {
            availability,
            reason,
            ordering: None,
            ordering_reason: None,
            ready_before_publication: None,
            overflow: None,
            parts: None,
            latest_read_to_parse_ns: None,
            read_stamp: None,
            readiness: None,
            admission: None,
            permit_present_ns: None,
            permit_absent_ns: None,
            permit_identity: None,
            tick_qualified: None,
            tick_to_entry_ns: None,
            flood_services: None,
            m3_complete: None,
            token: credited.token,
            window: credited.window,
            pane: Some(credited.pane),
            credited_dispatch_seq: None,
        }
    }

    /// In `ordered`: a recorded sample whose full additive order holds.
    pub(crate) fn ordered(&self) -> bool {
        self.availability == Availability::Recorded
            && self.ordering == Some(OrderingStatus::Ordered)
    }

    /// In B2: ordered, not a fallback, with a valid permit reconstruction.
    pub(crate) fn in_b2(&self) -> bool {
        self.ordered()
            && self.admission != Some(Admission::Fallback)
            && self.permit_present_ns.is_some()
            && self.permit_absent_ns.is_some()
    }

    /// In M4: ordered and tick-qualified.
    pub(crate) fn in_m4(&self) -> bool {
        self.ordered() && self.tick_qualified == Some(true)
    }

    /// In M3: ordered with a complete flood interval.
    pub(crate) fn in_m3(&self) -> bool {
        self.ordered() && self.m3_complete == Some(true)
    }

    /// In the read-stamped population: ordered with a retained chunk read.
    pub(crate) fn read_stamped(&self) -> bool {
        self.ordered() && self.read_stamp == Some(ReadStamp::Stamped)
    }

    /// The sample as its `result.json` object.
    pub(crate) fn to_json(self) -> Value {
        let (unavailable_reason, rejection_reason) = match self.availability {
            Availability::Recorded => (None, None),
            Availability::Unavailable => (self.reason, None),
            Availability::Rejected => (None, self.reason),
        };
        let parts = self.parts.map(|parts| {
            let named: serde_json::Map<String, Value> = PART_NAMES
                .iter()
                .zip(parts.additive_ns)
                .map(|(name, value)| ((*name).to_owned(), json!(value)))
                .collect();
            Value::Object(named)
        });
        json!({
            "schema": TIMELINE_SCHEMA,
            "availability": match self.availability {
                Availability::Recorded => "recorded",
                Availability::Unavailable => "unavailable",
                Availability::Rejected => "rejected",
            },
            "unavailable_reason": unavailable_reason,
            "rejection_reason": rejection_reason,
            "ordering": self.ordering.map(OrderingStatus::as_str),
            "ordering_reason": self.ordering_reason,
            "ready_before_publication": self.ready_before_publication,
            "overflow": self.overflow,
            "parts_ns": parts,
            "render_exit_to_dispatch_return_ns":
                self.parts.map(|parts| parts.render_exit_to_dispatch_return_ns),
            "dispatch_return_to_credited_end_ns":
                self.parts.map(|parts| parts.dispatch_return_to_credited_end_ns),
            "latest_read_to_parse_ns": self.latest_read_to_parse_ns,
            "read_stamp": self.read_stamp.map(|stamp| match stamp {
                ReadStamp::Stamped => "stamped",
                ReadStamp::NotConfigured => "not-configured",
                ReadStamp::MissingOrUnrepresentable => "missing-or-unrepresentable",
            }),
            "readiness": self.readiness.map(|readiness| json!({
                "outcome": readiness.outcome.as_str(),
                "loop_seq": readiness.loop_seq,
                "site": readiness.site.map(|site| match site {
                    Site::OutputService => "output_service",
                    Site::Admission => "admission",
                }),
            })),
            "admission": self.admission.map(|admission| match admission {
                Admission::Permit => "permit",
                Admission::Fallback => "fallback",
                Admission::Admitted => "admitted",
            }),
            "permit_present_ns": self.permit_present_ns,
            "permit_absent_ns": self.permit_absent_ns,
            "permit_identity": self.permit_identity.map(|identity| match identity {
                PermitIdentity::Known => "known",
                PermitIdentity::Unknown => "unknown",
                PermitIdentity::None => "none",
            }),
            "tick_qualified": self.tick_qualified,
            "tick_to_entry_ns": self.tick_to_entry_ns,
            "flood_services": self.flood_services,
            "m3_complete": self.m3_complete,
            "token": self.token,
            "window": self.window,
            "pane": self.pane,
            "credited_dispatch_seq": self.credited_dispatch_seq,
        })
    }
}

/// The interval `from`..`to` in whole nanoseconds; `None` when reversed or unrepresentable.
fn ns_between(from: Instant, to: Instant) -> Option<u64> {
    (to >= from).then(|| to.duration_since(from)).and_then(duration_ns)
}

/// One dispatch's validated boundaries and its single admission decision.
#[derive(Clone, Copy, Debug)]
struct Dispatch {
    entry_loop: u32,
    entry: Instant,
    /// The return's event-loop sequence and instant; every dispatch of a valid record has one.
    returned: Option<(u32, Instant)>,
    /// `RenderEnter`, once recorded.
    enter: Option<Instant>,
    /// `RenderExit`, once recorded.
    exit: Option<Instant>,
    /// The dispatch's one admission decision, once recorded.
    decision: Option<Decision>,
    /// That decision's instant, which the render and the return must not precede.
    admitted_at: Option<Instant>,
}

/// The permit held between transitions, reconstructed in record order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// No permit is held.
    Absent,
    /// A permit is held but its tick identity is unavailable.
    Unknown,
    /// The permit this tick stored, delivered at this instant.
    Known(TickId, Instant),
}

impl Held {
    /// Whether a permit is held.
    fn held(self) -> bool {
        self != Self::Absent
    }

    /// Whether a clear or a consumption naming `tick_seq` agrees with this state: a known permit is
    /// named by its own tick, an unknown one by none, and an absent one by nothing.
    fn named_by(self, tick_seq: Option<u32>) -> bool {
        match (self, tick_seq) {
            (Self::Known(identity, _), Some(tick_seq)) => identity.tick_seq == tick_seq,
            (Self::Unknown, None) => true,
            _ => false,
        }
    }
}

/// A record whose structure was validated: its dispatches, the permit's transitions in record order
/// starting from the state at arm, and the permit each `Permit` admission consumed, by dispatch.
struct Validated {
    dispatches: BTreeMap<u32, Dispatch>,
    transitions: Vec<(Instant, Held)>,
    consumed: BTreeMap<u32, Held>,
}

/// The event-loop sequence of an event that carries one.
fn loop_of(kind: &EventKind) -> Option<u32> {
    match *kind {
        EventKind::Check { loop_seq, .. }
        | EventKind::Tick { loop_seq, .. }
        | EventKind::Admission { loop_seq, .. }
        | EventKind::Cleared { loop_seq, .. }
        | EventKind::Entry { loop_seq, .. }
        | EventKind::Return { loop_seq, .. } => Some(loop_seq),
        EventKind::ChunkRead | EventKind::RenderEnter { .. } | EventKind::RenderExit { .. } => None,
    }
}

/// Whether two consecutive event-loop events may share one `loop_seq`: only one hook's own events, a
/// streaming check before its admission decision or discard, and a decision before its discard.
fn may_share_loop(previous: &EventKind, next: &EventKind) -> bool {
    matches!(
        (previous, next),
        (EventKind::Check { .. }, EventKind::Admission { .. } | EventKind::Cleared { .. })
            | (EventKind::Admission { .. }, EventKind::Cleared { .. })
    )
}

/// Validate the raw record before any population is derived, naming its first violation:
/// - `loop-sequence`: event-loop sequences never decrease in record order, and only one hook's events
///   share one;
/// - `dispatch-pair`: each dispatch has one entry and one return, overlaps no other, and none is left open;
/// - `admission-orphan`, `admission-count`, `admission-order`: one admission per dispatch, inside it,
///   at or after its entry and at or before its render and return;
/// - `render-pair`: one enter and one exit inside an open, admitted dispatch, in time order;
/// - `loop-time-order`: event-loop events and render boundaries never go back in time; equal times are
///   allowed, and `LatestChunkRead`, a historical worker time, is exempt;
/// - `initial-permit`, `tick-identity`, `tick-sequence`: a known permit held at arm was delivered at or
///   before the arm; a known tick identity is nonzero; known accepted ticks strictly increase their
///   `tick_seq` and never decrease their generation, from the permit held at arm on, and none follows
///   an unknown accepted tick, since both numbers only grow and an unknown one is past `u32::MAX`;
/// - `permit-transition`: a clear or consumption names the permit actually held;
/// - `flood-sequence`: services strictly increase, share no sequence with a tick, admission, clear or
///   boundary, and lie past their eviction mark.
///
/// Timestamps are not required to increase globally: `LatestChunkRead` carries a historical worker time.
fn validate(record: &Record, instants: &[Instant]) -> Result<Validated, &'static str> {
    let mut dispatches: BTreeMap<u32, Dispatch> = BTreeMap::new();
    let mut open: Option<u32> = None;
    let mut previous_loop: Option<(u32, &EventKind)> = None;
    let mut boundary_loops = std::collections::BTreeSet::new();
    let mut held = match (record.initial_permit, record.initial_permit_unknown) {
        (Some(permit), _) => Held::Known(permit.identity, permit.delivered_at),
        (None, true) => Held::Unknown,
        (None, false) => Held::Absent,
    };
    if let Some(permit) = record.initial_permit {
        if permit.delivered_at > record.armed_at {
            // When: the permit held at arm was delivered after the arm, it cannot have been held then.
            return Err("initial-permit");
        }
        if permit.identity.tick_seq == 0 || permit.identity.generation == 0 {
            // When: the identity is zero, no accepted tick produced it; both numbers start at one.
            return Err("tick-identity");
        }
    }
    let mut transitions = vec![(record.armed_at, held)];
    let mut consumed = BTreeMap::new();
    // The last event-loop instant, the last known accepted tick, and whether an unknown one was accepted.
    let mut last_time: Option<Instant> = None;
    let mut last_known = record.initial_permit.map(|permit| permit.identity);
    let mut unknown_seen = false;
    for (event, at) in record.events.iter().zip(instants) {
        let at = *at;
        if let Some(loop_seq) = loop_of(&event.kind) {
            let shared = previous_loop.is_some_and(|(last, last_kind)| {
                loop_seq < last || (loop_seq == last && !may_share_loop(last_kind, &event.kind))
            });
            if shared {
                // When: a sequence goes back, or two hooks share one, the loop order is malformed.
                return Err("loop-sequence");
            }
            previous_loop = Some((loop_seq, &event.kind));
            if !matches!(event.kind, EventKind::Check { .. }) {
                boundary_loops.insert(loop_seq);
            }
        }
        // The open dispatch an event names, if it names the open one.
        let current = |dispatch_seq: u32| (open == Some(dispatch_seq)).then_some(dispatch_seq);
        match event.kind {
            EventKind::Entry { loop_seq, dispatch_seq } => {
                if open.is_some() || dispatches.contains_key(&dispatch_seq) {
                    // When: a dispatch is still open or its sequence was used, the boundaries are malformed.
                    return Err("dispatch-pair");
                }
                let dispatch = Dispatch {
                    entry_loop: loop_seq,
                    entry: at,
                    returned: None,
                    enter: None,
                    exit: None,
                    decision: None,
                    admitted_at: None,
                };
                dispatches.insert(dispatch_seq, dispatch);
                open = Some(dispatch_seq);
            }
            EventKind::Return { loop_seq, dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("dispatch-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("dispatch-pair")?;
                if dispatch.enter.is_some() != dispatch.exit.is_some() {
                    // When: the render pair is unpaired at the return, the render boundary is malformed.
                    return Err("render-pair");
                }
                if dispatch.admitted_at.is_some_and(|admitted| at < admitted) {
                    // When: the return precedes its own admission decision, the decision is out of order.
                    return Err("admission-order");
                }
                if at < dispatch.exit.unwrap_or(dispatch.entry) {
                    // When: the return precedes the render or the entry, the dispatch is out of order.
                    return Err("dispatch-pair");
                }
                dispatch.returned = Some((loop_seq, at));
                open = None;
            }
            EventKind::Admission { dispatch_seq, decision, .. } => {
                let seq = current(dispatch_seq).ok_or("admission-orphan")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("admission-orphan")?;
                if dispatch.decision.is_some() {
                    // When: the dispatch already has a decision, a second one is malformed, never a choice.
                    return Err("admission-count");
                }
                if dispatch.enter.is_some() || at < dispatch.entry {
                    // When: the decision follows the render or precedes the entry, it is out of order.
                    return Err("admission-order");
                }
                dispatch.decision = Some(decision);
                dispatch.admitted_at = Some(at);
                if let Decision::Permit { tick_seq } = decision {
                    if !held.named_by(tick_seq) {
                        // When: the consumption names a permit other than the one held, it is impossible.
                        return Err("permit-transition");
                    }
                    consumed.insert(seq, held);
                    held = Held::Absent;
                    transitions.push((at, held));
                }
            }
            EventKind::Cleared { tick_seq, .. } => {
                if !held.named_by(tick_seq) {
                    // When: the clear names a permit other than the one held, or none is held, it is impossible.
                    return Err("permit-transition");
                }
                held = Held::Absent;
                transitions.push((at, held));
            }
            EventKind::Tick { identity, .. } => {
                if let Some(identity) = identity {
                    if identity.tick_seq == 0 || identity.generation == 0 {
                        // When: the identity is zero, no accepted tick produced it; both numbers start at one.
                        return Err("tick-identity");
                    }
                    let follows = last_known.is_none_or(|last| {
                        identity.tick_seq > last.tick_seq && identity.generation >= last.generation
                    });
                    if unknown_seen || !follows {
                        // When: a known tick repeats, goes back, or follows an unknown one, the sequence is impossible.
                        return Err("tick-sequence");
                    }
                    last_known = Some(identity);
                } else {
                    // When: the identity is None, every later accepted tick is past `u32::MAX` too.
                    unknown_seen = true;
                }
                held = identity.map_or(Held::Unknown, |identity| Held::Known(identity, at));
                transitions.push((at, held));
            }
            EventKind::RenderEnter { dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("render-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("render-pair")?;
                let admitted = matches!(
                    dispatch.decision,
                    Some(Decision::Permit { .. } | Decision::Fallback | Decision::Admitted)
                );
                if dispatch.enter.is_some() || !admitted || at < dispatch.entry {
                    // When: a second enter, an unadmitted render or an enter before the entry, it is malformed.
                    return Err("render-pair");
                }
                if dispatch.admitted_at.is_some_and(|admitted| at < admitted) {
                    // When: the render precedes its own admission decision, the decision is out of order.
                    return Err("admission-order");
                }
                dispatch.enter = Some(at);
            }
            EventKind::RenderExit { dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("render-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("render-pair")?;
                if dispatch.exit.is_some() || dispatch.enter.is_none_or(|enter| at < enter) {
                    // When: a second exit, an exit with no enter or one before it, the render is malformed.
                    return Err("render-pair");
                }
                dispatch.exit = Some(at);
            }
            EventKind::Check { .. } | EventKind::ChunkRead => {}
        }
        // The chunk read carries a historical worker time; every other event is in event-loop order.
        let historical = event.kind == EventKind::ChunkRead;
        if !historical && last_time.is_some_and(|last| at < last) {
            // When: an event-loop event or render boundary precedes the previous one, time went back.
            return Err("loop-time-order");
        }
        if !historical {
            last_time = Some(at);
        }
    }
    if open.is_some() {
        // When: a dispatch is still open at the take, its return is missing.
        return Err("dispatch-pair");
    }
    let mut last_service = None;
    for (loop_seq, _) in &record.flood {
        if last_service.is_some_and(|last| *loop_seq <= last) || boundary_loops.contains(loop_seq) {
            // When: services repeat, go back, or share a non-check sequence, the flood ring is malformed.
            return Err("flood-sequence");
        }
        last_service = Some(*loop_seq);
    }
    let evicted_past = record
        .flood_evicted_through
        .zip(record.flood.first())
        .is_some_and(|(evicted, (first, _))| evicted >= *first);
    if evicted_past {
        // When: a retained service is at or before the eviction mark, the ring's loss record is malformed.
        return Err("flood-sequence");
    }
    Ok(Validated { dispatches, transitions, consumed })
}

/// The credited dispatch, bound uniquely to the credited forward.
#[derive(Clone, Copy, Debug)]
struct Bound {
    dispatch_seq: u32,
    entry_loop: u32,
    entry: Instant,
    enter: Instant,
    exit: Instant,
    returned: Instant,
    decision: Decision,
}

/// How the credited forward bound to a dispatch pair.
enum Binding {
    Bound(Bound),
    /// No complete pair, or a pair without a render pair.
    NoPair,
    /// More than one complete pair inside the forward; neither first nor last is chosen.
    Ambiguous,
}

/// Bind the credited forward `started..=ended` to exactly one complete watched dispatch pair, which
/// must hold a render pair.
fn bind(dispatches: &BTreeMap<u32, Dispatch>, started: Instant, ended: Instant) -> Binding {
    let inside = |at: Instant| started <= at && at <= ended;
    let candidates: Vec<(u32, &Dispatch)> = dispatches
        .iter()
        .filter(|(_, dispatch)| {
            inside(dispatch.entry) && dispatch.returned.is_some_and(|(_, at)| inside(at))
        })
        .map(|(dispatch_seq, dispatch)| (*dispatch_seq, dispatch))
        .collect();
    let [(dispatch_seq, dispatch)] = candidates.as_slice() else {
        return if candidates.is_empty() { Binding::NoPair } else { Binding::Ambiguous };
    };
    let (Some((_, returned)), Some(enter), Some(exit), Some(decision)) =
        (dispatch.returned, dispatch.enter, dispatch.exit, dispatch.decision)
    else {
        // When: the one candidate rendered nothing, none of its frames can be credited.
        return Binding::NoPair;
    };
    Binding::Bound(Bound {
        dispatch_seq: *dispatch_seq,
        entry_loop: dispatch.entry_loop,
        entry: dispatch.entry,
        enter,
        exit,
        returned,
        decision,
    })
}

/// The site of a check at `check_loop` among validated, complete intervals: inside exactly one watched
/// redraw interval is the admission check, outside every one the output service.
fn site_of(dispatches: &BTreeMap<u32, Dispatch>, check_loop: u32) -> Option<Site> {
    let mut containing = 0_usize;
    for dispatch in dispatches.values() {
        let (return_loop, _) = dispatch.returned?;
        containing += usize::from(dispatch.entry_loop < check_loop && check_loop < return_loop);
    }
    match containing {
        0 => Some(Site::OutputService),
        1 => Some(Site::Admission),
        _ => None,
    }
}

/// The readiness check found in the record.
#[derive(Clone, Copy, Debug)]
struct FrameRequest {
    at: Instant,
    loop_seq: u32,
    outcome: CheckOutcome,
}

/// The decided ordering, its reason and, for `ordered`, the decomposition.
type Disposition = (OrderingStatus, Option<&'static str>, Option<Parts>);

/// The ordering of a bound sample with a valid split and a readiness check.
fn chain(credited: &Credited, split: &Split, bound: &Bound, frame: FrameRequest) -> Disposition {
    let parsed_at = credited.injected.checked_add(Duration::from_nanos(split.input_to_parse_ns));
    let published_at = parsed_at
        .and_then(|parsed| parsed.checked_add(Duration::from_nanos(split.parse_to_publication_ns)));
    let Some(published_at) = published_at else {
        // When: `published_at` is not representable, no order can be checked.
        return (OrderingStatus::ClockOrder, Some("unrepresentable-instant"), None);
    };
    if bound.entry < published_at && published_at <= bound.returned {
        // When: the publication fell inside the credited dispatch, the sample is context only.
        return (OrderingStatus::PublishedDuringDispatch, None, None);
    }
    if frame.loop_seq > bound.entry_loop {
        // When: readiness came after the credited entry by loop order, it is valid context, not a reversal.
        return (OrderingStatus::SplitOnly, Some("readiness-after-entry"), None);
    }
    if frame.at < published_at {
        // When: readiness preceded the publication, it is X1's named case, never forced into the order.
        return (OrderingStatus::SplitOnly, Some("ready-before-publication"), None);
    }
    let parts = (
        ns_between(published_at, frame.at),
        ns_between(frame.at, bound.entry),
        ns_between(bound.entry, bound.enter),
        ns_between(bound.enter, bound.exit),
        ns_between(bound.exit, credited.ended),
        ns_between(bound.exit, bound.returned),
        ns_between(bound.returned, credited.ended),
        ns_between(credited.injected, credited.ended),
    );
    let (
        Some(publication_to_frame_request),
        Some(frame_request_to_entry),
        Some(entry_to_render),
        Some(render),
        Some(render_exit_to_credited_end),
        Some(exit_to_return),
        Some(return_to_end),
        Some(credited_ns),
    ) = parts
    else {
        return (OrderingStatus::ClockOrder, Some("event-order"), None);
    };
    let additive_ns = [
        split.input_to_parse_ns,
        split.parse_to_publication_ns,
        publication_to_frame_request,
        frame_request_to_entry,
        entry_to_render,
        render,
        render_exit_to_credited_end,
    ];
    let total = additive_ns.iter().try_fold(0_u64, |sum, part| sum.checked_add(*part));
    let tail = exit_to_return.checked_add(return_to_end);
    if total != Some(credited_ns) || tail != Some(render_exit_to_credited_end) {
        // When: the parts do not sum exactly in integer nanoseconds, the sample is malformed.
        return (OrderingStatus::ClockOrder, Some("sum"), None);
    }
    let parts = Parts {
        additive_ns,
        render_exit_to_dispatch_return_ns: exit_to_return,
        dispatch_return_to_credited_end_ns: return_to_end,
    };
    (OrderingStatus::Ordered, None, Some(parts))
}

/// The permit's present and absent time over `from..until`, integrated over the validated transitions.
fn occupancy(transitions: &[(Instant, Held)], from: Instant, until: Instant) -> Option<(u64, u64)> {
    let (mut present, mut absent) = (0_u64, 0_u64);
    for (index, (start, held)) in transitions.iter().enumerate() {
        let end = transitions.get(index + 1).map_or(until, |(next, _)| *next);
        // A segment outside the window reverses once clipped and counts zero.
        let span = ns_between((*start).max(from), end.min(until)).unwrap_or(0);
        let total = if held.held() { &mut present } else { &mut absent };
        *total = total.checked_add(span)?;
    }
    Some((present, absent))
}

/// Reduce one credited sample's timeline to its schema-1 fields.
pub(crate) fn analyze(take: &TimelineTake, credited: &Credited) -> TimelineSample {
    let record = match take {
        TimelineTake::Unavailable(reason) => {
            return TimelineSample::bare(Availability::Unavailable, Some(reason), credited);
        }
        TimelineTake::Rejected(reason) => {
            return TimelineSample::bare(Availability::Rejected, Some(reason), credited);
        }
        TimelineTake::Recorded(record) => record,
    };
    if record.initial_permit.is_some() && record.initial_permit_unknown {
        // When: both initial-permit forms are set, the record is malformed and rejected.
        return TimelineSample::bare(
            Availability::Rejected,
            Some("malformed-initial-permit"),
            credited,
        );
    }
    let mut sample = TimelineSample::bare(Availability::Recorded, None, credited);
    sample.overflow = Some(record.overflow);
    let chunk_read = record.events.iter().find(|event| event.kind == EventKind::ChunkRead);
    sample.read_stamp = Some(match (record.chunk_timestamps, chunk_read) {
        (false, _) => ReadStamp::NotConfigured,
        (true, Some(_)) => ReadStamp::Stamped,
        (true, None) => ReadStamp::MissingOrUnrepresentable,
    });
    let split = credited.split.filter(|_| credited.split_reason == crate::record::SPLIT);
    let parsed_at = split.and_then(|split| {
        credited.injected.checked_add(Duration::from_nanos(split.input_to_parse_ns))
    });
    let read_at =
        chunk_read.and_then(|event| record.armed_at.checked_add(Duration::from_nanos(event.at_ns)));
    // A lower bound; a missing or later read is null, never zero.
    sample.latest_read_to_parse_ns =
        read_at.zip(parsed_at).and_then(|(read, parsed)| ns_between(read, parsed));
    let existing = match credited.split_reason {
        crate::record::SPLIT => None,
        "clock-order" => Some((OrderingStatus::ClockOrder, "clock-order")),
        reason => Some((OrderingStatus::SplitOnly, reason)),
    };
    let decide = |sample: &mut TimelineSample, (status, reason): (OrderingStatus, &'static str)| {
        sample.ordering = Some(status);
        sample.ordering_reason = Some(reason);
    };
    if record.overflow {
        // When: an event was dropped, every event-dependent field stays null.
        decide(&mut sample, existing.unwrap_or((OrderingStatus::SplitOnly, "overflow")));
        return sample;
    }
    let instants: Option<Vec<Instant>> = record
        .events
        .iter()
        .map(|event| record.armed_at.checked_add(Duration::from_nanos(event.at_ns)))
        .collect();
    let Some(instants) = instants else {
        // When: an event's instant is not representable, the sample's order cannot be read.
        decide(&mut sample, (OrderingStatus::ClockOrder, "unrepresentable-instant"));
        return sample;
    };
    // The raw record is validated before any population is derived, so no named ordering case can
    // mask a malformed sequence.
    let validated = match validate(record, &instants) {
        Ok(validated) => validated,
        Err(reason) => {
            // When: the recorded execution is malformed, it stays recorded as `clock-order` with its reason,
            // derives nothing, and leaves every event-dependent population.
            decide(&mut sample, (OrderingStatus::ClockOrder, reason));
            return sample;
        }
    };
    let binding = bind(&validated.dispatches, credited.forward_started, credited.ended);
    let bound = match binding {
        Binding::Bound(bound) => Some(bound),
        Binding::NoPair | Binding::Ambiguous => None,
    };
    let decision = bound.map(|bound| bound.decision);
    sample.credited_dispatch_seq = bound.map(|bound| bound.dispatch_seq);
    sample.admission = decision.and_then(|decision| match decision {
        Decision::Permit { .. } => Some(Admission::Permit),
        Decision::Fallback => Some(Admission::Fallback),
        Decision::Admitted => Some(Admission::Admitted),
        Decision::Deferred | Decision::Held => None,
    });
    if let Some(bound) = bound {
        // The consumed permit is the one validation found held at the credited admission.
        let consumed = validated.consumed.get(&bound.dispatch_seq).copied();
        sample.permit_identity = Some(match consumed {
            None => PermitIdentity::None,
            Some(Held::Known(..)) => PermitIdentity::Known,
            // Validation admits only a held permit's consumption, so this is the unknown one.
            Some(Held::Unknown | Held::Absent) => PermitIdentity::Unknown,
        });
        let delivery = match consumed {
            Some(Held::Known(_, delivered)) => Some(delivered),
            _ => None,
        };
        sample.tick_to_entry_ns = delivery.and_then(|delivered| ns_between(delivered, bound.entry));
        sample.tick_qualified = Some(sample.tick_to_entry_ns.is_some());
    }
    let frame = split.and_then(|split| {
        record.events.iter().zip(&instants).find_map(|(event, at)| match event.kind {
            EventKind::Check { loop_seq, generation, outcome }
                if generation >= split.echo_generation =>
            {
                Some(FrameRequest { at: *at, loop_seq, outcome })
            }
            _ => None,
        })
    });
    sample.readiness = frame.map(|frame| Readiness {
        outcome: frame.outcome,
        loop_seq: frame.loop_seq,
        site: site_of(&validated.dispatches, frame.loop_seq),
    });
    let published_at = split.zip(parsed_at).and_then(|(split, parsed)| {
        parsed.checked_add(Duration::from_nanos(split.parse_to_publication_ns))
    });
    sample.ready_before_publication =
        frame.zip(published_at).map(|(frame, published)| frame.at < published);
    let (status, reason, parts) = match (existing, binding, split, frame) {
        (Some((status, reason)), ..) => (status, Some(reason), None),
        (None, Binding::NoPair, ..) => (OrderingStatus::SplitOnly, Some("no-credited-pair"), None),
        (None, Binding::Ambiguous, ..) => {
            (OrderingStatus::SplitOnly, Some("ambiguous-credited-pair"), None)
        }
        (None, Binding::Bound(_), _, None) | (None, Binding::Bound(_), None, _) => {
            (OrderingStatus::SplitOnly, Some("missing-event:output-check"), None)
        }
        (None, Binding::Bound(bound), Some(split), Some(frame)) => {
            chain(credited, &split, &bound, frame)
        }
    };
    sample.ordering = Some(status);
    sample.ordering_reason = reason;
    sample.parts = parts;
    // Permit occupancy and the flood count cover readiness to entry; readiness after entry supplies neither.
    if let (Some(bound), Some(frame), Some(decision)) = (bound, frame, decision) {
        if frame.loop_seq <= bound.entry_loop && frame.at <= bound.entry {
            if decision != Decision::Fallback {
                let occupied = occupancy(&validated.transitions, frame.at, bound.entry);
                sample.permit_present_ns = occupied.map(|(present, _)| present);
                sample.permit_absent_ns = occupied.map(|(_, absent)| absent);
            }
            let first = frame.loop_seq.checked_add(1);
            let in_interval = |loop_seq: u32| {
                first.is_some_and(|first| first <= loop_seq && loop_seq <= bound.entry_loop)
            };
            sample.m3_complete = Some(first.is_none_or(|first| {
                record.flood_evicted_through.is_none_or(|evicted| evicted < first)
            }));
            let served = record
                .flood
                .iter()
                .filter(|(loop_seq, pane)| *pane != credited.pane && in_interval(*loop_seq))
                .count();
            sample.flood_services = u64::try_from(served).ok();
        }
    }
    sample
}

/// The timeline of a credited sample whose watch was not armed: none out of scope, else unavailable
/// with the arm's reason. An armed sample's timeline comes from its take.
pub(crate) fn unarmed_timeline(arm: ArmState) -> Option<TimelineTake> {
    match arm {
        ArmState::OutOfScope | ArmState::Armed(_) => None,
        ArmState::Unsupported => Some(TimelineTake::Unavailable("cfg-off")),
        ArmState::Failed(reason) => {
            Some(TimelineTake::Unavailable(reason.strip_prefix("arm-").unwrap_or(reason)))
        }
    }
}

/// The `echo_timeline_coverage` object over credited samples' timelines, each population with its named
/// numerator and denominator: B2 and M3 over ordered and, again, over credited. Null when no credited
/// sample was in scope; `unavailable`, with null populations, when none was recorded, never a measured zero.
pub(crate) fn coverage<'sample>(
    timelines: impl IntoIterator<Item = &'sample TimelineSample>,
) -> Value {
    let samples: Vec<&TimelineSample> = timelines.into_iter().collect();
    let credited = samples.len();
    if credited == 0 {
        // When: no credited sample was in the timeline's scope, there is no coverage to report.
        return Value::Null;
    }
    let count = |member: fn(&TimelineSample) -> bool| {
        samples.iter().filter(|sample| member(sample)).count()
    };
    let recorded = count(|sample| sample.availability == Availability::Recorded);
    let population = |numerator: usize, denominator: usize| json!({"numerator": numerator, "denominator": denominator});
    if recorded == 0 {
        // When: nothing was recorded, the side is unavailable rather than a population of zero.
        return json!({
            "schema": TIMELINE_SCHEMA,
            "availability": "unavailable",
            "credited": credited,
            "recorded": population(0, credited),
            "ordered": Value::Null,
            "b2": Value::Null,
            "m4": Value::Null,
            "m3": Value::Null,
            "read_stamped": Value::Null,
            "b2_of_credited": Value::Null,
            "m3_of_credited": Value::Null,
        });
    }
    let ordered = count(TimelineSample::ordered);
    json!({
        "schema": TIMELINE_SCHEMA,
        "availability": "available",
        "credited": credited,
        "recorded": population(recorded, credited),
        "ordered": population(ordered, credited),
        "b2": population(count(TimelineSample::in_b2), ordered),
        "m4": population(count(TimelineSample::in_m4), ordered),
        "m3": population(count(TimelineSample::in_m3), ordered),
        "read_stamped": population(count(TimelineSample::read_stamped), ordered),
        "b2_of_credited": population(count(TimelineSample::in_b2), credited),
        "m3_of_credited": population(count(TimelineSample::in_m3), credited),
    })
}

/// The timeline adapter, in a build with the echo trace: every name of the arm token sits here.
#[cfg(feature = "perf-echo-trace")]
pub(crate) mod adapter {
    use std::time::Instant;

    use sonicterm_app::app::{App, ArmToken};

    use super::TimelineTake;

    /// The App's V1 timeline accessor, in a build that names it.
    #[cfg(all(feature = "perf-echo-trace", perf_echo_timeline_api))]
    mod timeline_api {
        use std::time::Instant;

        use sonicterm_app::app::{
            AdmissionDecisionV1, App, ArmToken, EchoTimelineKindV1, EchoTimelineTakeV1,
            EchoTimelineV1, OutputCheckOutcomeV1,
        };

        use super::super::{
            CheckOutcome, Decision, Event, EventKind, HeldAtArm, Record, TickId, TimelineTake,
        };

        /// Transfer `pane`'s timeline for `token`; `armed_at` is the taken echo record's arm instant.
        pub(super) fn transfer(
            app: &mut App,
            pane: u64,
            token: ArmToken,
            armed_at: Option<Instant>,
        ) -> TimelineTake {
            match (app.take_echo_timeline_v1(pane, token), armed_at) {
                (EchoTimelineTakeV1::Timeline(timeline), Some(armed_at)) => {
                    TimelineTake::Recorded(record_of(timeline, armed_at))
                }
                // The watch take failed, so no arm instant exists to place the events.
                (EchoTimelineTakeV1::Timeline(_), None) => TimelineTake::Rejected("no-arm-instant"),
                (EchoTimelineTakeV1::NotRecorded, _) => TimelineTake::Unavailable("not-recorded"),
                (EchoTimelineTakeV1::GateOff, _) => TimelineTake::Unavailable("gate-off"),
                (EchoTimelineTakeV1::NoPane, _) => TimelineTake::Unavailable("no-pane"),
                (EchoTimelineTakeV1::Mismatch, _) => TimelineTake::Rejected("mismatch"),
                (EchoTimelineTakeV1::NotTaken, _) => TimelineTake::Rejected("not-taken"),
                (EchoTimelineTakeV1::AlreadyTaken, _) => TimelineTake::Rejected("already-taken"),
            }
        }

        /// The V1 record in the harness's types.
        fn record_of(timeline: EchoTimelineV1, armed_at: Instant) -> Record {
            let tick = |identity: sonicterm_app::app::TickIdentityV1| TickId {
                tick_seq: identity.tick_seq,
                generation: identity.generation,
            };
            let events = timeline
                .events
                .iter()
                .map(|event| Event {
                    at_ns: event.at_ns,
                    kind: match event.kind {
                        EchoTimelineKindV1::LatestChunkRead => EventKind::ChunkRead,
                        EchoTimelineKindV1::OutputCheck { loop_seq, generation, outcome } => {
                            EventKind::Check {
                                loop_seq,
                                generation,
                                outcome: match outcome {
                                    OutputCheckOutcomeV1::NativeRequest => {
                                        CheckOutcome::NativeRequest
                                    }
                                    OutputCheckOutcomeV1::MarkedInFlight => {
                                        CheckOutcome::MarkedInFlight
                                    }
                                    OutputCheckOutcomeV1::None => CheckOutcome::None,
                                },
                            }
                        }
                        EchoTimelineKindV1::Tick { loop_seq, identity } => {
                            EventKind::Tick { loop_seq, identity: identity.map(tick) }
                        }
                        EchoTimelineKindV1::Admission { loop_seq, dispatch_seq, decision } => {
                            EventKind::Admission {
                                loop_seq,
                                dispatch_seq,
                                decision: match decision {
                                    AdmissionDecisionV1::Permit { tick_seq } => {
                                        Decision::Permit { tick_seq }
                                    }
                                    AdmissionDecisionV1::Fallback => Decision::Fallback,
                                    AdmissionDecisionV1::Admitted => Decision::Admitted,
                                    AdmissionDecisionV1::Deferred(_) => Decision::Deferred,
                                    AdmissionDecisionV1::Held => Decision::Held,
                                },
                            }
                        }
                        EchoTimelineKindV1::PermitCleared { loop_seq, tick_seq, .. } => {
                            EventKind::Cleared { loop_seq, tick_seq }
                        }
                        EchoTimelineKindV1::DispatchEntry { loop_seq, dispatch_seq } => {
                            EventKind::Entry { loop_seq, dispatch_seq }
                        }
                        EchoTimelineKindV1::DispatchReturn { loop_seq, dispatch_seq } => {
                            EventKind::Return { loop_seq, dispatch_seq }
                        }
                        EchoTimelineKindV1::RenderEnter { dispatch_seq } => {
                            EventKind::RenderEnter { dispatch_seq }
                        }
                        EchoTimelineKindV1::RenderExit { dispatch_seq } => {
                            EventKind::RenderExit { dispatch_seq }
                        }
                    },
                })
                .collect();
            Record {
                armed_at,
                chunk_timestamps: timeline.chunk_timestamps,
                initial_permit: timeline.initial_permit.map(|held| HeldAtArm {
                    identity: tick(held.identity),
                    delivered_at: held.delivered_at,
                }),
                initial_permit_unknown: timeline.initial_permit_unknown,
                events,
                overflow: timeline.overflow,
                flood: timeline
                    .flood_services
                    .iter()
                    .map(|service| (service.loop_seq, service.pane_id))
                    .collect(),
                flood_evicted_through: timeline.flood_evicted_through,
            }
        }
    }

    /// Transfer `pane`'s timeline for `token` through the V1 accessor.
    #[cfg(all(feature = "perf-echo-trace", perf_echo_timeline_api))]
    pub(crate) fn drain(
        app: &mut App,
        pane: u64,
        token: ArmToken,
        armed_at: Option<Instant>,
    ) -> TimelineTake {
        timeline_api::transfer(app, pane, token, armed_at)
    }

    /// Without `perf_echo_timeline_api` the accessor is not named, so every armed sample's timeline
    /// is unavailable with reason `cfg-off`, and nothing is transferred.
    #[cfg(not(perf_echo_timeline_api))]
    pub(crate) fn drain(
        _app: &mut App,
        _pane: u64,
        _token: ArmToken,
        _armed_at: Option<Instant>,
    ) -> TimelineTake {
        TimelineTake::Unavailable("cfg-off")
    }
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod timeline_tests;

/// The ordered fixture, for the probe's production-adapter test.
#[cfg(test)]
#[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_imports))]
pub(crate) use timeline_tests::fixture_at;
