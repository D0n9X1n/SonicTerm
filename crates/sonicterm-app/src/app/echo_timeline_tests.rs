use super::*;
use crate::app::{
    output_event::OutputEvent,
    redraw::{
        redraw_dispatch_tests::{
            at_ms, fake_now, paced_owners, publish_output, set_fake_now, test_base,
        },
        RedrawCause,
    },
    spawn_pane::PaneVtHandles,
    App, ArmOutcome, ArmToken, EchoAppearance, EchoRowIdentity, EchoWatchTarget, TakeOutcome,
};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// With the gate off, the accessor answers `GateOff` for any pane and token, every time, and the
/// event-loop hooks (admission, output service, dispatch, link reset) read no recorder clock and
/// allocate no slot timeline or flood ring.
#[test]
fn gate_off_answers_gate_off_with_no_recorder_clock_read_or_allocation() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_set_dispatch_clock(fake_now);
    let pane_id = app.__test_seed_tab("off");
    let main = app.main_window_id.expect("synthetic main");
    let base = test_base();
    set_fake_now(base);
    let counts_before = recorder_counts();
    assert_eq!(app.arm_echo_watch(pane_id, target()), ArmOutcome::GateOff);
    publish_output(&app, main);
    app.mark_window_redraw(main, RedrawCause::Output);
    redraw_at(&mut app, main, at_ms(base, 20));
    service(&mut app, main, pane_id);
    app.set_software_render_degrade(true);
    for (pane_id, raw) in [(pane_id, 1), (pane_id, 1), (42, 9)] {
        assert_eq!(
            app.take_echo_timeline_v1(pane_id, ArmToken::for_test(raw)),
            EchoTimelineTakeV1::GateOff
        );
    }
    assert_eq!(recorder_counts(), counts_before, "no recorder clock read or allocation");
    assert!(app.windows.values().all(|window| window.redraw.timeline.owner.is_none()));
}

/// The public take protocol through the prerequisite's own arm, take and accessor APIs only:
/// `NotTaken` while armed, the record once after the watch is taken, then `AlreadyTaken`.
#[test]
fn the_public_take_protocol_transfers_a_timeline_once() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let pane_id = app.__test_seed_counting_tab("public");
    let ArmOutcome::Armed(token) = app.arm_echo_watch(pane_id, target()) else {
        panic!("the pane arms");
    };
    assert_eq!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::NotTaken);
    assert!(matches!(app.take_echo_watch(pane_id, token), TakeOutcome::Trace(_)));
    assert!(matches!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::Timeline(_)));
    assert_eq!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::AlreadyTaken);
}

/// The frozen record schema: every field and variant the contract names, built from public fields,
/// and every enum matched with no wildcard arm, so adding, removing or reshaping a variant fails this
/// test's build. A harness builds its own fixtures the same way.
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
    // No wildcard arm: a new decision or defer reason leaves this match non-exhaustive and fails the build.
    let decision_names: Vec<&str> = decisions
        .iter()
        .map(|decision| match decision {
            AdmissionDecisionV1::Permit { .. } => "permit",
            AdmissionDecisionV1::Fallback => "fallback",
            AdmissionDecisionV1::Admitted => "admitted",
            AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout) => "deferred-timeout",
            AdmissionDecisionV1::Deferred(DeferReasonV1::Contention) => "deferred-contention",
            AdmissionDecisionV1::Deferred(DeferReasonV1::Sync) => "deferred-sync",
            AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming) => "deferred-streaming",
            AdmissionDecisionV1::Held => "held",
        })
        .collect();
    assert_eq!(
        decision_names,
        [
            "permit",
            "fallback",
            "admitted",
            "deferred-timeout",
            "deferred-contention",
            "deferred-sync",
            "deferred-streaming",
            "held",
        ]
    );
    let outcomes = [
        OutputCheckOutcomeV1::NativeRequest,
        OutputCheckOutcomeV1::MarkedInFlight,
        OutputCheckOutcomeV1::None,
    ];
    // No wildcard arm: a new check outcome fails the build.
    let outcome_names: Vec<&str> = outcomes
        .iter()
        .map(|outcome| match outcome {
            OutputCheckOutcomeV1::NativeRequest => "native-request",
            OutputCheckOutcomeV1::MarkedInFlight => "marked-in-flight",
            OutputCheckOutcomeV1::None => "none",
        })
        .collect();
    assert_eq!(outcome_names, ["native-request", "marked-in-flight", "none"]);
    let causes = [
        PermitClearCauseV1::AdmissionDiscard,
        PermitClearCauseV1::LinkReset,
        PermitClearCauseV1::LinkPaused,
    ];
    // No wildcard arm: a new clear cause fails the build.
    let cause_names: Vec<&str> = causes
        .iter()
        .map(|cause| match cause {
            PermitClearCauseV1::AdmissionDiscard => "admission-discard",
            PermitClearCauseV1::LinkReset => "link-reset",
            PermitClearCauseV1::LinkPaused => "link-paused",
        })
        .collect();
    assert_eq!(cause_names, ["admission-discard", "link-reset", "link-paused"]);
    let takes = [
        EchoTimelineTakeV1::Timeline(record),
        EchoTimelineTakeV1::NotRecorded,
        EchoTimelineTakeV1::GateOff,
        EchoTimelineTakeV1::NoPane,
        EchoTimelineTakeV1::Mismatch,
        EchoTimelineTakeV1::NotTaken,
        EchoTimelineTakeV1::AlreadyTaken,
    ];
    // No wildcard arm: a new take result fails the build.
    let take_names: Vec<&str> = takes
        .iter()
        .map(|take| match take {
            EchoTimelineTakeV1::Timeline(_) => "timeline",
            EchoTimelineTakeV1::NotRecorded => "not-recorded",
            EchoTimelineTakeV1::GateOff => "gate-off",
            EchoTimelineTakeV1::NoPane => "no-pane",
            EchoTimelineTakeV1::Mismatch => "mismatch",
            EchoTimelineTakeV1::NotTaken => "not-taken",
            EchoTimelineTakeV1::AlreadyTaken => "already-taken",
        })
        .collect();
    assert_eq!(
        take_names,
        [
            "timeline",
            "not-recorded",
            "gate-off",
            "no-pane",
            "mismatch",
            "not-taken",
            "already-taken",
        ]
    );
}

/// A target at row 0, column 2, for `a`, under the default identity.
fn target() -> EchoWatchTarget {
    EchoWatchTarget { abs_row: 0, col: 2, character: 'a', identity: EchoRowIdentity::default() }
}

/// Give window `id`'s active pane counter handles, as a gate-on spawn would, and return its id.
fn watch_active_pane(app: &mut App, id: WindowId) -> u64 {
    let counters = app.pane_frame_counters();
    let window = app.windows.get_mut(&id).expect("live window");
    let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
    window.panes.get_mut(&pane_id).expect("active pane").frame_counters = counters;
    pane_id
}

/// Counting main and child owners on the fake dispatch clock, each active pane watchable.
fn watched_owners() -> (App, WindowId, WindowId, Instant, u64) {
    let base = test_base();
    let (mut app, main, child) = paced_owners(base);
    watch_active_pane(&mut app, child);
    let pane_id = watch_active_pane(&mut app, main);
    (app, main, child, base, pane_id)
}

