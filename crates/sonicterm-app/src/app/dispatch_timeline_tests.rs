use super::*;
use crate::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// An App with its frame-counter gate on, as a counters run builds it before any window exists.
fn gated_app() -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window or pane exists yet");
    app
}

/// The prerequisite records nothing: with the frame-counter gate off, arm and take both answer
/// `GateOff`, whatever the pane, witness or token.
#[test]
fn with_the_gate_off_arm_and_take_answer_gate_off() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    for (pane_id, witness) in [(1, None), (42, Some(target())), (u64::MAX, None)] {
        assert_eq!(app.arm_dispatch_timeline_v1(pane_id, witness), DispatchTimelineArmV1::GateOff);
    }
    for raw in [1, 7, u64::MAX] {
        assert_eq!(
            app.take_dispatch_timeline_v1(DispatchTimelineToken(raw)),
            DispatchTimelineTakeV1::GateOff
        );
    }
}

/// With the gate on, the prerequisite still records nothing: arm and take both answer `NotRecorded`,
/// repeatedly, so a harness reports the timeline unavailable and never treats the stub as a sample.
#[test]
fn with_the_gate_on_arm_and_take_answer_not_recorded() {
    let mut app = gated_app();
    for (pane_id, witness) in [(1, None), (1, Some(target())), (42, None)] {
        assert_eq!(
            app.arm_dispatch_timeline_v1(pane_id, witness),
            DispatchTimelineArmV1::NotRecorded
        );
    }
    for raw in [1, 1, u64::MAX] {
        assert_eq!(
            app.take_dispatch_timeline_v1(DispatchTimelineToken(raw)),
            DispatchTimelineTakeV1::NotRecorded
        );
    }
}

/// A witness target built from public fields, as a harness builds one.
fn target() -> AppearanceTargetV1 {
    AppearanceTargetV1 {
        abs_row: 120,
        col: 3,
        character: 'x',
        scrollback_evicted: 9,
        screen_epoch: 2,
        size_generation: 5,
    }
}

/// The derives the contract freezes; removing one fails this test's build.
fn frozen_value_traits<T: Clone + Copy + std::fmt::Debug + PartialEq + Eq>() {}
/// The derives the contract freezes on every id; removing one fails this test's build.
fn frozen_id_traits<T: Copy + std::hash::Hash + Ord>() {}

/// Every type's frozen derives: value types are `Copy` and comparable, ids also hash and order, and the
/// token hashes. The take and timeline types own vectors, so they are `Clone` but never `Copy`.
#[test]
fn every_type_keeps_its_frozen_derives() {
    frozen_value_traits::<DispatchTimelineToken>();
    frozen_value_traits::<DispatchTimelineArmV1>();
    frozen_value_traits::<CallbackKindV1>();
    frozen_value_traits::<PublicationDecisionV1>();
    frozen_value_traits::<DispatchEventKindV1>();
    frozen_value_traits::<AttemptIncompleteReasonV1>();
    frozen_value_traits::<NotRenderedReasonV1>();
    frozen_value_traits::<UnscopedKindV1>();
    frozen_value_traits::<WitnessResolutionV1>();
    frozen_value_traits::<DispatchEventV1>();
    frozen_value_traits::<PhaseV1>();
    frozen_value_traits::<AttemptPhasesV1>();
    frozen_value_traits::<SkipReasonV1>();
    frozen_value_traits::<AttemptOutcomeV1>();
    frozen_value_traits::<AttemptRecordV1>();
    frozen_value_traits::<TransitionTargetV1>();
    frozen_value_traits::<PhaseTransitionV1>();
    frozen_value_traits::<AppearanceTargetV1>();
    frozen_value_traits::<AppearanceRecordV1>();
    frozen_value_traits::<AppearanceLossV1>();
    frozen_id_traits::<DispatchId>();
    frozen_id_traits::<AttemptId>();
    frozen_id_traits::<PublicationSeq>();
    let _: fn(&DispatchTimelineToken, &mut std::collections::hash_map::DefaultHasher) =
        std::hash::Hash::hash;
    assert_eq!(WitnessResolutionV1::default().unresolved_at_take, None);
    assert_eq!(AttemptPhasesV1::default().ns, [0; 11]);
}

