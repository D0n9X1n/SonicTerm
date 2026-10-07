use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::record::{Delivery, EchoOutcome, LatencySample, SPLIT};

/// The instant `offset_ns` nanoseconds after `base`.
fn instant_at(base: Instant, offset_ns: u64) -> Instant {
    base + Duration::from_nanos(offset_ns)
}

/// One ordered credited sample, every instant in nanoseconds after the arm at `base`:
/// injected 100, parsed 200, published 500, a tick at 650, readiness at 700 (loop 2), the
/// forward 900..1500 holding entry 1000 (loop 5), a `Permit` at 1050, render 1100..1300 and the
/// App's return at 1400. Inside readiness..entry, another pane's service sits at loop 3 and the
/// watched pane's own at loop 4, which M3 does not count.
pub(crate) fn fixture_at(base: Instant) -> (Record, Credited) {
    let event = |at_ns: u64, kind: EventKind| Event { at_ns, kind };
    let record = Record {
        armed_at: base,
        chunk_timestamps: true,
        initial_permit: None,
        initial_permit_unknown: false,
        events: vec![
            event(150, EventKind::ChunkRead),
            event(
                650,
                EventKind::Tick {
                    loop_seq: 1,
                    identity: Some(TickId { tick_seq: 1, generation: 7 }),
                },
            ),
            event(
                700,
                EventKind::Check {
                    loop_seq: 2,
                    generation: 5,
                    outcome: CheckOutcome::NativeRequest,
                },
            ),
            event(1000, EventKind::Entry { loop_seq: 5, dispatch_seq: 1 }),
            event(
                1050,
                EventKind::Admission {
                    loop_seq: 6,
                    dispatch_seq: 1,
                    decision: Decision::Permit { tick_seq: Some(1) },
                },
            ),
            event(1100, EventKind::RenderEnter { dispatch_seq: 1 }),
            event(1300, EventKind::RenderExit { dispatch_seq: 1 }),
            event(1400, EventKind::Return { loop_seq: 7, dispatch_seq: 1 }),
        ],
        overflow: false,
        flood: vec![(2, 3), (3, 99), (4, 3)],
        flood_evicted_through: None,
    };
    let split = Split {
        input_to_parse_ns: 100,
        parse_to_publication_ns: 300,
        publication_to_present_ns: 1000,
        delivery_lag_ns: 10,
        delivery: Delivery::Sent,
        coalesced: false,
        sync_open: false,
        echo_generation: 5,
    };
    let credited = Credited {
        injected: instant_at(base, 100),
        forward_started: instant_at(base, 900),
        ended: instant_at(base, 1500),
        split: Some(split),
        split_reason: SPLIT,
        token: Some(11),
        window: Some(7),
        pane: 3,
    };
    (record, credited)
}

/// The ordered fixture, armed now.
fn fixture() -> (Record, Credited) {
    fixture_at(Instant::now())
}

/// One recorded event at `at_ns` after the arm.
fn timed(at_ns: u64, kind: EventKind) -> Event {
    Event { at_ns, kind }
}

/// Analyze a recorded fixture.
fn analyzed(record: Record, credited: &Credited) -> TimelineSample {
    analyze(&TimelineTake::Recorded(record), credited)
}

/// A malformed recorded execution's disposition: `recorded`, `clock-order` with `reason`, nothing derived,
/// out of every event-dependent population, and still in the credited and recorded denominators.
fn assert_clock_order(sample: &TimelineSample, reason: &str, name: &str) {
    assert_eq!(
        (sample.availability, sample.ordering, sample.ordering_reason),
        (Availability::Recorded, Some(OrderingStatus::ClockOrder), Some(reason)),
        "{name}"
    );
    let derived = (sample.readiness, sample.admission, sample.credited_dispatch_seq, sample.parts);
    assert_eq!(derived, (None, None, None, None), "{name}");
    assert_eq!(
        (sample.permit_identity, sample.tick_qualified, sample.flood_services),
        (None, None, None),
        "{name}"
    );
    assert!(
        !sample.ordered()
            && !sample.in_b2()
            && !sample.in_m4()
            && !sample.in_m3()
            && !sample.read_stamped(),
        "{name}"
    );
    let side = coverage([sample]);
    assert_eq!(side["recorded"], json!({"numerator": 1, "denominator": 1}), "{name}");
    assert_eq!(side["ordered"], json!({"numerator": 0, "denominator": 1}), "{name}");
}