/// The token an arm must have issued.
fn armed(outcome: ArmOutcome) -> ArmToken {
    match outcome {
        ArmOutcome::Armed(token) => token,
        other => panic!("arm returned {other:?}"),
    }
}

/// `pane_id`'s watch, as a worker clone would hold it.
fn watch_of(app: &App, pane_id: u64) -> Arc<EchoWatch> {
    let pane = app.find_pane(pane_id).expect("live pane");
    Arc::clone(&pane.frame_counters.as_ref().expect("counting pane").echo)
}

/// Take the watch, then the timeline; both must succeed.
fn take_timeline(app: &mut App, pane_id: u64, token: ArmToken) -> EchoTimelineV1 {
    assert!(matches!(app.take_echo_watch(pane_id, token), TakeOutcome::Trace(_)));
    match app.take_echo_timeline_v1(pane_id, token) {
        EchoTimelineTakeV1::Timeline(timeline) => timeline,
        other => panic!("timeline take returned {other:?}"),
    }
}

/// The recorded event kinds, in record order.
fn kinds(timeline: &EchoTimelineV1) -> Vec<EchoTimelineKindV1> {
    timeline.events.iter().map(|event| event.kind).collect()
}

/// The recorded admission decisions, in record order.
fn decisions(timeline: &EchoTimelineV1) -> Vec<AdmissionDecisionV1> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::Admission { decision, .. } => Some(decision),
            _ => None,
        })
        .collect()
}

/// The recorded permit clears as (tick_seq, cause), in record order.
fn clears(timeline: &EchoTimelineV1) -> Vec<(Option<u32>, PermitClearCauseV1)> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::PermitCleared { tick_seq, cause, .. } => Some((tick_seq, cause)),
            _ => None,
        })
        .collect()
}

/// The recorded output checks as (generation, outcome), in record order.
fn checks(timeline: &EchoTimelineV1) -> Vec<(u64, OutputCheckOutcomeV1)> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::OutputCheck { generation, outcome, .. } => {
                Some((generation, outcome))
            }
            _ => None,
        })
        .collect()
}

/// The recorded ticks' identities, in record order.
fn ticks(timeline: &EchoTimelineV1) -> Vec<Option<TickIdentityV1>> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::Tick { identity, .. } => Some(identity),
            _ => None,
        })
        .collect()
}

/// One watched `RedrawRequested` dispatch around the production admission at `at`.
fn redraw_at(app: &mut App, id: WindowId, at: Instant) -> bool {
    set_fake_now(at);
    app.note_dispatch_entry(id);
    let admitted = app.admit_window_redraw(id);
    app.note_dispatch_return(id);
    admitted
}

/// Service one `PaneOutput` event for `pane_id` in window `id`.
fn service(app: &mut App, id: WindowId, pane_id: u64) {
    let now = fake_now();
    app.service_output_event(OutputEvent::Pane { window_id: id, pane_id }, now);
}

/// Install a fake display link on window `id`, so link pacing runs headlessly.
fn install_link(app: &mut App, id: WindowId) {
    let log = std::rc::Rc::new(std::cell::RefCell::new(
        super::super::display_link::FakeLinkLog::default(),
    ));
    app.windows.get_mut(&id).expect("live window").display_link.source =
        Some(Box::new(super::super::display_link::FakeLink(log)));
}

/// Leave a streaming `Link` admission pending at 1 ms and start the link, as the event loop does.
fn pend_link(app: &mut App, id: WindowId, base: Instant) {
    publish_output(app, id);
    app.mark_window_redraw(id, RedrawCause::Output);
    assert!(!redraw_at(app, id, at_ms(base, 1)), "a streaming frame waits for a tick");
    app.sync_display_links();
}

/// Deliver a tick of the window's live generation at `at`, as the native target does.
fn fire_at(app: &mut App, id: WindowId, at: Instant) {
    set_fake_now(at);
    let generation = app.windows[&id].redraw.link_live.expect("the link runs");
    app.handle_display_link_tick(id, generation, at);
}

/// Every v1 event kind survives the internal 24-byte form unchanged, including a `u64::MAX`
/// generation and every `None` identity, which have their own codes rather than zero slots.
#[test]
fn every_event_kind_round_trips_through_the_internal_form() {
    let identity = TickIdentityV1 { tick_seq: u32::MAX, generation: 7 };
    let mut kinds = vec![
        EchoTimelineKindV1::LatestChunkRead,
        EchoTimelineKindV1::Tick { loop_seq: 1, identity: Some(identity) },
        EchoTimelineKindV1::Tick { loop_seq: 2, identity: None },
        EchoTimelineKindV1::DispatchEntry { loop_seq: 3, dispatch_seq: u32::MAX },
        EchoTimelineKindV1::DispatchReturn { loop_seq: u32::MAX, dispatch_seq: 4 },
        EchoTimelineKindV1::RenderEnter { dispatch_seq: 5 },
        EchoTimelineKindV1::RenderExit { dispatch_seq: 5 },
    ];
    for generation in [0, 1, u64::from(u32::MAX) + 1, u64::MAX] {
        for outcome in [
            OutputCheckOutcomeV1::NativeRequest,
            OutputCheckOutcomeV1::MarkedInFlight,
            OutputCheckOutcomeV1::None,
        ] {
            kinds.push(EchoTimelineKindV1::OutputCheck { loop_seq: 9, generation, outcome });
        }
    }
    for decision in [
        AdmissionDecisionV1::Permit { tick_seq: Some(0) },
        AdmissionDecisionV1::Permit { tick_seq: None },
        AdmissionDecisionV1::Fallback,
        AdmissionDecisionV1::Admitted,
        AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Contention),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Sync),
        AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming),
        AdmissionDecisionV1::Held,
    ] {
        kinds.push(EchoTimelineKindV1::Admission { loop_seq: 6, dispatch_seq: 7, decision });
    }
    for cause in [
        PermitClearCauseV1::AdmissionDiscard,
        PermitClearCauseV1::LinkReset,
        PermitClearCauseV1::LinkPaused,
    ] {
        for tick_seq in [Some(0), Some(u32::MAX), None] {
            kinds.push(EchoTimelineKindV1::PermitCleared { loop_seq: 8, tick_seq, cause });
        }
    }
    for (index, kind) in kinds.into_iter().enumerate() {
        let at_ns = u64::MAX - index as u64;
        let decoded = TraceEvent::encode(at_ns, kind).decode();
        assert_eq!(decoded, EchoTimelineEventV1 { at_ns, kind }, "{kind:?}");
    }
}