/// `AttemptPhasesV1::ns` is indexed by `PhaseV1`: the eleven phases are the indices 0 through 10 in the
/// contract's order, so a reordered or inserted phase misfiles every total.
#[test]
fn every_phase_indexes_its_own_slot_in_contract_order() {
    let phases = [
        PhaseV1::PreAssembly,
        PhaseV1::AssemblyOther,
        PhaseV1::Ink,
        PhaseV1::AtlasUpload,
        PhaseV1::PresentPrep,
        PhaseV1::Compose,
        PhaseV1::Blit,
        PhaseV1::GpuSubmit,
        PhaseV1::PresentOther,
        PhaseV1::Settle,
        PhaseV1::Other,
    ];
    // No wildcard arm: a new phase fails the build; the names pin the order.
    let names: Vec<&str> = phases
        .iter()
        .map(|phase| match phase {
            PhaseV1::PreAssembly => "pre-assembly",
            PhaseV1::AssemblyOther => "assembly-other",
            PhaseV1::Ink => "ink",
            PhaseV1::AtlasUpload => "atlas-upload",
            PhaseV1::PresentPrep => "present-prep",
            PhaseV1::Compose => "compose",
            PhaseV1::Blit => "blit",
            PhaseV1::GpuSubmit => "gpu-submit",
            PhaseV1::PresentOther => "present-other",
            PhaseV1::Settle => "settle",
            PhaseV1::Other => "other",
        })
        .collect();
    assert_eq!(
        names,
        [
            "pre-assembly",
            "assembly-other",
            "ink",
            "atlas-upload",
            "present-prep",
            "compose",
            "blit",
            "gpu-submit",
            "present-other",
            "settle",
            "other",
        ]
    );
    let indices: Vec<usize> = phases.iter().map(|phase| *phase as usize).collect();
    assert_eq!(indices, (0..AttemptPhasesV1::default().ns.len()).collect::<Vec<_>>());
}

/// The witness resolution is 24 bytes on the supported 64-bit targets, as the contract's memory budget
/// counts it (an `Option<u64>` has no niche, plus a `u32`, padded).
#[cfg(target_pointer_width = "64")]
#[test]
fn the_witness_resolution_is_twenty_four_bytes() {
    assert_eq!(std::mem::size_of::<WitnessResolutionV1>(), 24);
}

/// The frozen arm and take schemas: every variant built and matched with no wildcard arm, so adding,
/// removing or reshaping one fails this test's build, and the names pin the contract's order.
#[test]
fn the_arm_and_take_schemas_are_the_contracts() {
    let token = DispatchTimelineToken(3);
    let arms = [
        DispatchTimelineArmV1::Armed(token),
        DispatchTimelineArmV1::NotRecorded,
        DispatchTimelineArmV1::GateOff,
        DispatchTimelineArmV1::NoPane,
        DispatchTimelineArmV1::StillArmed(token),
        DispatchTimelineArmV1::Exhausted,
    ];
    let arm_names: Vec<&str> = arms
        .iter()
        .map(|arm| match arm {
            DispatchTimelineArmV1::Armed(_) => "armed",
            DispatchTimelineArmV1::NotRecorded => "not-recorded",
            DispatchTimelineArmV1::GateOff => "gate-off",
            DispatchTimelineArmV1::NoPane => "no-pane",
            DispatchTimelineArmV1::StillArmed(_) => "still-armed",
            DispatchTimelineArmV1::Exhausted => "exhausted",
        })
        .collect();
    assert_eq!(
        arm_names,
        ["armed", "not-recorded", "gate-off", "no-pane", "still-armed", "exhausted"]
    );
    let takes = [
        DispatchTimelineTakeV1::Timeline(timeline()),
        DispatchTimelineTakeV1::NotRecorded,
        DispatchTimelineTakeV1::GateOff,
        DispatchTimelineTakeV1::Mismatch,
        DispatchTimelineTakeV1::AlreadyTaken,
    ];
    let take_names: Vec<&str> = takes
        .iter()
        .map(|take| match take {
            DispatchTimelineTakeV1::Timeline(_) => "timeline",
            DispatchTimelineTakeV1::NotRecorded => "not-recorded",
            DispatchTimelineTakeV1::GateOff => "gate-off",
            DispatchTimelineTakeV1::Mismatch => "mismatch",
            DispatchTimelineTakeV1::AlreadyTaken => "already-taken",
        })
        .collect();
    assert_eq!(take_names, ["timeline", "not-recorded", "gate-off", "mismatch", "already-taken"]);
}