/// The ordered fixture's seven parts sum exactly to the credited latency, the two reported sub-parts
/// sum to the seventh, and the App's return precedes the harness's credited end (a nonzero gap).
#[test]
fn an_ordered_sample_decomposes_exactly_to_the_credited_end() {
    let (record, credited) = fixture();
    let sample = analyzed(record, &credited);
    assert_eq!((sample.ordering, sample.ordering_reason), (Some(OrderingStatus::Ordered), None));
    let parts = sample.parts.expect("ordered");
    assert_eq!(parts.additive_ns, [100, 300, 200, 300, 100, 200, 200]);
    assert_eq!(parts.additive_ns.iter().sum::<u64>(), 1400, "the credited latency in integer ns");
    assert_eq!(
        parts.render_exit_to_dispatch_return_ns + parts.dispatch_return_to_credited_end_ns,
        parts.additive_ns[6]
    );
    assert!(parts.dispatch_return_to_credited_end_ns > 0, "App return precedes the credited end");
    assert_eq!(sample.credited_dispatch_seq, Some(1));
    assert_eq!(sample.admission, Some(Admission::Permit));
    assert_eq!((sample.permit_present_ns, sample.permit_absent_ns), (Some(300), Some(0)));
    assert_eq!(sample.permit_identity, Some(PermitIdentity::Known));
    assert_eq!((sample.tick_qualified, sample.tick_to_entry_ns), (Some(true), Some(350)));
    assert_eq!((sample.flood_services, sample.m3_complete), (Some(1), Some(true)));
    assert_eq!(sample.latest_read_to_parse_ns, Some(50));
    assert_eq!(sample.ready_before_publication, Some(false));
    let readiness = sample.readiness.expect("readiness");
    assert_eq!(
        (readiness.outcome, readiness.loop_seq, readiness.site),
        (CheckOutcome::NativeRequest, 2, Some(Site::OutputService))
    );
    assert!(sample.in_b2() && sample.in_m4() && sample.in_m3() && sample.read_stamped());
    let value = sample.to_json();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["parts_ns"]["render_exit_to_credited_end"], 200);
    assert_eq!(value["readiness"]["site"], "output_service");
    assert_eq!((value["token"].clone(), value["window"].clone()), (json!(11), json!(7)));
}

/// Every ordering status from injected instants: publication inside the dispatch, readiness after
/// entry (valid context, `split_only`, never `clock-order`), readiness before publication, a missing
/// readiness check, and existing split reasons.
#[test]
fn every_ordering_status_from_injected_instants() {
    let (record, mut credited) = fixture();
    let mut split = credited.split.unwrap();
    split.parse_to_publication_ns = 1000;
    credited.split = Some(split);
    let sample = analyzed(record, &credited);
    assert_eq!(sample.ordering, Some(OrderingStatus::PublishedDuringDispatch));
    assert_eq!(sample.parts, None);

    let (mut record, credited) = fixture();
    // Readiness is first seen by the credited dispatch's own admission check, after its entry.
    record.events[2].kind =
        EventKind::Check { loop_seq: 2, generation: 4, outcome: CheckOutcome::None };
    let admission_check =
        EventKind::Check { loop_seq: 6, generation: 5, outcome: CheckOutcome::None };
    record.events.insert(4, timed(1020, admission_check));
    let sample = analyzed(record, &credited);
    assert_eq!(
        (sample.ordering, sample.ordering_reason),
        (Some(OrderingStatus::SplitOnly), Some("readiness-after-entry"))
    );
    assert_eq!(sample.readiness.and_then(|readiness| readiness.site), Some(Site::Admission));
    assert_eq!(
        (sample.flood_services, sample.permit_present_ns),
        (None, None),
        "neither B2 nor M3"
    );

    let (mut record, credited) = fixture();
    // The tick that precedes the early check is moved too, so event-loop time stays in order.
    record.events[1].at_ns = 400;
    record.events[2].at_ns = 450;
    let sample = analyzed(record, &credited);
    assert_eq!(sample.ordering_reason, Some("ready-before-publication"));
    assert_eq!(sample.ready_before_publication, Some(true));

    let (mut record, credited) = fixture();
    record.events[2].kind =
        EventKind::Check { loop_seq: 2, generation: 4, outcome: CheckOutcome::None };
    let sample = analyzed(record, &credited);
    assert_eq!(sample.ordering_reason, Some("missing-event:output-check"));
    assert_eq!(sample.readiness, None);

    for (reason, status) in
        [("pane-not-shown", OrderingStatus::SplitOnly), ("clock-order", OrderingStatus::ClockOrder)]
    {
        let (record, mut credited) = fixture();
        credited.split = None;
        credited.split_reason = reason;
        let sample = analyzed(record, &credited);
        assert_eq!((sample.ordering, sample.ordering_reason), (Some(status), Some(reason)));
        assert_eq!(sample.parts, None);
    }
}