/// The take protocol: `NotTaken` while armed, `Mismatch` for another token, `NoPane` for an
/// unknown pane; the owner's first watch take freezes the record, the first timeline take transfers
/// it, and every later take answers `AlreadyTaken` without a second copy.
#[test]
fn the_owners_take_freezes_and_transfers_exactly_once() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    assert_eq!(watch.peek_timeline(), "recording");
    app.note_dispatch_entry(main);
    app.note_dispatch_return(main);
    assert_eq!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::NotTaken);
    assert_eq!(
        app.take_echo_timeline_v1(pane_id, ArmToken::for_test(token.get() + 1)),
        EchoTimelineTakeV1::Mismatch
    );
    assert_eq!(app.take_echo_timeline_v1(u64::MAX, token), EchoTimelineTakeV1::NoPane);
    assert!(matches!(app.take_echo_watch(pane_id, token), TakeOutcome::Trace(_)));
    assert_eq!(watch.peek_timeline(), "frozen", "the owner's take froze the record");
    assert!(app.windows[&main].redraw.timeline.owner.is_none(), "and ended the owner");
    let EchoTimelineTakeV1::Timeline(timeline) = app.take_echo_timeline_v1(pane_id, token) else {
        panic!("the frozen record transfers");
    };
    assert_eq!(
        kinds(&timeline),
        [
            EchoTimelineKindV1::DispatchEntry { loop_seq: 1, dispatch_seq: 1 },
            EchoTimelineKindV1::DispatchReturn { loop_seq: 2, dispatch_seq: 1 },
        ]
    );
    assert!(!timeline.overflow);
    assert_eq!(watch.peek_timeline(), "transferred");
    for _ in 0..2 {
        assert_eq!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::AlreadyTaken);
    }
}

/// A same-pane rearm replaces the owner: the earlier token's unfrozen timeline is freed, and the
/// new owner's sequences start again from arm.
#[test]
fn a_same_pane_rearm_replaces_the_owner() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let first = armed(app.arm_echo_watch(pane_id, target()));
    app.note_dispatch_entry(main);
    app.note_dispatch_return(main);
    let second = armed(app.arm_echo_watch(pane_id, target()));
    assert_eq!(
        app.windows[&main].redraw.timeline.owner.as_ref().map(|owner| owner.token),
        Some(second)
    );
    app.note_dispatch_entry(main);
    app.note_dispatch_return(main);
    let timeline = take_timeline(&mut app, pane_id, second);
    assert_eq!(
        kinds(&timeline),
        [
            EchoTimelineKindV1::DispatchEntry { loop_seq: 1, dispatch_seq: 1 },
            EchoTimelineKindV1::DispatchReturn { loop_seq: 2, dispatch_seq: 1 },
        ],
        "the replaced owner's events and sequences are gone"
    );
    assert_eq!(app.take_echo_timeline_v1(pane_id, first), EchoTimelineTakeV1::Mismatch);
}

/// Arming another pane while the owner is active keeps both ordinary watches but records no
/// timeline for the new arming; the owner's ring and sequences continue untouched, and a stale take
/// of the new arming neither copies nor releases the owner's ring.
#[test]
fn an_overlapping_arm_records_nothing_and_leaves_the_owner_untouched() {
    let (mut app, main, _child, _base, owner_pane) = watched_owners();
    let other_pane = app.__test_seed_counting_tab("other");
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let owner_token = armed(app.arm_echo_watch(owner_pane, target()));
    service(&mut app, main, owner_pane);
    let other_token = armed(app.arm_echo_watch(other_pane, target()));
    assert_eq!(watch_of(&app, other_pane).peek_timeline(), "absent");
    assert_eq!(app.windows[&main].redraw.timeline.watched_pane(), Some(owner_pane));
    service(&mut app, main, other_pane);
    // A stale take of the non-owner pane with the owner's token, then its own take: neither touches the ring.
    assert_eq!(app.take_echo_watch(other_pane, owner_token), TakeOutcome::Mismatch);
    assert!(matches!(app.take_echo_watch(other_pane, other_token), TakeOutcome::Trace(_)));
    assert_eq!(app.take_echo_timeline_v1(other_pane, other_token), EchoTimelineTakeV1::NotRecorded);
    assert_eq!(watch_of(&app, owner_pane).peek_timeline(), "recording");
    let timeline = take_timeline(&mut app, owner_pane, owner_token);
    assert_eq!(
        timeline.flood_services,
        [
            FloodServiceV1 { loop_seq: 1, pane_id: owner_pane },
            FloodServiceV1 { loop_seq: 2, pane_id: other_pane },
        ],
        "the owner's ring kept both services, in loop order"
    );
    assert_eq!(timeline.flood_evicted_through, None);
}

/// A stale take with an earlier token of the owner's own pane cannot copy, clear or release the
/// current owner's ring.
#[test]
fn a_stale_take_cannot_touch_another_tokens_ring() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let stale = armed(app.arm_echo_watch(pane_id, target()));
    let current = armed(app.arm_echo_watch(pane_id, target()));
    service(&mut app, main, pane_id);
    assert_eq!(app.take_echo_watch(pane_id, stale), TakeOutcome::Mismatch);
    assert_eq!(app.take_echo_timeline_v1(pane_id, stale), EchoTimelineTakeV1::Mismatch);
    assert_eq!(app.windows[&main].redraw.timeline.watched_pane(), Some(pane_id));
    assert_eq!(watch_of(&app, pane_id).peek_timeline(), "recording");
    let timeline = take_timeline(&mut app, pane_id, current);
    assert_eq!(timeline.flood_services, [FloodServiceV1 { loop_seq: 1, pane_id }]);
}

/// A pane transferred out of the owner window ends that window's timeline without complete
/// evidence: the ordinary watch still takes, the timeline answers `NotRecorded`.
#[test]
fn a_pane_transfer_ends_the_timeline_without_complete_evidence() {
    let (mut app, main, child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    service(&mut app, main, pane_id);
    let pane = app.windows.get_mut(&main).unwrap().remove_pane(pane_id).expect("owner pane");
    app.windows.get_mut(&child).unwrap().panes.insert(pane_id, pane);
    assert!(app.windows[&main].redraw.timeline.owner.is_none());
    assert_eq!(watch_of(&app, pane_id).peek_timeline(), "absent");
    assert!(matches!(app.take_echo_watch(pane_id, token), TakeOutcome::Trace(_)));
    assert_eq!(app.take_echo_timeline_v1(pane_id, token), EchoTimelineTakeV1::NotRecorded);
}

/// Closing the owner window releases the slot timeline through the owner's own release, while a
/// worker handle keeps the watch alive: the final worker `Arc` is not the cleanup.
#[test]
fn closing_the_owner_window_releases_the_timeline_while_a_worker_handle_survives() {
    let (mut app, _main, child, _base, _main_pane) = watched_owners();
    let pane_id = app.windows[&child].tab_states[0].active_pane;
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let handles = PaneVtHandles::from_pane_state(app.find_pane(pane_id).expect("child pane"));
    let watch = watch_of(&app, pane_id);
    let weak = Arc::downgrade(&watch);
    drop(app.windows.remove(&child).expect("child window"));
    assert_eq!(watch.peek_timeline(), "absent", "the closed window's owner released it");
    assert!(weak.upgrade().is_some(), "the worker handle still owns the watch");
    assert_eq!(
        watch.armed_token(),
        token.get(),
        "the ordinary watch is the pane's, not the window's"
    );
    drop(handles);
}

/// Retirement disarms the watch and releases its timeline under the slot protocol while a worker
/// handle survives, ends the window's owner, and a post-close stale writer changes nothing.
#[test]
fn retirement_releases_the_timeline_and_a_stale_writer_changes_nothing() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let handles = PaneVtHandles::from_pane_state(app.find_pane(pane_id).expect("owner pane"));
    let watch = watch_of(&app, pane_id);
    let pane = app.windows.get_mut(&main).unwrap().panes.remove(&pane_id).expect("owner pane");
    assert!(app.windows[&main].redraw.timeline.owner.is_some(), "a bare removal ended nothing");
    app.retire_pane(pane);
    assert!(app.windows[&main].redraw.timeline.owner.is_none(), "retirement ended the owner");
    assert_eq!(watch.peek_timeline(), "absent");
    assert_eq!(watch.armed_token(), 0, "and disarmed the watch");
    let at = Instant::now();
    assert!(!watch.record(token.get(), at, |trace| trace.lost = true), "a stale writer is refused");
    watch.write_timeline(token, at, |timeline, _, elapsed| {
        timeline.record(elapsed, Some(EchoTimelineKindV1::LatestChunkRead));
    });
    assert_eq!(watch.peek_timeline(), "absent");
    assert!(!watch.peek().expect("record kept").lost);
    drop(handles);
}