/// A timeline built from public fields, holding one of every event kind, attempt and transition target.
fn timeline() -> DispatchTimelineV1 {
    let (dispatch, attempt, publication) = (DispatchId(1), AttemptId(2), PublicationSeq(3));
    let events = event_kinds(dispatch, attempt, publication)
        .into_iter()
        .enumerate()
        .map(|(index, kind)| DispatchEventV1 { at_ns: index as u64, kind })
        .collect();
    DispatchTimelineV1 {
        armed_at: std::time::Instant::now(),
        pane_id: 7,
        window: 11,
        events,
        attempts: vec![AttemptRecordV1 {
            attempt,
            parent: dispatch,
            attempt_ns: 55,
            phases: AttemptPhasesV1 { ns: [5; 11] },
            outcome: AttemptOutcomeV1::Presented,
            assembled: true,
            partial_fallbacks: 1,
            transitions: Some((0, 2)),
        }],
        transitions: vec![
            PhaseTransitionV1 {
                attempt,
                at_ns: 4,
                opened: TransitionTargetV1::Phase(PhaseV1::Other),
            },
            PhaseTransitionV1 { attempt, at_ns: 59, opened: TransitionTargetV1::Closed },
        ],
        witness: Some(AppearanceRecordV1::NotSeen),
        witness_resolution: WitnessResolutionV1 { unresolved_at_take: Some(4), abandoned: 1 },
        overflow: false,
    }
}

/// One of every event kind, with every nested enum's variants spread across them.
fn event_kinds(
    dispatch: DispatchId,
    attempt: AttemptId,
    publication: PublicationSeq,
) -> Vec<DispatchEventKindV1> {
    let mut kinds = vec![
        DispatchEventKindV1::PublicationObserved { publication, covered_generation: 9 },
        DispatchEventKindV1::OutputCheck {
            dispatch,
            generation: 9,
            outcome: OutputCheckOutcomeV1::NativeRequest,
        },
        DispatchEventKindV1::DispatchReturn { dispatch },
        DispatchEventKindV1::RenderCall { attempt, parent: dispatch },
        DispatchEventKindV1::RenderEnter { attempt, parent: dispatch },
        DispatchEventKindV1::RenderExit { attempt },
        DispatchEventKindV1::RenderReturned { attempt },
        DispatchEventKindV1::ScopeLost { closed: true },
        DispatchEventKindV1::Admitted { dispatch },
    ];
    for decision in [
        PublicationDecisionV1::Sent,
        PublicationDecisionV1::Suppressed,
        PublicationDecisionV1::Refused,
        PublicationDecisionV1::Untargeted,
    ] {
        let window = (decision != PublicationDecisionV1::Untargeted).then_some(11);
        kinds.push(DispatchEventKindV1::PublicationDecided { publication, decision, window });
    }
    for callback in [
        CallbackKindV1::UserEvent,
        CallbackKindV1::WindowEvent { redraw: true },
        CallbackKindV1::NewEvents,
        CallbackKindV1::AboutToWait,
        CallbackKindV1::Resumed,
    ] {
        kinds.push(DispatchEventKindV1::DispatchEntry { dispatch, callback, window: Some(11) });
    }
    for reason in [
        AttemptIncompleteReasonV1::RootNeverOpened,
        AttemptIncompleteReasonV1::RootNeverClosed,
        AttemptIncompleteReasonV1::NestedSuppressed,
        AttemptIncompleteReasonV1::NestedInvalidated,
    ] {
        kinds.push(DispatchEventKindV1::AttemptIncomplete { attempt, parent: dispatch, reason });
    }
    for reason in [
        NotRenderedReasonV1::NoLayout,
        NotRenderedReasonV1::StructuralInvalid,
        NotRenderedReasonV1::Contended,
        NotRenderedReasonV1::SyncHeld,
        NotRenderedReasonV1::NoRenderer,
        NotRenderedReasonV1::WindowGone,
        NotRenderedReasonV1::AdapterUnwound,
    ] {
        kinds.push(DispatchEventKindV1::AdmittedNotRendered { dispatch, reason });
    }
    for what in [UnscopedKindV1::OutputCheck, UnscopedKindV1::RenderEnter] {
        kinds.push(DispatchEventKindV1::Unscoped { what });
    }
    kinds
}

