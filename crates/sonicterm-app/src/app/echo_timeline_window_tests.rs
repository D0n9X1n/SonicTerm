//! Echo output, admission and permit order contracts.

use super::super::echo_timeline_tests::*;
use super::*;

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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let first = armed(arm_on_both_clocks(&mut app, pane_id));
    app.note_dispatch_entry(main);
    app.note_dispatch_return(main);
    let second = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let owner_token = armed(arm_on_both_clocks(&mut app, owner_pane));
    service(&mut app, main, owner_pane);
    let other_token = armed(arm_on_both_clocks(&mut app, other_pane));
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
    let stale = armed(arm_on_both_clocks(&mut app, pane_id));
    let current = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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

    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let stale = armed(arm_on_both_clocks(&mut app, pane_id));
    let current = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
        Some(super::super::super::display_link::PacingMode::Timer);
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, hidden_pane));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    for source in line_ending_variants(concat!(
        include_str!("redraw.rs"),
        "\n",
        include_str!("redraw_admission.rs"),
        "\n",
        include_str!("redraw_state.rs"),
        "\n",
        include_str!("redraw_window.rs")
    )) {
        let source = source.replace("\r\n", "\n");
        let start = source.find("if !renderer.device_accepts_gpu_work() {").expect("device branch");
        let branch =
            &source[start..start + source[start..].find("return false;").expect("refusal")];
        let held = branch.find("AdmissionDecisionV1::Held").expect("the refusal is Held");
        let reset = branch.find("invalidate_link_pacing()").expect("then the link resets");
        assert!(held < reset, "{branch}");
    }
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
    let token = armed(arm_on_both_clocks(&mut app, owner_pane));
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