/// A `loop_seq` or `dispatch_seq` past `u32::MAX` drops its event and sets `overflow`; nothing is
/// truncated or zeroed, and a dispatch whose sequence does not fit drops its render pair too.
#[test]
fn unrepresentable_sequences_drop_their_events_and_set_overflow() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    app.windows.get_mut(&main).unwrap().redraw.timeline.owner.as_mut().unwrap().loop_seq =
        u64::from(u32::MAX) - 1;
    app.note_dispatch_entry(main);
    app.note_dispatch_return(main);
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        kinds(&timeline),
        [EchoTimelineKindV1::DispatchEntry { loop_seq: u32::MAX, dispatch_seq: 1 }]
    );
    assert!(timeline.overflow, "the return's loop_seq did not fit");

    let token = armed(app.arm_echo_watch(pane_id, target()));
    app.windows.get_mut(&main).unwrap().redraw.timeline.owner.as_mut().unwrap().dispatch_seq =
        u64::from(u32::MAX);
    app.note_dispatch_entry(main);
    let marker = app.render_marker(main).expect("a watched dispatch is in progress");
    marker.enter();
    marker.exit_at(Instant::now());
    app.note_dispatch_return(main);
    let timeline = take_timeline(&mut app, pane_id, token);
    assert!(timeline.events.is_empty(), "every event of that dispatch was dropped");
    assert!(timeline.overflow);
}

/// A tick identity is `None` once `tick_seq` or the generation does not fit `u32`; `tick_seq`
/// is never reset, saturates at `u64::MAX` rather than wrapping, and a stale generation has none.
#[test]
fn tick_identities_are_never_truncated_and_tick_seq_exhaustion_is_pinned() {
    let mut timeline =
        WindowTimeline { tick_seq: u64::from(u32::MAX) - 1, ..WindowTimeline::default() };
    let now = Instant::now();
    timeline.accept_tick(3, now);
    assert_eq!(
        timeline.identity_for(3),
        Some(TickIdentityV1 { tick_seq: u32::MAX, generation: 3 })
    );
    assert_eq!(timeline.identity_for(2), None, "a stale generation names no tick");
    timeline.accept_tick(3, now);
    assert_eq!(timeline.identity_for(3), None, "tick_seq past u32::MAX");
    timeline.tick_seq = 2;
    timeline.accept_tick(u64::from(u32::MAX) + 1, now);
    assert_eq!(timeline.identity_for(u64::from(u32::MAX) + 1), None, "generation past u32::MAX");
    timeline.tick_seq = u64::MAX;
    timeline.accept_tick(5, now);
    assert_eq!(timeline.tick_seq, u64::MAX, "exhausted tick_seq saturates, never wraps to 0");
    assert_eq!(timeline.identity_for(5), None);
}

/// The record a slot test builds: a fresh timeline whose transfer is frozen with no flood.
fn frozen(mut timeline: Box<SlotTimeline>) -> EchoTimelineV1 {
    timeline.freeze(FrozenFlood::empty());
    timeline.transfer().expect("frozen")
}

/// `elapsed` of `millis` milliseconds.
fn millis(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// Identical repeats are compressed and the first occurrence's time stands; an outcome change at an
/// unchanged generation is kept; the first check after arm is kept at any generation.
#[test]
fn output_checks_compress_repeats_and_keep_changes() {
    let mut timeline = SlotTimeline::new(false, InitialPermit::Absent);
    let none = OutputCheckOutcomeV1::None;
    let native = OutputCheckOutcomeV1::NativeRequest;
    timeline.note_check(millis(1), Some(1), 4, none, None);
    timeline.note_check(millis(2), Some(2), 4, none, None);
    timeline.note_check(millis(3), Some(3), 4, native, None);
    timeline.note_check(millis(4), Some(4), 4, native, None);
    let record = frozen(timeline);
    assert_eq!(
        record.events,
        [
            EchoTimelineEventV1 {
                at_ns: 1_000_000,
                kind: EchoTimelineKindV1::OutputCheck { loop_seq: 1, generation: 4, outcome: none },
            },
            EchoTimelineEventV1 {
                at_ns: 3_000_000,
                kind: EchoTimelineKindV1::OutputCheck {
                    loop_seq: 3,
                    generation: 4,
                    outcome: native
                },
            },
        ],
        "the first check (an unchanged generation) and the outcome change, each at its first time"
    );
}

/// The race: an appearance recorded after a qualifying check still selects that earliest check,
/// never a later repeat's time; recording stops only once the appearance and a check reaching it
/// are both present.
#[test]
fn output_checks_stop_only_after_the_appearance_and_a_qualifying_check() {
    let mut timeline = SlotTimeline::new(false, InitialPermit::Absent);
    let none = OutputCheckOutcomeV1::None;
    timeline.note_check(millis(1), Some(1), 3, none, None);
    timeline.note_check(millis(2), Some(2), 5, none, None);
    // The appearance (generation 5) is now known: the generation-5 repeat is not recorded.
    timeline.note_check(millis(3), Some(3), 5, none, Some(5));
    timeline.note_check(millis(4), Some(4), 6, none, Some(5));
    let record = frozen(timeline);
    assert_eq!(
        record.events.iter().map(|event| event.at_ns).collect::<Vec<_>>(),
        [1_000_000, 2_000_000],
        "the qualifying check keeps its own first time and recording stopped"
    );

    let mut timeline = SlotTimeline::new(false, InitialPermit::Absent);
    timeline.note_check(millis(1), Some(1), 3, none, Some(5));
    timeline.note_check(millis(2), Some(2), 4, none, Some(5));
    timeline.note_check(millis(3), Some(3), 5, none, Some(5));
    timeline.note_check(millis(4), Some(4), 6, none, Some(5));
    assert_eq!(
        frozen(timeline).events.len(),
        3,
        "it continued until a check reached the appearance"
    );
}

/// A check written with a stale token is discarded under the slot lock, like every writer.
#[test]
fn a_stale_tokens_check_is_discarded() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let stale = armed(app.arm_echo_watch(pane_id, target()));
    let current = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    watch.write_timeline(stale, Instant::now(), |timeline, appearance, elapsed| {
        timeline.note_check(elapsed, Some(9), 1, OutputCheckOutcomeV1::None, appearance);
    });
    service(&mut app, main, pane_id);
    let timeline = take_timeline(&mut app, pane_id, current);
    assert_eq!(
        checks(&timeline),
        [(0, OutputCheckOutcomeV1::None)],
        "only the current token's check"
    );
}