/// The frozen event schema: every event kind and every nested reason, decision and callback, matched with
/// no wildcard arm, so adding, removing or reshaping a variant of any of them fails this test's build.
#[test]
fn the_event_schema_is_the_contracts() {
    let names: Vec<String> = event_kinds(DispatchId(1), AttemptId(2), PublicationSeq(3))
        .iter()
        .map(|kind| match kind {
            DispatchEventKindV1::PublicationObserved { .. } => "observed".to_owned(),
            DispatchEventKindV1::PublicationDecided { decision, .. } => format!(
                "decided-{}",
                match decision {
                    PublicationDecisionV1::Sent => "sent",
                    PublicationDecisionV1::Suppressed => "suppressed",
                    PublicationDecisionV1::Refused => "refused",
                    PublicationDecisionV1::Untargeted => "untargeted",
                }
            ),
            DispatchEventKindV1::OutputCheck { .. } => "check".to_owned(),
            DispatchEventKindV1::DispatchEntry { callback, .. } => format!(
                "entry-{}",
                match callback {
                    CallbackKindV1::UserEvent => "user",
                    CallbackKindV1::WindowEvent { redraw: true } => "redraw",
                    CallbackKindV1::WindowEvent { redraw: false } => "window",
                    CallbackKindV1::NewEvents => "new-events",
                    CallbackKindV1::AboutToWait => "about-to-wait",
                    CallbackKindV1::Resumed => "resumed",
                }
            ),
            DispatchEventKindV1::DispatchReturn { .. } => "return".to_owned(),
            DispatchEventKindV1::RenderCall { .. } => "call".to_owned(),
            DispatchEventKindV1::RenderEnter { .. } => "enter".to_owned(),
            DispatchEventKindV1::RenderExit { .. } => "exit".to_owned(),
            DispatchEventKindV1::RenderReturned { .. } => "returned".to_owned(),
            DispatchEventKindV1::AttemptIncomplete { reason, .. } => format!(
                "incomplete-{}",
                match reason {
                    AttemptIncompleteReasonV1::RootNeverOpened => "never-opened",
                    AttemptIncompleteReasonV1::RootNeverClosed => "never-closed",
                    AttemptIncompleteReasonV1::NestedSuppressed => "nested-suppressed",
                    AttemptIncompleteReasonV1::NestedInvalidated => "nested-invalidated",
                }
            ),
            DispatchEventKindV1::ScopeLost { .. } => "scope-lost".to_owned(),
            DispatchEventKindV1::Admitted { .. } => "admitted".to_owned(),
            DispatchEventKindV1::AdmittedNotRendered { reason, .. } => format!(
                "not-rendered-{}",
                match reason {
                    NotRenderedReasonV1::NoLayout => "no-layout",
                    NotRenderedReasonV1::StructuralInvalid => "structural",
                    NotRenderedReasonV1::Contended => "contended",
                    NotRenderedReasonV1::SyncHeld => "sync-held",
                    NotRenderedReasonV1::NoRenderer => "no-renderer",
                    NotRenderedReasonV1::WindowGone => "window-gone",
                    NotRenderedReasonV1::AdapterUnwound => "adapter-unwound",
                }
            ),
            DispatchEventKindV1::Unscoped { what } => format!(
                "unscoped-{}",
                match what {
                    UnscopedKindV1::OutputCheck => "check",
                    UnscopedKindV1::RenderEnter => "enter",
                }
            ),
        })
        .collect();
    assert_eq!(
        names,
        [
            "observed",
            "check",
            "return",
            "call",
            "enter",
            "exit",
            "returned",
            "scope-lost",
            "admitted",
            "decided-sent",
            "decided-suppressed",
            "decided-refused",
            "decided-untargeted",
            "entry-user",
            "entry-redraw",
            "entry-new-events",
            "entry-about-to-wait",
            "entry-resumed",
            "incomplete-never-opened",
            "incomplete-never-closed",
            "incomplete-nested-suppressed",
            "incomplete-nested-invalidated",
            "not-rendered-no-layout",
            "not-rendered-structural",
            "not-rendered-contended",
            "not-rendered-sync-held",
            "not-rendered-no-renderer",
            "not-rendered-window-gone",
            "not-rendered-adapter-unwound",
            "unscoped-check",
            "unscoped-enter",
        ]
    );
}

