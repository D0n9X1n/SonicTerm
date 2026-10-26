use super::*;
pub(super) use crate::app::{
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
pub(super) use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// A target at row 0, column 2, for `a`, under the default identity.
pub(super) fn target() -> EchoWatchTarget {
    EchoWatchTarget { abs_row: 0, col: 2, character: 'a', identity: EchoRowIdentity::default() }
}

/// Give window `id`'s active pane counter handles, as a gate-on spawn would, and return its id.
pub(super) fn watch_active_pane(app: &mut App, id: WindowId) -> u64 {
    let counters = app.pane_frame_counters();
    let window = app.windows.get_mut(&id).expect("live window");
    let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
    window.panes.get_mut(&pane_id).expect("active pane").frame_counters = counters;
    pane_id
}

/// Counting main and child owners on the fake dispatch clock, each active pane watchable.
pub(super) fn watched_owners() -> (App, WindowId, WindowId, Instant, u64) {
    let base = test_base();
    let (mut app, main, child) = paced_owners(base);
    watch_active_pane(&mut app, child);
    let pane_id = watch_active_pane(&mut app, main);
    (app, main, child, base, pane_id)
}

/// Arm `pane_id` at the earlier of the real clock and the fake dispatch clock. The recorder's own
/// hooks read the real clock and the tick handler the fake one, so every event of either clock that a
/// test makes after this call is at or after the arm, however long the test process has run. This
/// assumes the fake clock is nondecreasing after the call, as every caller sets it.
///
/// It is a lower bound, not one chronology: the tests that deliver fake-clock ticks among real-clock
/// recorder events (the initial permit, unrepresentable permit, replacement tick, discard, link reset,
/// link pause, held/deferred and tick-sequence tests) prove identities and states, not temporal order;
/// `recorded_events_share_one_clock_in_record_order` is the chronology test.
pub(super) fn arm_on_both_clocks(app: &mut App, pane_id: u64) -> ArmOutcome {
    let armed_at = Instant::now().min(fake_now());
    app.arm_echo_watch_at(pane_id, target(), armed_at)
}

/// The token an arm must have issued.
pub(super) fn armed(outcome: ArmOutcome) -> ArmToken {
    match outcome {
        ArmOutcome::Armed(token) => token,
        other => panic!("arm returned {other:?}"),
    }
}

/// `pane_id`'s watch, as a worker clone would hold it.
pub(super) fn watch_of(app: &App, pane_id: u64) -> Arc<EchoWatch> {
    let pane = app.find_pane(pane_id).expect("live pane");
    Arc::clone(&pane.frame_counters.as_ref().expect("counting pane").echo)
}

/// Take the watch, then the timeline; both must succeed.
pub(super) fn take_timeline(app: &mut App, pane_id: u64, token: ArmToken) -> EchoTimelineV1 {
    assert!(matches!(app.take_echo_watch(pane_id, token), TakeOutcome::Trace(_)));
    match app.take_echo_timeline_v1(pane_id, token) {
        EchoTimelineTakeV1::Timeline(timeline) => timeline,
        other => panic!("timeline take returned {other:?}"),
    }
}

/// The recorded event kinds, in record order.
pub(super) fn kinds(timeline: &EchoTimelineV1) -> Vec<EchoTimelineKindV1> {
    timeline.events.iter().map(|event| event.kind).collect()
}

/// The recorded admission decisions, in record order.
pub(super) fn decisions(timeline: &EchoTimelineV1) -> Vec<AdmissionDecisionV1> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::Admission { decision, .. } => Some(decision),
            _ => None,
        })
        .collect()
}

/// The recorded permit clears as (tick_seq, cause), in record order.
pub(super) fn clears(timeline: &EchoTimelineV1) -> Vec<(Option<u32>, PermitClearCauseV1)> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::PermitCleared { tick_seq, cause, .. } => Some((tick_seq, cause)),
            _ => None,
        })
        .collect()
}

