//! Packed echo timeline event encoding and decoding.

use super::*;

/// `value`'s low and high halves, for two payload slots; `join_u64` restores it exactly.
pub(super) fn split_u64(value: u64) -> (u32, u32) {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (low, high)
}

/// The `u64` that `split_u64` split into `low` and `high`.
pub(super) fn join_u64(low: u32, high: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

impl TraceEvent {
    /// The internal form of `kind` at `at_ns`; `decode` returns exactly `kind`.
    pub(in crate::app) fn encode(at_ns: u64, kind: EchoTimelineKindV1) -> Self {
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
    pub(in crate::app) fn decode(self) -> EchoTimelineEventV1 {
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

#[cfg(test)]
#[path = "echo_timeline_wire_tests.rs"]
mod echo_timeline_wire_tests;