/// The frozen attempt and witness schemas: every outcome, skip reason, transition target and appearance
/// record matched with no wildcard arm, and every struct built from its public fields.
#[test]
fn the_attempt_and_witness_schemas_are_the_contracts() {
    let outcomes = [
        AttemptOutcomeV1::Skipped(SkipReasonV1::NoPanes),
        AttemptOutcomeV1::Skipped(SkipReasonV1::Unchanged),
        AttemptOutcomeV1::Skipped(SkipReasonV1::Noop),
        AttemptOutcomeV1::CachedReblit,
        AttemptOutcomeV1::AtlasRetry,
        AttemptOutcomeV1::SurfaceRetry,
        AttemptOutcomeV1::Unavailable,
        AttemptOutcomeV1::Presented,
        AttemptOutcomeV1::Failed,
        AttemptOutcomeV1::Unwound,
    ];
    let outcome_names: Vec<&str> = outcomes
        .iter()
        .map(|outcome| match outcome {
            AttemptOutcomeV1::Skipped(SkipReasonV1::NoPanes) => "skipped-no-panes",
            AttemptOutcomeV1::Skipped(SkipReasonV1::Unchanged) => "skipped-unchanged",
            AttemptOutcomeV1::Skipped(SkipReasonV1::Noop) => "skipped-noop",
            AttemptOutcomeV1::CachedReblit => "cached",
            AttemptOutcomeV1::AtlasRetry => "atlas-retry",
            AttemptOutcomeV1::SurfaceRetry => "surface-retry",
            AttemptOutcomeV1::Unavailable => "unavailable",
            AttemptOutcomeV1::Presented => "presented",
            AttemptOutcomeV1::Failed => "failed",
            AttemptOutcomeV1::Unwound => "unwound",
        })
        .collect();
    assert_eq!(
        outcome_names,
        [
            "skipped-no-panes",
            "skipped-unchanged",
            "skipped-noop",
            "cached",
            "atlas-retry",
            "surface-retry",
            "unavailable",
            "presented",
            "failed",
            "unwound",
        ]
    );
    let targets = [TransitionTargetV1::Phase(PhaseV1::Ink), TransitionTargetV1::Closed];
    let target_names: Vec<&str> = targets
        .iter()
        .map(|target| match target {
            TransitionTargetV1::Phase(_) => "phase",
            TransitionTargetV1::Closed => "closed",
        })
        .collect();
    assert_eq!(target_names, ["phase", "closed"]);
    let loss = AppearanceLossV1 { at_ns: 8, generation: 4 };
    let records = [
        AppearanceRecordV1::Appeared { at_ns: 5, generation: 3, lost: Some(loss) },
        AppearanceRecordV1::FirstAppearanceUnobserved { at_ns: 6 },
        AppearanceRecordV1::AlreadyPresent,
        AppearanceRecordV1::IdentityChanged { at_ns: 7 },
        AppearanceRecordV1::ArmGap,
        AppearanceRecordV1::NotSeen,
        AppearanceRecordV1::ArmUnread,
    ];
    let record_names: Vec<&str> = records
        .iter()
        .map(|record| match record {
            AppearanceRecordV1::Appeared { .. } => "appeared",
            AppearanceRecordV1::FirstAppearanceUnobserved { .. } => "first-unobserved",
            AppearanceRecordV1::AlreadyPresent => "already-present",
            AppearanceRecordV1::IdentityChanged { .. } => "identity-changed",
            AppearanceRecordV1::ArmGap => "arm-gap",
            AppearanceRecordV1::NotSeen => "not-seen",
            AppearanceRecordV1::ArmUnread => "arm-unread",
        })
        .collect();
    assert_eq!(
        record_names,
        [
            "appeared",
            "first-unobserved",
            "already-present",
            "identity-changed",
            "arm-gap",
            "not-seen",
            "arm-unread",
        ]
    );
    let built = timeline();
    assert_eq!((built.events.len(), built.attempts.len(), built.transitions.len()), (31, 1, 2));
    assert_eq!(target(), target());
}