/// The credited pair is bound uniquely: no complete pair inside the forward, or a pair with no render,
/// is `no-credited-pair`; two pairs inside one forward, at equal or distinct timestamps, are
/// `ambiguous-credited-pair`, and neither the first nor the last is chosen.
#[test]
fn the_credited_pair_is_bound_uniquely() {
    let (record, mut credited) = fixture();
    credited.forward_started = instant_at(record.armed_at, 1050);
    let sample = analyzed(record, &credited);
    assert_eq!(sample.ordering_reason, Some("no-credited-pair"));
    assert_eq!(sample.credited_dispatch_seq, None);

    let (mut record, credited) = fixture();
    record.events.retain(|event| {
        !matches!(event.kind, EventKind::RenderEnter { .. } | EventKind::RenderExit { .. })
    });
    assert_eq!(analyzed(record, &credited).ordering_reason, Some("no-credited-pair"));

    // A second complete pair after the first, inside the same forward: all at the first's return, or later.
    for offsets in [[1400; 5], [1410, 1420, 1430, 1440, 1450]] {
        let (mut record, credited) = fixture();
        let [entry, admitted, enter, exit, returned] = offsets;
        let decision = Decision::Admitted;
        record.events.extend([
            timed(entry, EventKind::Entry { loop_seq: 8, dispatch_seq: 2 }),
            timed(admitted, EventKind::Admission { loop_seq: 9, dispatch_seq: 2, decision }),
            timed(enter, EventKind::RenderEnter { dispatch_seq: 2 }),
            timed(exit, EventKind::RenderExit { dispatch_seq: 2 }),
            timed(returned, EventKind::Return { loop_seq: 10, dispatch_seq: 2 }),
        ]);
        let sample = analyzed(record, &credited);
        assert_eq!(sample.ordering_reason, Some("ambiguous-credited-pair"), "{offsets:?}");
        assert_eq!((sample.credited_dispatch_seq, sample.parts), (None, None), "{offsets:?}");
    }
}

/// A fallback admission with an otherwise complete order stays ordered and is excluded from B2.
#[test]
fn an_ordered_fallback_stays_ordered_and_leaves_b2() {
    let (mut record, credited) = fixture();
    record.events.retain(|event| !matches!(event.kind, EventKind::Tick { .. }));
    record.events[3].kind =
        EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision: Decision::Fallback };
    let sample = analyzed(record, &credited);
    assert!(sample.ordered());
    assert_eq!(sample.admission, Some(Admission::Fallback));
    assert_eq!((sample.permit_present_ns, sample.permit_absent_ns), (None, None));
    assert!(!sample.in_b2());
    assert_eq!(
        (sample.permit_identity, sample.tick_qualified),
        (Some(PermitIdentity::None), Some(false))
    );
}

/// An unknown tick identity keeps the known permit occupancy and makes `tick_to_entry_ns` null.
#[test]
fn an_unknown_identity_keeps_the_known_occupancy() {
    let (mut record, credited) = fixture();
    record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: None };
    record.events[4].kind = EventKind::Admission {
        loop_seq: 6,
        dispatch_seq: 1,
        decision: Decision::Permit { tick_seq: None },
    };
    let sample = analyzed(record, &credited);
    assert_eq!((sample.permit_present_ns, sample.permit_absent_ns), (Some(300), Some(0)));
    assert_eq!(sample.permit_identity, Some(PermitIdentity::Unknown));
    assert_eq!((sample.tick_qualified, sample.tick_to_entry_ns), (Some(false), None));
    assert_eq!(sample.to_json()["tick_to_entry_ns"], json!(null));
    assert!(sample.ordered() && !sample.in_m4());
}

/// A missing chunk read is null, never 0: stamping on without a retained read is
/// `missing-or-unrepresentable`, stamping off is `not-configured`.
#[test]
fn missing_read_data_is_null_never_zero() {
    let (mut record, credited) = fixture();
    record.events.retain(|event| event.kind != EventKind::ChunkRead);
    let sample = analyzed(record.clone(), &credited);
    assert_eq!(sample.read_stamp, Some(ReadStamp::MissingOrUnrepresentable));
    assert_eq!(sample.latest_read_to_parse_ns, None);
    assert_eq!(sample.to_json()["latest_read_to_parse_ns"], json!(null));
    assert!(!sample.read_stamped());
    record.chunk_timestamps = false;
    assert_eq!(analyzed(record, &credited).to_json()["read_stamp"], "not-configured");
}

