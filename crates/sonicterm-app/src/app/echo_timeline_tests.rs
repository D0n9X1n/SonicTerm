use super::*;
use crate::app::{App, ArmToken};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// The prerequisite records no timeline: any pane and token read `NotRecorded`, every time, so a
/// harness reports the timeline unavailable, never complete.
#[test]
fn the_prerequisite_records_no_timeline() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    for (pane_id, raw) in [(1, 1), (1, 1), (42, 9)] {
        assert_eq!(
            app.take_echo_timeline_v1(pane_id, ArmToken::for_test(raw)),
            EchoTimelineTakeV1::NotRecorded
        );
    }
}

/// The frozen record schema: every field and variant the contract names, built from public fields
/// and matched exhaustively, so removing or reshaping one fails this test's build. A harness builds
/// its own fixtures the same way.
#[test]
fn the_timeline_schema_is_the_contracts() {
    let identity = TickIdentityV1 { tick_seq: 4, generation: 2 };
    let kinds = [
        EchoTimelineKindV1::LatestChunkRead,
        EchoTimelineKindV1::OutputCheck {
            loop_seq: 1,
            generation: u64::MAX,
            outcome: OutputCheckOutcomeV1::NativeRequest,
        },
        EchoTimelineKindV1::Tick { loop_seq: 2, identity: Some(identity) },
        EchoTimelineKindV1::Admission {
            loop_seq: 3,
            dispatch_seq: 1,
            decision: AdmissionDecisionV1::Permit { tick_seq: None },
        },
        EchoTimelineKindV1::PermitCleared {
            loop_seq: 4,
            tick_seq: Some(4),
            cause: PermitClearCauseV1::AdmissionDiscard,
        },
        EchoTimelineKindV1::DispatchEntry { loop_seq: 5, dispatch_seq: 1 },
        EchoTimelineKindV1::DispatchReturn { loop_seq: 6, dispatch_seq: 1 },
        EchoTimelineKindV1::RenderEnter { dispatch_seq: 1 },
        EchoTimelineKindV1::RenderExit { dispatch_seq: 1 },
    ];
    let record = EchoTimelineV1 {
        chunk_timestamps: true,
        initial_permit: Some(PermitSnapshotV1 {
            identity,
            delivered_at: std::time::Instant::now(),
        }),
        initial_permit_unknown: false,
        events: kinds.iter().map(|kind| EchoTimelineEventV1 { at_ns: 0, kind: *kind }).collect(),
        overflow: false,
        flood_services: vec![FloodServiceV1 { loop_seq: 3, pane_id: 9 }],
        flood_evicted_through: None,
    };
    let names: Vec<&str> = record
        .events
        .iter()
        .map(|event| match event.kind {
            EchoTimelineKindV1::LatestChunkRead => "chunk",
            EchoTimelineKindV1::OutputCheck { .. } => "check",
            EchoTimelineKindV1::Tick { .. } => "tick",
            EchoTimelineKindV1::Admission { .. } => "admission",
            EchoTimelineKindV1::PermitCleared { .. } => "cleared",
            EchoTimelineKindV1::DispatchEntry { .. } => "entry",
            EchoTimelineKindV1::DispatchReturn { .. } => "return",
            EchoTimelineKindV1::RenderEnter { .. } => "enter",
            EchoTimelineKindV1::RenderExit { .. } => "exit",
        })
        .collect();
    assert_eq!(names.len(), 9);
    let decisions = [
        AdmissionDecisionV1::Permit { tick_seq: Some(1) },
        AdmissionDecisionV1::Fallback,
        AdmissionDecisionV1::Admitted,
        AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Contention),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Sync),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming),
        AdmissionDecisionV1::Held,
    ];
    assert_eq!(decisions.len(), 8);
    let outcomes = [
        OutputCheckOutcomeV1::NativeRequest,
        OutputCheckOutcomeV1::MarkedInFlight,
        OutputCheckOutcomeV1::None,
    ];
    let causes = [
        PermitClearCauseV1::AdmissionDiscard,
        PermitClearCauseV1::LinkReset,
        PermitClearCauseV1::LinkPaused,
    ];
    assert_eq!((outcomes.len(), causes.len()), (3, 3));
    let takes = [
        EchoTimelineTakeV1::Timeline(record),
        EchoTimelineTakeV1::NotRecorded,
        EchoTimelineTakeV1::GateOff,
        EchoTimelineTakeV1::NoPane,
        EchoTimelineTakeV1::Mismatch,
        EchoTimelineTakeV1::NotTaken,
        EchoTimelineTakeV1::AlreadyTaken,
    ];
    assert_eq!(takes.len(), 7);
}