/// Every frozen field keeps the type the contract gives it: each is read into an explicitly typed binding,
/// so widening or narrowing one (a `u32` count to `u64`, a `u16` column to `u32`) fails this test's build
/// even where the struct's size would not change.
#[test]
fn every_field_keeps_its_frozen_type() {
    let resolution = WitnessResolutionV1::default();
    let unresolved_at_take: Option<u64> = resolution.unresolved_at_take;
    let abandoned: u32 = resolution.abandoned;
    let built = timeline();
    let attempt = built.attempts[0];
    let attempt_ns: u64 = attempt.attempt_ns;
    let phase_ns: [u64; 11] = attempt.phases.ns;
    let assembled: bool = attempt.assembled;
    let partial_fallbacks: u8 = attempt.partial_fallbacks;
    let transitions: Option<(u16, u16)> = attempt.transitions;
    let event_at_ns: u64 = built.events[0].at_ns;
    let transition_at_ns: u64 = built.transitions[0].at_ns;
    let armed_at: std::time::Instant = built.armed_at;
    let (pane_id, window, overflow): (u64, u64, bool) =
        (built.pane_id, built.window, built.overflow);
    let witness = target();
    let (abs_row, col, character): (u64, u16, char) =
        (witness.abs_row, witness.col, witness.character);
    let identities: (u64, u64, u64) =
        (witness.scrollback_evicted, witness.screen_epoch, witness.size_generation);
    let loss = AppearanceLossV1 { at_ns: 8, generation: 4 };
    let (loss_at_ns, loss_generation): (u64, u64) = (loss.at_ns, loss.generation);
    assert_eq!((unresolved_at_take, abandoned), (None, 0));
    assert_eq!(
        (attempt_ns, phase_ns, assembled, partial_fallbacks, transitions),
        (55, [5; 11], true, 1, Some((0, 2)))
    );
    assert_eq!((event_at_ns, transition_at_ns, pane_id, window, overflow), (0, 4, 7, 11, false));
    assert!(armed_at <= std::time::Instant::now());
    assert_eq!((abs_row, col, character, identities), (120, 3, 'x', (9, 2, 5)));
    assert_eq!((loss_at_ns, loss_generation), (8, 4));
}