/// cfg-off and every V1 `NotRecorded` are `unavailable` with their named reasons; diagnostic
/// classifications are null, and identities are kept only when the harness knows them independently.
#[test]
fn unavailable_records_keep_only_independent_identities() {
    let (_record, mut credited) = fixture();
    credited.token = None;
    let cfg_off = analyze(&TimelineTake::Unavailable("cfg-off"), &credited).to_json();
    assert_eq!(cfg_off["availability"], "unavailable");
    assert_eq!(cfg_off["unavailable_reason"], "cfg-off");
    for field in [
        "ordering",
        "read_stamp",
        "readiness",
        "admission",
        "permit_identity",
        "token",
        "credited_dispatch_seq",
        "overflow",
    ] {
        assert_eq!(cfg_off[field], json!(null), "{field}");
    }
    assert_eq!((cfg_off["pane"].clone(), cfg_off["window"].clone()), (json!(3), json!(7)));
    let (_record, credited) = fixture();
    let not_recorded = analyze(&TimelineTake::Unavailable("not-recorded"), &credited).to_json();
    assert_eq!(not_recorded["unavailable_reason"], "not-recorded");
    assert_eq!(not_recorded["token"], 11, "the armed token is known to the harness");
    assert_eq!(not_recorded["ordering"], json!(null));
    assert_eq!(unarmed_timeline(ArmState::Unsupported), Some(TimelineTake::Unavailable("cfg-off")));
    assert_eq!(
        unarmed_timeline(ArmState::Failed("arm-gate-off")),
        Some(TimelineTake::Unavailable("gate-off"))
    );
    assert_eq!(unarmed_timeline(ArmState::OutOfScope), None);
    let (mut record, credited) = fixture();
    record.initial_permit_unknown = true;
    record.initial_permit = Some(HeldAtArm {
        identity: TickId { tick_seq: 1, generation: 7 },
        delivered_at: record.armed_at,
    });
    let rejected = analyzed(record, &credited).to_json();
    assert_eq!(
        (rejected["availability"].clone(), rejected["rejection_reason"].clone()),
        (json!("rejected"), json!("malformed-initial-permit"))
    );
}

/// Schema fixtures: a known consumed tick on a non-ordered sample is outside M4; an overflowed
/// sample keeps its credited latency, stays in the credited and recorded populations, and leaves
/// every event-dependent population.
#[test]
fn schema_fixtures_keep_populations_exact() {
    let (record, mut credited) = fixture();
    credited.split = None;
    credited.split_reason = "pane-not-shown";
    let unordered = analyzed(record, &credited);
    assert_eq!(unordered.tick_qualified, Some(true));
    assert!(!unordered.ordered() && !unordered.in_m4());

    let (mut record, credited) = fixture();
    record.overflow = true;
    let overflowed = analyzed(record, &credited);
    assert_eq!(overflowed.availability, Availability::Recorded);
    assert_eq!(overflowed.ordering_reason, Some("overflow"));
    assert_eq!(
        (overflowed.tick_qualified, overflowed.readiness, overflowed.flood_services),
        (None, None, None)
    );
    assert!(
        !overflowed.ordered()
            && !overflowed.in_b2()
            && !overflowed.in_m3()
            && !overflowed.read_stamped()
    );
    let coverage = coverage([&overflowed]);
    assert_eq!(coverage["recorded"], json!({"numerator": 1, "denominator": 1}));
    assert_eq!(coverage["ordered"], json!({"numerator": 0, "denominator": 1}));
    let sample = LatencySample::credited(
        1.0,
        credited.injected,
        credited.ended,
        &EchoOutcome::Unsupported,
        false,
    )
    .with_timeline(Some(overflowed));
    let value = serde_json::to_value(sample).expect("a sample converts to JSON");
    assert_eq!(value["latency_ms"], 0.0014, "the original credited latency");
    assert_eq!(value["echo_timeline"]["overflow"], true);
}

/// `readiness.site` comes from the validated, complete intervals alone: inside exactly one watched
/// interval is `admission`, outside every one is `output_service`, and an incomplete interval leaves it
/// null. A record left open is rejected before any site is derived, and an overflowed record has no
/// readiness at all.
#[test]
fn the_readiness_site_is_derived_or_null() {
    let base = Instant::now();
    let interval = |entry_loop: u32, return_loop: u32| Dispatch {
        entry_loop,
        entry: base,
        returned: Some((return_loop, base)),
        enter: None,
        exit: None,
        decision: None,
        admitted_at: None,
    };
    let dispatches = BTreeMap::from([(1, interval(4, 6))]);
    assert_eq!(site_of(&dispatches, 5), Some(Site::Admission));
    assert_eq!(site_of(&dispatches, 2), Some(Site::OutputService));
    let mut open = interval(7, 9);
    open.returned = None;
    let incomplete = BTreeMap::from([(1, interval(4, 6)), (2, open)]);
    assert_eq!(site_of(&incomplete, 8), None, "an incomplete interval leaves the site undecided");
    let (mut record, credited) = fixture();
    record.events.push(timed(1450, EventKind::Entry { loop_seq: 8, dispatch_seq: 2 }));
    let left_open = analyzed(record, &credited);
    assert_eq!(
        (left_open.availability, left_open.ordering_reason, left_open.readiness),
        (Availability::Recorded, Some("dispatch-pair"), None)
    );
    let (mut record, credited) = fixture();
    record.overflow = true;
    assert_eq!(analyzed(record, &credited).to_json()["readiness"], json!(null));
}

