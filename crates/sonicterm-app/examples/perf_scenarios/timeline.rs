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

/// A record whose structure was validated: its dispatches, the permit's transitions in record order
/// starting from the state at arm, and the permit each `Permit` admission consumed, by dispatch.
struct Validated {
    dispatches: BTreeMap<u32, Dispatch>,
    transitions: Vec<(Instant, Held)>,
    consumed: BTreeMap<u32, Held>,
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

/// The readiness check found in the record.
#[derive(Clone, Copy, Debug)]
struct FrameRequest {
    at: Instant,
    loop_seq: u32,
    outcome: CheckOutcome,
}

/// The decided ordering, its reason and, for `ordered`, the decomposition.
type Disposition = (OrderingStatus, Option<&'static str>, Option<Parts>);

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

#[path = "timeline_validate.rs"]
mod validate;
use validate::{bind, chain, occupancy, site_of, validate};
#[path = "timeline_analyze.rs"]
mod analyze;
pub(crate) use analyze::{analyze, coverage, unarmed_timeline};