/// The Pane-output service records the watched pane's generation with its Output request outcome;
/// a check may precede the publication (`ready_before_publication`).
#[test]
fn the_output_service_records_a_check_before_publication() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    let appeared_at = Instant::now();
    assert!(watch.record(token.get(), appeared_at, |trace| {
        trace.appearance =
            Some(EchoAppearance { generation: 1, parsed_at: appeared_at, sync_open: false });
    }));
    publish_output(&app, main);
    service(&mut app, main, pane_id);
    let TakeOutcome::Trace(trace) = app.take_echo_watch(pane_id, token) else {
        panic!("the watch takes");
    };
    assert_eq!(trace.publication, None, "no flush was published");
    let EchoTimelineTakeV1::Timeline(timeline) = app.take_echo_timeline_v1(pane_id, token) else {
        panic!("the timeline transfers");
    };
    assert_eq!(checks(&timeline), [(1, OutputCheckOutcomeV1::NativeRequest)]);
}

/// The streaming admission check is an observation too: when it is the first check at the
/// appearance's generation, it is the one recorded (outcome `None`), before its admission decision.
#[test]
fn the_admission_check_can_be_the_first_qualifying_observation() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    app.windows.get_mut(&main).unwrap().redraw.pacing =
        Some(super::super::display_link::PacingMode::Timer);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let watch = watch_of(&app, pane_id);
    let appeared_at = Instant::now();
    assert!(watch.record(token.get(), appeared_at, |trace| {
        trace.appearance =
            Some(EchoAppearance { generation: 1, parsed_at: appeared_at, sync_open: false });
    }));
    publish_output(&app, main);
    app.mark_window_redraw(main, RedrawCause::Output);
    assert!(!redraw_at(&mut app, main, at_ms(base, 1)), "streaming waits for the period");
    service(&mut app, main, pane_id);
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        checks(&timeline),
        [(1, OutputCheckOutcomeV1::None)],
        "the admission check qualified"
    );
    assert!(matches!(
        kinds(&timeline)[1..3],
        [
            EchoTimelineKindV1::OutputCheck { .. },
            EchoTimelineKindV1::Admission {
                decision: AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming),
                ..
            },
        ]
    ));
}

/// The visibility check short-circuits on an earlier advanced pane and never reads the watched
/// pane, which is not even visible; the explicit load still records the watched pane's generation.
#[test]
fn the_explicit_load_samples_the_watched_pane_beside_a_short_circuiting_any() {
    let (mut app, main, _child, _base, visible_pane) = watched_owners();
    let hidden_pane = app.__test_seed_counting_tab("hidden");
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let token = armed(app.arm_echo_watch(hidden_pane, target()));
    publish_output(&app, main);
    for _ in 0..2 {
        app.windows[&main].panes[&hidden_pane].output_generation.fetch_add(1, Ordering::Release);
    }
    service(&mut app, main, visible_pane);
    let timeline = take_timeline(&mut app, hidden_pane, token);
    assert_eq!(checks(&timeline), [(2, OutputCheckOutcomeV1::NativeRequest)]);
    assert_eq!(timeline.flood_services, [FloodServiceV1 { loop_seq: 1, pane_id: visible_pane }]);
}

/// A tick accepted before arm names the initial permit with its delivery instant; a consumed permit
/// carries that tick's identity, and no clear is recorded for a consumption.
#[test]
fn the_initial_permit_comes_from_a_pre_arm_tick_and_a_permit_consumes_it() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    let token = armed(app.arm_echo_watch(pane_id, target()));
    assert!(redraw_at(&mut app, main, at_ms(base, 16)), "the tick admits");
    let timeline = take_timeline(&mut app, pane_id, token);
    let identity = TickIdentityV1 {
        tick_seq: 1,
        generation: u32::try_from(
            app.windows[&main].redraw.link_generation.load(Ordering::Acquire),
        )
        .unwrap(),
    };
    assert_eq!(
        timeline.initial_permit,
        Some(PermitSnapshotV1 { identity, delivered_at: at_ms(base, 16) })
    );
    assert!(!timeline.initial_permit_unknown, "never both");
    assert_eq!(decisions(&timeline), [AdmissionDecisionV1::Permit { tick_seq: Some(1) }]);
    assert!(clears(&timeline).is_empty(), "a consumption is not a clear");
    assert!(app.windows[&main].redraw.link_permit.is_none());
}

/// A permit held at arm whose tick identity is not representable is `initial_permit_unknown`, never
/// both; its consumption reports no `tick_seq`.
#[test]
fn an_unrepresentable_permit_at_arm_is_unknown() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    pend_link(&mut app, main, base);
    app.windows.get_mut(&main).unwrap().redraw.timeline.tick_seq = u64::from(u32::MAX);
    fire_at(&mut app, main, at_ms(base, 16));
    let token = armed(app.arm_echo_watch(pane_id, target()));
    assert!(redraw_at(&mut app, main, at_ms(base, 16)));
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!((timeline.initial_permit, timeline.initial_permit_unknown), (None, true));
    assert_eq!(decisions(&timeline), [AdmissionDecisionV1::Permit { tick_seq: None }]);
}

/// Ticks after arm are recorded with their identity; a replacement tick while a permit is held is
/// another `Tick`, with no clear; the admission consumes the replacing tick's permit.
#[test]
fn a_replacement_tick_is_recorded_without_a_clear() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    fire_at(&mut app, main, at_ms(base, 17));
    assert!(redraw_at(&mut app, main, at_ms(base, 17)));
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!((timeline.initial_permit, timeline.initial_permit_unknown), (None, false));
    assert_eq!(
        ticks(&timeline)
            .iter()
            .map(|identity| identity.map(|tick| tick.tick_seq))
            .collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
    assert_eq!(
        decisions(&timeline),
        [
            AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming),
            AdmissionDecisionV1::Permit { tick_seq: Some(2) },
        ]
    );
    assert!(clears(&timeline).is_empty());
}

/// A stale tick is rejected and records nothing: no `Tick`, no metadata, no permit.
#[test]
fn a_stale_tick_records_nothing() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    let live = app.windows[&main].redraw.link_live.expect("the link runs");
    set_fake_now(at_ms(base, 16));
    app.handle_display_link_tick(main, live - 1, at_ms(base, 16));
    assert!(app.windows[&main].redraw.link_permit.is_none());
    assert_eq!(app.windows[&main].redraw.timeline.tick_seq, 0);
    assert!(ticks(&take_timeline(&mut app, pane_id, token)).is_empty());
}