/// Coverage names every numerator and denominator; an unavailable side is `unavailable` with null
/// populations, never a measured zero, and no in-scope sample gives null.
#[test]
fn coverage_reports_named_fractions_and_unavailability() {
    let (record, credited) = fixture();
    let ordered = analyzed(record, &credited);
    let (mut record, credited) = fixture();
    record.events[4].kind =
        EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision: Decision::Fallback };
    let fallback = analyzed(record, &credited);
    let available = coverage([&ordered, &fallback]);
    assert_eq!(available["availability"], "available");
    assert_eq!(available["ordered"], json!({"numerator": 2, "denominator": 2}));
    assert_eq!(available["b2"], json!({"numerator": 1, "denominator": 2}));
    assert_eq!(available["m4"], json!({"numerator": 1, "denominator": 2}));
    assert_eq!(available["read_stamped"], json!({"numerator": 2, "denominator": 2}));
    assert_eq!(available["b2_of_credited"], json!({"numerator": 1, "denominator": 2}));
    // Unordered, overflowed and unavailable credited samples make the two denominators differ.
    let (record, mut unordered_context) = fixture();
    unordered_context.split = None;
    unordered_context.split_reason = "pane-not-shown";
    let unordered = analyzed(record, &unordered_context);
    let (mut record, credited) = fixture();
    record.overflow = true;
    let overflowed = analyzed(record, &credited);
    let not_recorded = analyze(&TimelineTake::Unavailable("not-recorded"), &credited);
    let mixed = coverage([&ordered, &fallback, &unordered, &overflowed, &not_recorded]);
    assert_eq!(mixed["recorded"], json!({"numerator": 4, "denominator": 5}));
    assert_eq!(mixed["ordered"], json!({"numerator": 2, "denominator": 5}));
    assert_eq!(mixed["b2"], json!({"numerator": 1, "denominator": 2}));
    assert_eq!(mixed["b2_of_credited"], json!({"numerator": 1, "denominator": 5}));
    assert_eq!(mixed["m3"], json!({"numerator": 2, "denominator": 2}));
    assert_eq!(mixed["m3_of_credited"], json!({"numerator": 2, "denominator": 5}));
    let unavailable = analyze(&TimelineTake::Unavailable("not-recorded"), &credited);
    let side = coverage([&unavailable]);
    assert_eq!(side["availability"], "unavailable");
    assert_eq!(side["recorded"], json!({"numerator": 0, "denominator": 1}));
    assert_eq!(side["ordered"], json!(null), "never a measured zero");
    assert_eq!(
        (side["b2_of_credited"].clone(), side["m3_of_credited"].clone()),
        (json!(null), json!(null))
    );
    assert_eq!(coverage(std::iter::empty()), json!(null));
}

/// The adapter is compiled exactly when its cfg is on, and both the take and the discard path drain
/// the timeline after taking the watch.
#[test]
fn every_take_and_discard_drains_the_timeline() {
    assert_eq!(API_ENABLED, cfg!(all(feature = "perf-echo-trace", perf_echo_timeline_api)));
    let source = include_str!("record.rs").replace("\r\n", "\n");
    let take = &source[source.find("pub(crate) fn take(\n        app: &mut App,").expect("take")..];
    let take = &take[..take.find("\n    }\n").expect("take end")];
    assert!(take.contains("crate::timeline::adapter::drain(app, pane, token, armed_at)"), "{take}");
    let discard = &source[source.find("pub(crate) fn discard(app: &mut App").expect("discard")..];
    let discard = &discard[..discard.find("\n    }\n").expect("discard end")];
    let watch = discard.find("app.take_echo_watch(pane, token)");
    let drained = discard.find("crate::timeline::adapter::drain(app, pane, token, None)");
    // The discard drains after it takes the watch; a missing drain fails this assertion.
    assert!(
        watch.zip(drained).is_some_and(|(watch, drained)| watch < drained),
        "the discard takes the watch, then drains: {discard}"
    );
}