/// The recorded output checks as (generation, outcome), in record order.
pub(super) fn checks(timeline: &EchoTimelineV1) -> Vec<(u64, OutputCheckOutcomeV1)> {
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
pub(super) fn ticks(timeline: &EchoTimelineV1) -> Vec<Option<TickIdentityV1>> {
    kinds(timeline)
        .into_iter()
        .filter_map(|kind| match kind {
            EchoTimelineKindV1::Tick { identity, .. } => Some(identity),
            _ => None,
        })
        .collect()
}

/// One watched `RedrawRequested` dispatch around the production admission at `at`.
pub(super) fn redraw_at(app: &mut App, id: WindowId, at: Instant) -> bool {
    set_fake_now(at);
    app.note_dispatch_entry(id);
    let admitted = app.admit_window_redraw(id);
    app.note_dispatch_return(id);
    admitted
}

/// Service one `PaneOutput` event for `pane_id` in window `id`.
pub(super) fn service(app: &mut App, id: WindowId, pane_id: u64) {
    let now = fake_now();
    app.service_output_event(OutputEvent::Pane { window_id: id, pane_id }, now);
}

/// Install a fake display link on window `id`, so link pacing runs headlessly.
pub(super) fn install_link(app: &mut App, id: WindowId) {
    let log = std::rc::Rc::new(std::cell::RefCell::new(
        super::super::display_link::FakeLinkLog::default(),
    ));
    app.windows.get_mut(&id).expect("live window").display_link.source =
        Some(Box::new(super::super::display_link::FakeLink(log)));
}

/// Leave a streaming `Link` admission pending at 1 ms and start the link, as the event loop does.
pub(super) fn pend_link(app: &mut App, id: WindowId, base: Instant) {
    publish_output(app, id);
    app.mark_window_redraw(id, RedrawCause::Output);
    assert!(!redraw_at(app, id, at_ms(base, 1)), "a streaming frame waits for a tick");
    app.sync_display_links();
}

/// Deliver a tick of the window's live generation at `at`, as the native target does.
pub(super) fn fire_at(app: &mut App, id: WindowId, at: Instant) {
    set_fake_now(at);
    let generation = app.windows[&id].redraw.link_live.expect("the link runs");
    app.handle_display_link_tick(id, generation, at);
}

/// The record a slot test builds: a fresh timeline whose transfer is frozen with no flood.
pub(super) fn frozen(mut timeline: Box<SlotTimeline>) -> EchoTimelineV1 {
    timeline.freeze(FrozenFlood::empty());
    timeline.transfer().expect("frozen")
}

/// `elapsed` of `millis` milliseconds.
pub(super) fn millis(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// A source as an LF checkout and as a CRLF one (Windows), so each source pin is shown to read both.
pub(super) fn line_ending_variants(source: &str) -> [String; 2] {
    let lf_text = source.replace("\r\n", "\n");
    let crlf_text = lf_text.replace('\n', "\r\n");
    [lf_text, crlf_text]
}

/// With the gate on and nothing armed, every event-loop hook (tick, admission, service, dispatch)
/// takes no slot lock, reads no recorder clock, allocates nothing, and no window gains an owner.
#[test]
pub(super) fn unarmed_hooks_take_no_slot_lock_clock_read_or_allocation() {
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
pub(super) fn a_watched_dispatch_records_one_render_pair() {
    let (mut app, main, child, _base, pane_id) = watched_owners();
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
/// records the entry before dispatching and the return after its frame bookkeeping. Each source
/// is read as LF on any checkout, so a CRLF (Windows) checkout pins the same anchors.
#[test]
pub(super) fn the_boundaries_sit_at_their_anchors() {
    let adapters = [
        concat!(
            include_str!("window_event.rs"),
            "\n",
            include_str!("window_event_pointer.rs"),
            "\n",
            include_str!("window_event_redraw.rs")
        ),
        include_str!("child_window_redraw.rs"),
    ];
    for source in adapters.into_iter().flat_map(line_ending_variants) {
        let source = source.replace("\r\n", "\n");
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
    for (checkout, source) in ["LF", "CRLF"].into_iter().zip(line_ending_variants(concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    ))) {
        let source = source.replace("\r\n", "\n");
        let handler = &source[source.find("fn window_event(&mut self").expect("outer handler")..];
        let end = handler.find("\n    }\n");
        assert!(end.is_some(), "the outer handler's end is found on a {checkout} checkout");
        let handler = &handler[..end.unwrap_or(handler.len())];
        let entry = handler.find("self.note_dispatch_entry(win_id);").expect("entry");
        let dispatch = handler.find("self.do_window_event(").expect("dispatch");
        let lines = handler.find("self.emit_frame_lines(").expect("frame lines");
        let returned = handler.find("self.note_dispatch_return(win_id);").expect("return");
        assert!(entry < dispatch && dispatch < lines && lines < returned, "{handler}");
    }
}

/// `tick_seq` is the window's accepted-tick count and is not reset at arm: a tick accepted after
/// the arm continues the numbering of a tick accepted before it.
#[test]
pub(super) fn tick_seq_continues_across_an_arm() {
    let (mut app, main, _child, base, pane_id) = watched_owners();
    install_link(&mut app, main);
    pend_link(&mut app, main, base);
    fire_at(&mut app, main, at_ms(base, 16));
    assert!(redraw_at(&mut app, main, at_ms(base, 16)), "the pre-arm tick admits");
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn a_check_keeps_its_observation_instant_across_a_delayed_record() {
    for admission in [false, true] {
        let (mut app, main, _child, _base, pane_id) = watched_owners();
        let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn a_delayed_exit_record_does_not_extend_the_dispatch_interval() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn a_lost_sequence_sets_overflow_behind_a_compressed_check() {
    for qualified in [false, true] {
        let (mut app, main, _child, _base, pane_id) = watched_owners();
        let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn the_sixty_fifth_event_overflows_and_keeps_the_prefix_and_trace() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn a_watched_child_records_its_render_pair() {
    let (mut app, main, child, _base, _main_pane) = watched_owners();
    let pane_id = app.windows[&child].tab_states[0].active_pane;
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
pub(super) fn recorded_events_share_one_clock_in_record_order() {
    let (mut app, main, _child, _base, pane_id) = watched_owners();
    app.__test_set_dispatch_clock(Instant::now);
    let now = Instant::now();
    let window = app.windows.get_mut(&main).unwrap();
    // A 10 s period keeps the streaming frame waiting for its tick however slow the host is.
    window.redraw.monitor_period = Duration::from_secs(10);
    window.last_render = now;
    window.stream_clock = now;
    install_link(&mut app, main);
    let token = armed(arm_on_both_clocks(&mut app, pane_id));
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