/// Without a tick, the fallback ceiling admits a link-paced streaming frame as `Fallback`, which
/// holds and discards no permit.
#[test]
fn the_fallback_ceiling_admits_as_fallback() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    assert!(redraw_at(&mut app, main, at_ms(base, 20)), "admitted at the two-period ceiling");
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        decisions(&timeline),
        [AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming), AdmissionDecisionV1::Fallback]
    );
    assert!(clears(&timeline).is_empty());
}

/// An admission that is not a `Permit` discards a held permit, recorded as its own
/// `PermitCleared { AdmissionDiscard }` with the tick's identity.
#[test]
fn a_non_permit_admission_records_its_discard() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    let window = app.windows.get_mut(&main).unwrap();
    let generation = window.panes[&pane_id].output_generation.load(Ordering::Acquire);
    window.panes.get_mut(&pane_id).unwrap().observed_output_generation = generation;
    // Pending input with no new output is not streaming work, so no tick is consumed.
    app.mark_window_redraw(main, RedrawCause::Input);
    assert!(redraw_at(&mut app, main, at_ms(base, 16)), "no streaming work is admitted");
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(decisions(&timeline)[1], AdmissionDecisionV1::Admitted);
    assert_eq!(clears(&timeline), [(Some(1), PermitClearCauseV1::AdmissionDiscard)]);
}

/// Link invalidation clears a held permit as `LinkReset`, including the software-degrade loop; a
/// second invalidation with no permit held records nothing.
#[test]
fn link_reset_records_the_clear_including_software_degrade() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    app.set_software_render_degrade(true);
    app.windows.get_mut(&main).unwrap().redraw.invalidate_link_pacing();
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(clears(&timeline), [(Some(1), PermitClearCauseV1::LinkReset)]);
}

/// Pausing the live interval clears a held permit as `LinkPaused`.
#[test]
fn a_link_pause_records_the_clear() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    app.windows.get_mut(&main).unwrap().redraw.pacing = None;
    app.sync_display_links();
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(clears(&timeline), [(Some(1), PermitClearCauseV1::LinkPaused)]);
}

/// `Held` (a hidden window) and `Deferred` (a surface timeout) leave a held permit untouched.
#[test]
fn held_and_deferred_leave_the_permit_untouched() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    app.windows.get_mut(&main).unwrap().hidden = true;
    assert!(!redraw_at(&mut app, main, at_ms(base, 16)));
    let window = app.windows.get_mut(&main).unwrap();
    window.hidden = false;
    window.redraw.timeout_pending = true;
    window.last_render = at_ms(base, 16);
    assert!(!redraw_at(&mut app, main, at_ms(base, 17)));
    assert!(app.windows[&main].redraw.link_permit.is_some(), "the permit is still held");
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        decisions(&timeline),
        [
            AdmissionDecisionV1::Deferred(DeferReasonV1::Streaming),
            AdmissionDecisionV1::Held,
            AdmissionDecisionV1::Deferred(DeferReasonV1::Timeout),
        ]
    );
    assert!(clears(&timeline).is_empty());
}

/// A device-refused admission records `Held` first and then resets the link, so a held permit's
/// separate `LinkReset` follows it. Pinned by source: a refusing device needs a GPU renderer.
#[test]
fn a_device_refused_held_precedes_its_separate_link_reset() {
    let source = include_str!("redraw.rs");
    let start = source.find("if !renderer.device_accepts_gpu_work() {").expect("device branch");
    let branch = &source[start..start + source[start..].find("return false;").expect("refusal")];
    let held = branch.find("AdmissionDecisionV1::Held").expect("the refusal is Held");
    let reset = branch.find("invalidate_link_pacing()").expect("then the link resets");
    assert!(held < reset, "{branch}");
}

/// The flood ring keeps the last 256 services oldest first; the 257th evicts the first, recorded
/// by its `loop_seq`.
#[test]
fn the_flood_ring_evicts_by_loop_seq() {
    let mut ring = FloodRing::new();
    for loop_seq in 1..=256 {
        ring.push(loop_seq, u64::from(loop_seq) + 100);
    }
    let full = ring.freeze();
    assert_eq!((full.services.len(), full.evicted_through), (256, None));
    ring.push(257, 7);
    let evicted = ring.freeze();
    assert_eq!(evicted.evicted_through, Some(1));
    assert_eq!(evicted.services.len(), 256);
    assert_eq!(evicted.services[0], FloodServiceV1 { loop_seq: 2, pane_id: 102 });
    assert_eq!(evicted.services[255], FloodServiceV1 { loop_seq: 257, pane_id: 7 });
}

/// Other panes' services enter the owner's ring by `loop_seq`, interleaved with the watched pane's
/// own; each recorded check shares its service's `loop_seq`, and a repeated check adds only a flood entry.
#[test]
fn flood_services_are_counted_by_loop_seq() {
    let (mut app, main, _child, _base, owner_pane) = watched_owners();
    let other_pane = app.__test_seed_counting_tab("other");
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let token = armed(app.arm_echo_watch(owner_pane, target()));
    service(&mut app, main, other_pane);
    publish_output(&app, main);
    service(&mut app, main, owner_pane);
    service(&mut app, main, other_pane);
    service(&mut app, main, other_pane);
    let timeline = take_timeline(&mut app, owner_pane, token);
    assert_eq!(
        timeline.flood_services,
        [
            FloodServiceV1 { loop_seq: 1, pane_id: other_pane },
            FloodServiceV1 { loop_seq: 2, pane_id: owner_pane },
            FloodServiceV1 { loop_seq: 3, pane_id: other_pane },
            FloodServiceV1 { loop_seq: 4, pane_id: other_pane },
        ]
    );
    assert_eq!(
        checks(&timeline),
        [
            (0, OutputCheckOutcomeV1::None),
            (1, OutputCheckOutcomeV1::NativeRequest),
            (1, OutputCheckOutcomeV1::MarkedInFlight),
        ]
    );
    let check_loops: Vec<u32> = kinds(&timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::OutputCheck { loop_seq, .. } => Some(loop_seq),
            _ => None,
        })
        .collect();
    assert_eq!(check_loops, [1, 2, 3], "the fourth service repeated the third's check");
}

/// With the gate on and nothing armed, every event-loop hook (tick, admission, service, dispatch)
/// takes no slot lock, reads no recorder clock, allocates nothing, and no window gains an owner.
#[test]
fn unarmed_hooks_take_no_slot_lock_clock_read_or_allocation() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    let locks_before = super::super::echo_watch::slot_locks();
    let counts_before = recorder_counts();
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    assert!(redraw_at(&mut app, main, at_ms(base, 16)));
    service(&mut app, main, pane_id);
    app.set_software_render_degrade(true);
    assert_eq!(super::super::echo_watch::slot_locks(), locks_before);
    assert_eq!(recorder_counts(), counts_before);
    assert!(app.windows[&main].redraw.timeline.owner.is_none());
    assert!(app.render_marker(main).is_none());
}