/// Against a real App: a discarded sample's timeline is drained, so a later take finds it already
/// transferred; a credited take transfers the record with its arm instant.
#[cfg(all(feature = "perf-echo-trace", perf_echo_timeline_api))]
#[test]
fn a_real_app_take_and_discard_both_transfer_the_timeline() {
    use sonicterm_app::app::{App, EchoTimelineTakeV1};

    use crate::record::{echo_api, echo_target, RowIdentity};

    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.force_frame_counters_on().expect("no window exists yet");
    let pane = app.__test_seed_counting_tab("s2");
    let target = echo_target((0, 6), 80, 0);
    let ArmState::Armed(token) = echo_api::arm(&mut app, pane, &target, RowIdentity::default())
    else {
        panic!("the pane arms");
    };
    echo_api::discard(&mut app, pane, token);
    assert_eq!(app.take_echo_timeline_v1(pane, token), EchoTimelineTakeV1::AlreadyTaken);
    let ArmState::Armed(token) = echo_api::arm(&mut app, pane, &target, RowIdentity::default())
    else {
        panic!("the pane rearms");
    };
    let (outcome, take) = echo_api::take(&mut app, pane, token, None);
    assert!(matches!(outcome, EchoOutcome::Taken(_)));
    assert!(matches!(take, TimelineTake::Recorded(_)), "{take:?}");
    assert_eq!(app.take_echo_timeline_v1(pane, token), EchoTimelineTakeV1::AlreadyTaken);
}

/// A malformed recorded execution is found before any population is derived, so no early named
/// classification can mask it, and keeps the frozen disposition: `recorded`, `clock-order` with the named
/// reason, nothing derived, out of every event-dependent population but in credited and recorded. The cases
/// are an orphan render, a return looped before its entry, readiness sharing the entry's loop sequence, a
/// repeated flood sequence, a second admission for the credited dispatch, a duplicated render and a
/// dispatch left open.
#[test]
fn malformed_recorded_evidence_is_clock_order_before_any_population() {
    let cases: [(&str, fn(&mut Record), &str); 7] = [
        (
            "orphan render",
            |record| {
                record.events.insert(3, timed(800, EventKind::RenderEnter { dispatch_seq: 9 }));
            },
            "render-pair",
        ),
        (
            "return looped before its entry",
            |record| {
                record.events[7].kind = EventKind::Return { loop_seq: 4, dispatch_seq: 1 };
            },
            "loop-sequence",
        ),
        (
            "readiness shares the entry's loop",
            |record| {
                let outcome = CheckOutcome::NativeRequest;
                record.events[2].kind = EventKind::Check { loop_seq: 5, generation: 5, outcome };
            },
            "loop-sequence",
        ),
        (
            "repeated flood sequence",
            |record| {
                record.flood = vec![(2, 3), (3, 99), (3, 99), (4, 3)];
            },
            "flood-sequence",
        ),
        (
            "second admission",
            |record| {
                let decision = Decision::Admitted;
                let second = EventKind::Admission { loop_seq: 7, dispatch_seq: 1, decision };
                record.events.insert(5, timed(1060, second));
                record.events[8].kind = EventKind::Return { loop_seq: 8, dispatch_seq: 1 };
            },
            "admission-count",
        ),
        (
            "duplicated render",
            |record| {
                record.events.insert(6, timed(1150, EventKind::RenderEnter { dispatch_seq: 1 }));
            },
            "render-pair",
        ),
        (
            "dispatch left open",
            |record| {
                record.events.push(timed(1450, EventKind::Entry { loop_seq: 8, dispatch_seq: 2 }));
            },
            "dispatch-pair",
        ),
    ];
    for (name, corrupt, reason) in cases {
        let (mut record, credited) = fixture();
        corrupt(&mut record);
        assert_clock_order(&analyzed(record, &credited), reason, name);
    }
}

/// The permit is reconstructed in record order as absent, held-unknown or held-known, and every clear
/// and consumption must name the permit actually held. Consuming a cleared tick after its replacement,
/// clearing with nothing held, naming another tick, consuming with none held, consuming a known permit as
/// unknown, or a transition back in time is rejected; consuming the replacement is valid and its timing
/// comes from the replacement's own delivery; an unknown permit held at arm is a legitimate unknown.
#[test]
fn permit_transitions_must_name_the_permit_held() {
    let replaced = |consumed_tick: u32| {
        let (mut record, credited) = fixture();
        let first = Some(TickId { tick_seq: 1, generation: 7 });
        let second = Some(TickId { tick_seq: 2, generation: 8 });
        record.events.splice(
            1..2,
            [
                timed(600, EventKind::Tick { loop_seq: 1, identity: first }),
                timed(620, EventKind::Cleared { loop_seq: 2, tick_seq: Some(1) }),
                timed(650, EventKind::Tick { loop_seq: 3, identity: second }),
            ],
        );
        let outcome = CheckOutcome::NativeRequest;
        record.events[4].kind = EventKind::Check { loop_seq: 4, generation: 5, outcome };
        let decision = Decision::Permit { tick_seq: Some(consumed_tick) };
        record.events[6].kind = EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision };
        record.flood = vec![(4, 99)];
        analyzed(record, &credited)
    };
    let stale = replaced(1);
    assert_clock_order(&stale, "permit-transition", "the cleared tick consumed");
    let fresh = replaced(2);
    assert!(fresh.ordered());
    assert_eq!(fresh.permit_identity, Some(PermitIdentity::Known));
    assert_eq!(fresh.tick_to_entry_ns, Some(350), "from the replacement's own delivery");
    assert_eq!((fresh.permit_present_ns, fresh.permit_absent_ns), (Some(300), Some(0)));

    let impossible: [(&str, fn(&mut Record), &str); 5] = [
        (
            "a clear with nothing held",
            |record| {
                record
                    .events
                    .insert(1, timed(600, EventKind::Cleared { loop_seq: 1, tick_seq: None }));
            },
            "permit-transition",
        ),
        (
            "a clear naming another tick",
            |record| {
                record
                    .events
                    .insert(2, timed(660, EventKind::Cleared { loop_seq: 2, tick_seq: Some(9) }));
            },
            "permit-transition",
        ),
        (
            "a consumption with none held",
            |record| {
                record.events.remove(1);
            },
            "permit-transition",
        ),
        (
            "a known permit consumed as unknown",
            |record| {
                let decision = Decision::Permit { tick_seq: None };
                record.events[4].kind =
                    EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision };
            },
            "permit-transition",
        ),
        (
            "a transition back in time",
            |record| {
                record.events[1].at_ns = 1100;
            },
            "loop-time-order",
        ),
    ];
    for (name, corrupt, reason) in impossible {
        let (mut record, credited) = fixture();
        corrupt(&mut record);
        assert_clock_order(&analyzed(record, &credited), reason, name);
    }

    let (mut record, credited) = fixture();
    record.initial_permit_unknown = true;
    record.events.remove(1);
    let decision = Decision::Permit { tick_seq: None };
    record.events[3].kind = EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision };
    let unknown = analyzed(record, &credited);
    assert!(unknown.ordered());
    assert_eq!(
        (unknown.permit_identity, unknown.tick_qualified),
        (Some(PermitIdentity::Unknown), Some(false))
    );
    assert_eq!((unknown.permit_present_ns, unknown.permit_absent_ns), (Some(300), Some(0)));
}

/// Event-loop events and render boundaries never go back in time; equal times are allowed and the
/// historical chunk read is exempt. An admission after its render and return is `admission-order`, with or
/// without a render pair; a tick delivered after the readiness check it precedes is `loop-time-order`. A
/// chunk read appended after later event-loop events, and equal timestamps, stay ordered.
#[test]
fn event_loop_events_keep_time_order() {
    let (mut record, credited) = fixture();
    record.events[4].at_ns = 1450;
    assert_clock_order(
        &analyzed(record, &credited),
        "admission-order",
        "admission after the render",
    );
    let (mut record, credited) = fixture();
    record.events[4].at_ns = 1450;
    record.events.retain(|event| {
        !matches!(event.kind, EventKind::RenderEnter { .. } | EventKind::RenderExit { .. })
    });
    assert_clock_order(
        &analyzed(record, &credited),
        "admission-order",
        "admission after the return",
    );
    // An admission inside the render, after its enter and before its return, is the admission's own fault.
    let (mut record, credited) = fixture();
    record.events[4].at_ns = 1150;
    assert_clock_order(
        &analyzed(record, &credited),
        "admission-order",
        "admission inside the render",
    );
    let (mut record, credited) = fixture();
    record.events[1].at_ns = 800;
    assert_clock_order(
        &analyzed(record, &credited),
        "loop-time-order",
        "tick after the readiness check",
    );

    let (mut record, credited) = fixture();
    let read = record.events.remove(0);
    record.events.push(read);
    let late_read = analyzed(record, &credited);
    assert!(late_read.ordered(), "the historical read appended last is exempt");
    assert_eq!(
        (late_read.read_stamp, late_read.latest_read_to_parse_ns),
        (Some(ReadStamp::Stamped), Some(50))
    );
    let (mut record, credited) = fixture();
    record.events[4].at_ns = 1000;
    record.events[5].at_ns = 1000;
    assert!(analyzed(record, &credited).ordered(), "equal event-loop timestamps are in order");
}