/// One render pair per `dispatch_seq`, through the marker a watched dispatch hands its adapter; no
/// marker exists outside a watched dispatch, and an unwatched window records no boundary.
#[test]
fn a_watched_dispatch_records_one_render_pair() {
    let (mut app, main, child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    assert!(app.render_marker(main).is_none(), "no dispatch in progress");
    for _ in 0..2 {
        app.note_dispatch_entry(main);
        let marker = app.render_marker(main).expect("watched dispatch");
        marker.enter();
        marker.exit_at(Instant::now());
        drop(marker);
        app.note_dispatch_return(main);
    }
    app.note_dispatch_entry(child);
    assert!(app.render_marker(child).is_none(), "the child window has no owner");
    app.note_dispatch_return(child);
    let timeline = take_timeline(&mut app, pane_id, token);
    let pair = |loop_seq: u32, dispatch_seq: u32| {
        [
            EchoTimelineKindV1::DispatchEntry { loop_seq, dispatch_seq },
            EchoTimelineKindV1::RenderEnter { dispatch_seq },
            EchoTimelineKindV1::RenderExit { dispatch_seq },
            EchoTimelineKindV1::DispatchReturn { loop_seq: loop_seq + 1, dispatch_seq },
        ]
    };
    assert_eq!(kinds(&timeline), [pair(1, 1), pair(3, 2)].concat());
}

/// Both render adapters wrap the renderer call itself with the marker, and the outer handler
/// records the entry before dispatching and the return after its frame bookkeeping.
#[test]
fn the_boundaries_sit_at_their_anchors() {
    for source in [include_str!("window_event.rs"), include_str!("child_window_redraw.rs")] {
        let enter = source.find("marker.enter();").expect("render enter");
        let call = source.find("r.render_releasing(").expect("render call");
        let returned = source.find("let returned_at =").expect("one renderer-return read");
        let exit = source.find("marker.exit_at(returned_at);").expect("render exit");
        let rendered = source.find("dispatch.rendered_at(returned_at);").expect("dispatch close");
        // The return is read once after the call, then closes the render pair and the dispatch.
        assert!(enter < call && call < returned && returned < exit && exit < rendered);
        assert_eq!(source.matches("marker.enter();").count(), 1);
        assert_eq!(source.matches("let returned_at =").count(), 1);
        assert_eq!(source.matches("marker.exit_at(returned_at);").count(), 1);
        let order = source.find("// Acquisition order here is parser guards, then the echo slot");
        assert!(
            order.is_some_and(|order| order < enter),
            "the order is documented at the enter anchor"
        );
    }
    let source = include_str!("mod.rs");
    let handler = &source[source.find("fn window_event(&mut self").expect("outer handler")..];
    let handler = &handler[..handler.find("\n    }\n").expect("handler end")];
    let entry = handler.find("self.note_dispatch_entry(win_id);").expect("entry");
    let dispatch = handler.find("self.do_window_event(").expect("dispatch");
    let lines = handler.find("self.emit_frame_lines(").expect("frame lines");
    let returned = handler.find("self.note_dispatch_return(win_id);").expect("return");
    assert!(entry < dispatch && dispatch < lines && lines < returned, "{handler}");
}

/// `tick_seq` is the window's accepted-tick count and is not reset at arm: a tick accepted after
/// the arm continues the numbering of a tick accepted before it.
#[test]
fn tick_seq_continues_across_an_arm() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    assert!(redraw_at(&mut app, main, at_ms(base, 16)), "the pre-arm tick admits");
    let token = armed(app.arm_echo_watch(pane_id, target()));
    publish_output(&app, main);
    app.mark_window_redraw(main, RedrawCause::Output);
    assert!(!redraw_at(&mut app, main, at_ms(base, 17)), "the next frame waits for a tick");
    app.sync_display_links();
    fire_at(&mut app, main, at_ms(base, 18));
    assert!(redraw_at(&mut app, main, at_ms(base, 18)));
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        ticks(&timeline)
            .iter()
            .map(|identity| identity.map(|tick| tick.tick_seq))
            .collect::<Vec<_>>(),
        [Some(2)],
        "the post-arm tick is the window's second"
    );
    assert_eq!(
        decisions(&timeline).last(),
        Some(&AdmissionDecisionV1::Permit { tick_seq: Some(2) })
    );
}

/// A check is written at its observation's instant, at both sites: a publish between the explicit
/// load and a delayed record leaves the check at the load's time and generation, before that publish.
#[test]
fn a_check_keeps_its_observation_instant_across_a_delayed_record() {
    for admission in [false, true] {
        let (mut app, main, _child, _base, pane_id) = watched_owners();
        let token = armed(app.arm_echo_watch(pane_id, target()));
        let observation = app.windows[&main].watched_observation().expect("an owner observes");
        std::thread::sleep(Duration::from_millis(5));
        // The worker publishes the next batch after the load and before the record.
        app.windows[&main].panes[&pane_id].output_generation.fetch_add(1, Ordering::Release);
        let published_at = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        let timeline = &mut app.windows.get_mut(&main).unwrap().redraw.timeline;
        if admission {
            timeline.note_admission(AdmissionDecisionV1::Admitted, None, Some(observation));
        } else {
            let outcome = OutputCheckOutcomeV1::NativeRequest;
            timeline.note_output_service(pane_id, Some(observation), outcome);
        }
        let TakeOutcome::Trace(trace) = app.take_echo_watch(pane_id, token) else {
            panic!("the watch takes");
        };
        let EchoTimelineTakeV1::Timeline(timeline) = app.take_echo_timeline_v1(pane_id, token)
        else {
            panic!("the timeline transfers");
        };
        let check = timeline
            .events
            .iter()
            .find(|event| matches!(event.kind, EchoTimelineKindV1::OutputCheck { .. }))
            .expect("the check is recorded");
        let checked_at = trace.armed_at + Duration::from_nanos(check.at_ns);
        assert_eq!(checked_at, observation.observed_at, "admission: {admission}");
        assert!(checked_at < published_at, "admission: {admission}");
        assert_eq!(checks(&timeline)[0].0, 0, "the generation the load saw");
    }
}

/// A delayed exit recording never extends the dispatch interval: the renderer's return is read once,
/// and the `RenderExit` event and the scalar dispatch interval both end exactly there.
#[test]
fn a_delayed_exit_record_does_not_extend_the_dispatch_interval() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let totals = Arc::new(crate::app::guard_custody::CustodyTotals::default());
    let first_guard = Instant::now();
    let (custody, dispatch) = crate::app::guard_custody::start_at(&totals, first_guard);
    drop(custody);
    app.note_dispatch_entry(main);
    let marker = app.render_marker(main).expect("watched dispatch");
    marker.enter();
    let returned_at = Instant::now();
    // The exit's write waits 30 ms before it takes the slot, as a contended slot would.
    crate::app::echo_watch::pause_next_timeline_write(|| {
        std::thread::sleep(Duration::from_millis(30));
    });
    marker.exit_at(returned_at);
    dispatch.rendered_at(returned_at);
    app.note_dispatch_return(main);
    let dispatch_ns = totals.dispatch_ns.load(Ordering::Relaxed);
    assert_eq!(u128::from(dispatch_ns), returned_at.duration_since(first_guard).as_nanos());
    let TakeOutcome::Trace(trace) = app.take_echo_watch(pane_id, token) else {
        panic!("the watch takes");
    };
    let EchoTimelineTakeV1::Timeline(timeline) = app.take_echo_timeline_v1(pane_id, token) else {
        panic!("the timeline transfers");
    };
    let exit = timeline
        .events
        .iter()
        .find(|event| matches!(event.kind, EchoTimelineKindV1::RenderExit { .. }))
        .expect("the exit is recorded");
    assert_eq!(trace.armed_at + Duration::from_nanos(exit.at_ns), returned_at);
}