/// The fixture with a second accepted tick `second` at 680 ns, after the first, and its admission
/// consuming `consumed`.
fn two_ticks(second: Option<TickId>, consumed: Option<u32>) -> (Record, Credited) {
    let (mut record, credited) = fixture();
    record.events.insert(2, timed(680, EventKind::Tick { loop_seq: 2, identity: second }));
    let outcome = CheckOutcome::NativeRequest;
    record.events[3].kind = EventKind::Check { loop_seq: 3, generation: 5, outcome };
    let decision = Decision::Permit { tick_seq: consumed };
    record.events[5].kind = EventKind::Admission { loop_seq: 6, dispatch_seq: 1, decision };
    record.flood = vec![(3, 3), (4, 99)];
    (record, credited)
}

/// Permit identities follow the producer: a permit held at arm was delivered at or before it; a known
/// identity is nonzero; known accepted ticks strictly increase their sequence and never decrease their
/// generation, from the permit held at arm on; and no known tick follows an unknown one. Legitimate unknown
/// identities stay unknown, never absent.
#[test]
fn permit_identities_follow_the_producer() {
    let known = |tick_seq: u32, generation: u32| Some(TickId { tick_seq, generation });
    let (mut record, credited) = fixture();
    let identity = TickId { tick_seq: 1, generation: 7 };
    record.initial_permit =
        Some(HeldAtArm { identity, delivered_at: instant_at(record.armed_at, 1200) });
    record.events.remove(1);
    assert_clock_order(
        &analyzed(record, &credited),
        "initial-permit",
        "an initial delivery after arm",
    );
    let (mut record, credited) = fixture();
    let zero = TickId { tick_seq: 0, generation: 7 };
    record.initial_permit = Some(HeldAtArm { identity: zero, delivered_at: record.armed_at });
    record.events.remove(1);
    assert_clock_order(
        &analyzed(record, &credited),
        "tick-identity",
        "a zero identity held at arm",
    );
    let negatives = [
        ("a repeated tick sequence", two_ticks(known(1, 7), Some(1)), "tick-sequence"),
        ("a zero tick sequence", two_ticks(known(0, 7), Some(0)), "tick-identity"),
        (
            "a decreasing tick sequence",
            {
                let (mut record, credited) = two_ticks(known(1, 7), Some(1));
                record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: known(2, 7) };
                (record, credited)
            },
            "tick-sequence",
        ),
        ("a decreasing generation", two_ticks(known(2, 6), Some(2)), "tick-sequence"),
        (
            "a known tick after an unknown one",
            {
                let (mut record, credited) = two_ticks(known(2, 8), Some(2));
                record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: None };
                (record, credited)
            },
            "tick-sequence",
        ),
        (
            "a zero generation",
            {
                let (mut record, credited) = fixture();
                record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: known(1, 0) };
                (record, credited)
            },
            "tick-identity",
        ),
    ];
    for (name, (record, credited), reason) in negatives {
        assert_clock_order(&analyzed(record, &credited), reason, name);
    }
    let (mut record, credited) = fixture();
    record.initial_permit = Some(HeldAtArm { identity, delivered_at: record.armed_at });
    record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: known(1, 7) };
    assert_clock_order(
        &analyzed(record, &credited),
        "tick-sequence",
        "a tick repeating the permit held at arm",
    );

    let replaced = analyzed_pair(two_ticks(known(2, 7), Some(2)));
    assert!(replaced.ordered(), "a replacement in the same generation");
    assert_eq!(replaced.permit_identity, Some(PermitIdentity::Known));
    let after_known = analyzed_pair(two_ticks(None, None));
    assert!(after_known.ordered(), "an unknown tick after a known one");
    assert_eq!(
        (after_known.permit_identity, after_known.tick_qualified),
        (Some(PermitIdentity::Unknown), Some(false))
    );
    let (mut record, credited) = two_ticks(None, None);
    record.events[1].kind = EventKind::Tick { loop_seq: 1, identity: None };
    let both_unknown = analyzed(record, &credited);
    assert!(both_unknown.ordered(), "two unknown ticks");
    assert_eq!(
        both_unknown.permit_present_ns,
        Some(300),
        "an unknown permit is held, never absent"
    );
}

/// Analyze a record and its context.
fn analyzed_pair((record, credited): (Record, Credited)) -> TimelineSample {
    analyzed(record, &credited)
}