/// A service or admission check whose `loop_seq` does not fit sets overflow even when the check
/// itself is compressed (a repeat) or recording had stopped (already qualified); the retained
/// events and flood entries are unchanged.
#[test]
fn a_lost_sequence_sets_overflow_behind_a_compressed_check() {
    for qualified in [false, true] {
        let (mut app, main, _child, _base, pane_id) = watched_owners();
        let token = armed(app.arm_echo_watch(pane_id, target()));
        if qualified {
            let watch = watch_of(&app, pane_id);
            let appeared_at = Instant::now();
            assert!(watch.record(token.get(), appeared_at, |trace| {
                trace.appearance = Some(EchoAppearance {
                    generation: 0,
                    parsed_at: appeared_at,
                    sync_open: false,
                });
            }));
        }
        let owner = app.windows.get_mut(&main).unwrap().redraw.timeline.owner.as_mut().unwrap();
        owner.loop_seq = u64::from(u32::MAX) - 1;
        service(&mut app, main, pane_id);
        service(&mut app, main, pane_id);
        let timeline = take_timeline(&mut app, pane_id, token);
        let check = EchoTimelineKindV1::OutputCheck {
            loop_seq: u32::MAX,
            generation: 0,
            outcome: OutputCheckOutcomeV1::None,
        };
        assert_eq!(kinds(&timeline), [check], "qualified: {qualified}");
        assert_eq!(timeline.flood_services, [FloodServiceV1 { loop_seq: u32::MAX, pane_id }]);
        assert!(timeline.overflow, "qualified: {qualified}");
    }
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    let owner = app.windows.get_mut(&main).unwrap().redraw.timeline.owner.as_mut().unwrap();
    owner.loop_seq = u64::from(u32::MAX) - 1;
    for _ in 0..2 {
        let observation = app.windows[&main].watched_observation();
        let timeline = &mut app.windows.get_mut(&main).unwrap().redraw.timeline;
        timeline.note_admission(AdmissionDecisionV1::Admitted, None, observation);
    }
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(checks(&timeline), [(0, OutputCheckOutcomeV1::None)]);
    assert!(timeline.overflow, "the admission check's lost sequence");
}

/// The 65th event is dropped with overflow set; the 64 retained events and the ordinary echo record
/// are kept.
#[test]
fn the_sixty_fifth_event_overflows_and_keeps_the_prefix_and_trace() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(app.arm_echo_watch(pane_id, target()));
    for _ in 0..33 {
        app.note_dispatch_entry(main);
        app.note_dispatch_return(main);
    }
    let watch = watch_of(&app, pane_id);
    let appeared_at = Instant::now();
    assert!(watch.record(token.get(), appeared_at, |trace| {
        trace.appearance =
            Some(EchoAppearance { generation: 3, parsed_at: appeared_at, sync_open: false });
    }));
    let TakeOutcome::Trace(trace) = app.take_echo_watch(pane_id, token) else {
        panic!("the watch takes");
    };
    assert_eq!(trace.appearance.map(|appearance| appearance.generation), Some(3));
    let EchoTimelineTakeV1::Timeline(timeline) = app.take_echo_timeline_v1(pane_id, token) else {
        panic!("the timeline transfers");
    };
    assert_eq!(timeline.events.len(), TIMELINE_EVENT_CAPACITY);
    assert!(timeline.overflow);
    assert_eq!(
        timeline.events[0].kind,
        EchoTimelineKindV1::DispatchEntry { loop_seq: 1, dispatch_seq: 1 }
    );
    assert_eq!(
        timeline.events[63].kind,
        EchoTimelineKindV1::DispatchReturn { loop_seq: 64, dispatch_seq: 32 },
        "the prefix is kept; the 65th and 66th were dropped"
    );
}

/// A watched child window records its own dispatch boundaries and render pair through the marker its
/// adapter takes from the child's own timeline.
#[test]
fn a_watched_child_records_its_render_pair() {
    let (mut app, main, child, _base, _main_pane) = watched_owners();
    let pane_id = app.windows[&child].tab_states[0].active_pane;
    let token = armed(app.arm_echo_watch(pane_id, target()));
    app.note_dispatch_entry(main);
    assert!(app.render_marker(main).is_none(), "the main window has no owner");
    app.note_dispatch_return(main);
    app.note_dispatch_entry(child);
    let marker =
        app.windows[&child].redraw.timeline.render_marker().expect("watched child dispatch");
    marker.enter();
    marker.exit_at(Instant::now());
    drop(marker);
    app.note_dispatch_return(child);
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(
        kinds(&timeline),
        [
            EchoTimelineKindV1::DispatchEntry { loop_seq: 1, dispatch_seq: 1 },
            EchoTimelineKindV1::RenderEnter { dispatch_seq: 1 },
            EchoTimelineKindV1::RenderExit { dispatch_seq: 1 },
            EchoTimelineKindV1::DispatchReturn { loop_seq: 2, dispatch_seq: 1 },
        ]
    );
}

/// On the production clock every hook's instant, the accepted tick's included, comes from one clock,
/// so the recorded events are in nondecreasing time in record order.
#[test]
fn recorded_events_share_one_clock_in_record_order() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    app.__test_set_dispatch_clock(Instant::now);
    let now = Instant::now();
    let window = app.windows.get_mut(&main).unwrap();
    // A 10 s period keeps the streaming frame waiting for its tick however slow the host is.
    window.redraw.monitor_period = Duration::from_secs(10);
    window.last_render = now;
    window.stream_clock = now;
    install_link(&mut app, main);
    let token = armed(app.arm_echo_watch(pane_id, target()));
    publish_output(&app, main);
    app.mark_window_redraw(main, RedrawCause::Output);
    app.note_dispatch_entry(main);
    assert!(!app.admit_window_redraw(main), "streaming waits for a tick inside the period");
    app.note_dispatch_return(main);
    app.sync_display_links();
    let generation = app.windows[&main].redraw.link_live.expect("the link runs");
    app.handle_display_link_tick(main, generation, Instant::now());
    app.service_output_event(OutputEvent::Pane { window_id: main, pane_id }, Instant::now());
    app.note_dispatch_entry(main);
    assert!(app.admit_window_redraw(main), "the tick's permit admits");
    let marker = app.render_marker(main).expect("watched dispatch");
    marker.enter();
    marker.exit_at(Instant::now());
    drop(marker);
    app.note_dispatch_return(main);
    let timeline = take_timeline(&mut app, pane_id, token);
    assert_eq!(ticks(&timeline).len(), 1);
    assert_eq!(
        decisions(&timeline).last(),
        Some(&AdmissionDecisionV1::Permit { tick_seq: Some(1) })
    );
    let times: Vec<u64> = timeline.events.iter().map(|event| event.at_ns).collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{:?}", timeline.events);
}
