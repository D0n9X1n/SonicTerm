use super::*;

/// The recorded cfg is this build's own: the adapter calls the App exactly when the cfg is set.
#[test]
fn the_recorded_cfg_is_the_builds_own() {
    assert_eq!(API_ENABLED, cfg!(perf_dispatch_timeline_api));
}

/// Every disposition has its own name in `result.json`, matched with no wildcard arm, so a new one fails
/// this test's build and two can never share a name.
#[test]
fn every_disposition_has_its_own_name() {
    let all = [
        Disposition::Unavailable,
        Disposition::NotRecorded,
        Disposition::GateOff,
        Disposition::NoPane,
        Disposition::StillArmed,
        Disposition::Exhausted,
        Disposition::Recorded,
        Disposition::RecordedIncomplete,
        Disposition::Mismatch,
        Disposition::AlreadyTaken,
    ];
    let names: Vec<&str> = all
        .iter()
        .map(|disposition| match disposition {
            Disposition::Unavailable
            | Disposition::NotRecorded
            | Disposition::GateOff
            | Disposition::NoPane
            | Disposition::StillArmed
            | Disposition::Exhausted
            | Disposition::Recorded
            | Disposition::RecordedIncomplete
            | Disposition::Mismatch
            | Disposition::AlreadyTaken => disposition.as_str(),
        })
        .collect();
    assert_eq!(
        names,
        [
            "unavailable",
            "not-recorded",
            "gate-off",
            "no-pane",
            "still-armed",
            "exhausted",
            "recorded",
            "recorded-incomplete",
            "mismatch",
            "already-taken",
        ]
    );
}

/// Without the cfg the adapter never calls the App: arming reports the timeline unavailable, whatever
/// the App's gate, so a comparison reads it as missing, never as an empty sample.
#[cfg(not(perf_dispatch_timeline_api))]
#[test]
fn without_the_cfg_arming_reports_unavailable() {
    use sonicterm_app::app::App;
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    assert_eq!(api::arm(&mut app, 1, None), ArmResult::Unavailable);
    app.force_frame_counters_on().expect("no window exists yet");
    assert_eq!(api::arm(&mut app, 1, Some(witness())), ArmResult::Unavailable);
    let mut storage = ();
    let timeline =
        sample::SampleTimeline::arm(&mut app, 1, Some(OuterKind::AboutToWait), &mut storage);
    assert!(!timeline.is_armed());
    assert_eq!(timeline.close(&mut app, &mut storage), Disposition::Unavailable);
}

/// A witness target in the harness's own terms.
fn witness() -> WitnessTarget {
    WitnessTarget {
        abs_row: 120,
        col: 3,
        character: 'x',
        scrollback_evicted: 9,
        screen_epoch: 2,
        size_generation: 5,
    }
}

/// With the cfg the adapter calls the App's real methods: the prerequisite's stub answers `GateOff` while
/// the frame-counter gate is off and `NotRecorded` once it is on, and the adapter passes each through
/// rather than answering `Unavailable` itself. The sample facade then holds no recorder.
#[cfg(perf_dispatch_timeline_api)]
#[test]
fn with_the_cfg_the_adapter_calls_the_apps_arm() {
    use sonicterm_app::app::App;
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    assert_eq!(api::arm(&mut app, 1, None), ArmResult::GateOff);
    app.force_frame_counters_on().expect("no window exists yet");
    assert_eq!(api::arm(&mut app, 1, Some(witness())), ArmResult::NotRecorded);
    assert_eq!(api::arm(&mut app, u64::MAX, None), ArmResult::NotRecorded);
    let mut storage: sample::Storage = None;
    let timeline =
        sample::SampleTimeline::arm(&mut app, 1, Some(OuterKind::AboutToWait), &mut storage);
    assert!(!timeline.is_armed());
    assert_eq!(timeline.close(&mut app, &mut storage), Disposition::NotRecorded);
    assert!(storage.is_none(), "an unarmed sample allocates no recorder storage");
}

/// The adapter reads every arm and take outcome the App can return as its own variant: none is folded
/// into another, and a timeline passes through whole.
#[cfg(perf_dispatch_timeline_api)]
#[test]
fn with_the_cfg_every_outcome_maps_to_its_own_result() {
    use sonicterm_app::app::{
        DispatchTimelineArmV1, DispatchTimelineTakeV1, DispatchTimelineV1, WitnessResolutionV1,
    };
    assert_eq!(api::arm_result(DispatchTimelineArmV1::NotRecorded), ArmResult::NotRecorded);
    assert_eq!(api::arm_result(DispatchTimelineArmV1::GateOff), ArmResult::GateOff);
    assert_eq!(api::arm_result(DispatchTimelineArmV1::NoPane), ArmResult::NoPane);
    assert_eq!(api::arm_result(DispatchTimelineArmV1::Exhausted), ArmResult::Exhausted);
    let timeline = DispatchTimelineV1 {
        armed_at: std::time::Instant::now(),
        pane_id: 7,
        window: 11,
        events: Vec::new(),
        attempts: Vec::new(),
        transitions: Vec::new(),
        witness: None,
        witness_resolution: WitnessResolutionV1::default(),
        overflow: true,
    };
    assert_eq!(
        api::take_result(DispatchTimelineTakeV1::Timeline(timeline.clone())),
        TakeResult::Timeline(timeline)
    );
    assert_eq!(api::take_result(DispatchTimelineTakeV1::NotRecorded), TakeResult::NotRecorded);
    assert_eq!(api::take_result(DispatchTimelineTakeV1::GateOff), TakeResult::GateOff);
    assert_eq!(api::take_result(DispatchTimelineTakeV1::Mismatch), TakeResult::Mismatch);
    assert_eq!(api::take_result(DispatchTimelineTakeV1::AlreadyTaken), TakeResult::AlreadyTaken);
}

#[cfg(perf_dispatch_timeline_api)]
mod recorder_tests {
    use std::time::{Duration, Instant};

    use super::recorders::{
        offset_ns, Buffers, InvocationV1, OuterCallbackV1, Recorder, INVOCATION_CAPACITY,
        OUTER_CAPACITY,
    };
    use super::{Invocation, InvocationKind, OuterKind};

    /// `offset_ms` milliseconds after `start`.
    fn at(start: Instant, offset_ms: u64) -> Instant {
        start + Duration::from_millis(offset_ms)
    }

    /// An invocation of `kind` with no window, made by the App's own loop.
    fn call(kind: InvocationKind) -> Invocation {
        Invocation { kind, window: 0, synthetic: false }
    }

    /// A recorder armed at `start` on fresh storage, outside any callback.
    fn armed(start: Instant) -> Recorder {
        Recorder::arm(start, None, Buffers::new())
    }

    /// One `about_to_wait` carrying a synthetic window event, a `RunAction` and the `AboutToWait`
    /// forward, in that order: each invocation joins the one outer callback, and every time is relative
    /// to the arm.
    #[test]
    fn one_outer_callback_joins_its_three_invocations() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 1));
        let synthetic = Invocation {
            kind: InvocationKind::WindowEvent { redraw: false },
            window: 9,
            synthetic: true,
        };
        recorder.enter_invocation(synthetic, at(start, 2));
        recorder.exit_invocation(at(start, 3));
        recorder.enter_invocation(call(InvocationKind::RunAction), at(start, 4));
        recorder.exit_invocation(at(start, 5));
        recorder.enter_invocation(call(InvocationKind::AboutToWait), at(start, 6));
        recorder.exit_invocation(at(start, 7));
        recorder.exit_outer(at(start, 8));
        let frozen = recorder.freeze();
        let nanos_per_ms = 1_000_000;
        assert_eq!(
            frozen.buffers.outer,
            [OuterCallbackV1 {
                enter_ns: nanos_per_ms,
                return_ns: 8 * nanos_per_ms,
                id: 0,
                kind: OuterKind::AboutToWait,
                open: false,
                clipped_at_arm: false,
            }]
        );
        let kinds: Vec<(InvocationKind, u32, bool, u64, u64)> = frozen
            .buffers
            .invocations
            .iter()
            .map(|record| {
                (record.kind, record.parent, record.synthetic, record.enter_ns, record.return_ns)
            })
            .collect();
        assert_eq!(
            kinds,
            [
                (
                    InvocationKind::WindowEvent { redraw: false },
                    0,
                    true,
                    2 * nanos_per_ms,
                    3 * nanos_per_ms
                ),
                (InvocationKind::RunAction, 0, false, 4 * nanos_per_ms, 5 * nanos_per_ms),
                (InvocationKind::AboutToWait, 0, false, 6 * nanos_per_ms, 7 * nanos_per_ms),
            ]
        );
        assert_eq!(frozen.buffers.invocations[0].window, 9);
        assert!(!frozen.incomplete && !frozen.overflow, "{frozen:?}");
    }

    /// Arming inside a running callback starts that callback's record at the arm, clipped, so the
    /// invocations it makes after the arm have a parent; a callback still open at take stays open
    /// (`OpenAtTake`), which is clipped, not missing.
    #[test]
    fn a_callback_running_at_arm_is_clipped_and_one_open_at_take_stays_open() {
        let start = Instant::now();
        let mut recorder = Recorder::arm(start, Some(OuterKind::AboutToWait), Buffers::new());
        recorder.enter_invocation(call(InvocationKind::AboutToWait), at(start, 1));
        recorder.exit_invocation(at(start, 2));
        recorder.exit_outer(at(start, 3));
        recorder.enter_outer(OuterKind::UserEvent, at(start, 4));
        let frozen = recorder.freeze();
        let outer = &frozen.buffers.outer;
        assert_eq!(outer[0].enter_ns, 0);
        assert!(outer[0].clipped_at_arm && !outer[0].open);
        assert_eq!(frozen.buffers.invocations[0].parent, 0);
        assert!(outer[1].open && !outer[1].clipped_at_arm);
        assert!(!frozen.incomplete, "an outer callback open at take is clipped, not incomplete");
    }

    /// Every record the join cannot use makes the sample incomplete: an orphan invocation, nested
    /// invocations, nested outer callbacks, a return with nothing open, an invocation unreturned at take,
    /// and a time before the arm.
    #[test]
    fn every_unjoinable_record_makes_the_sample_incomplete() {
        let start = Instant::now();
        let cases: [(&str, fn(&mut Recorder, Instant)); 7] = [
            ("orphan invocation", |recorder, start| {
                recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, 1));
                recorder.exit_invocation(at(start, 2));
            }),
            ("nested invocations", |recorder, start| {
                recorder.enter_outer(OuterKind::UserEvent, at(start, 1));
                recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, 2));
                recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, 3));
            }),
            ("nested outer callbacks", |recorder, start| {
                recorder.enter_outer(OuterKind::UserEvent, at(start, 1));
                recorder.enter_outer(OuterKind::NewEvents, at(start, 2));
            }),
            ("unmatched outer return", |recorder, start| recorder.exit_outer(at(start, 1))),
            ("unmatched invocation return", |recorder, start| {
                recorder.exit_invocation(at(start, 1))
            }),
            ("invocation unreturned at take", |recorder, start| {
                recorder.enter_outer(OuterKind::WindowEvent, at(start, 1));
                recorder.enter_invocation(
                    call(InvocationKind::WindowEvent { redraw: true }),
                    at(start, 2),
                );
            }),
            ("a time before the arm", |recorder, start| {
                recorder.enter_outer(OuterKind::Resumed, start - Duration::from_millis(1));
            }),
        ];
        for (name, script) in cases {
            let mut recorder = armed(start);
            script(&mut recorder, start);
            let frozen = recorder.freeze();
            assert!(frozen.incomplete, "{name}: {frozen:?}");
            assert!(!frozen.overflow, "{name}");
        }
    }

    /// The review's first trace: an outer callback that returns before it entered (enter at 10, return at
    /// 5) is incomplete, never a valid interval.
    #[test]
    fn a_reversed_outer_return_is_incomplete() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::UserEvent, at(start, 10));
        recorder.exit_outer(at(start, 5));
        assert!(recorder.freeze().incomplete);
    }

    /// The review's second trace: an outer callback returning while its invocation is still open (outer
    /// 1–3, invocation 2–4) would leave the invocation outside its parent, so the sample is incomplete.
    #[test]
    fn an_invocation_outliving_its_parent_is_incomplete() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 1));
        recorder.enter_invocation(call(InvocationKind::AboutToWait), at(start, 2));
        recorder.exit_outer(at(start, 3));
        recorder.exit_invocation(at(start, 4));
        assert!(recorder.freeze().incomplete);
    }

    /// An invocation that starts before its parent callback entered lies outside it: incomplete.
    #[test]
    fn an_invocation_starting_before_its_parent_is_incomplete() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::UserEvent, at(start, 5));
        recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, 2));
        recorder.exit_invocation(at(start, 6));
        recorder.exit_outer(at(start, 7));
        assert!(recorder.freeze().incomplete);
    }

    /// An invocation that returns before it entered is incomplete.
    #[test]
    fn a_reversed_invocation_return_is_incomplete() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::UserEvent, at(start, 1));
        recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, 6));
        recorder.exit_invocation(at(start, 4));
        recorder.exit_outer(at(start, 9));
        assert!(recorder.freeze().incomplete);
    }

    /// Records are sequential: an outer callback entering before the previous one returned, or an
    /// invocation entering before the previous one returned, is incomplete.
    #[test]
    fn overlapping_records_are_incomplete() {
        let start = Instant::now();
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::UserEvent, at(start, 1));
        recorder.exit_outer(at(start, 5));
        recorder.enter_outer(OuterKind::NewEvents, at(start, 3));
        recorder.exit_outer(at(start, 8));
        assert!(recorder.freeze().incomplete, "outer overlap");
        let mut recorder = armed(start);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 1));
        recorder.enter_invocation(call(InvocationKind::NewEvents), at(start, 2));
        recorder.exit_invocation(at(start, 6));
        recorder.enter_invocation(call(InvocationKind::AboutToWait), at(start, 4));
        recorder.exit_invocation(at(start, 7));
        recorder.exit_outer(at(start, 9));
        assert!(recorder.freeze().incomplete, "invocation overlap");
    }

    /// An offset that does not fit a `u64` is rejected, never saturated, and a time before the arm has no
    /// offset; an ordinary one converts exactly.
    #[test]
    fn an_unrepresentable_offset_is_rejected() {
        assert_eq!(offset_ns(Some(Duration::MAX)), None);
        assert_eq!(offset_ns(Some(Duration::from_nanos(u64::MAX))), Some(u64::MAX));
        assert_eq!(offset_ns(None), None);
        assert_eq!(offset_ns(Some(Duration::from_micros(3))), Some(3_000));
    }

    /// Each buffer holds exactly its capacity: the next callback or invocation overflows the sample,
    /// which is excluded rather than trimmed, and the records already kept are unchanged.
    #[test]
    fn each_buffer_overflows_one_past_its_capacity() {
        let start = Instant::now();
        let fill_outer = |count: u64| {
            let mut recorder = armed(start);
            for index in 0..count {
                recorder.enter_outer(OuterKind::NewEvents, at(start, index));
                recorder.exit_outer(at(start, index));
            }
            recorder.freeze()
        };
        let exact = fill_outer(OUTER_CAPACITY as u64);
        assert!(!exact.overflow && !exact.incomplete, "exactly the capacity fits");
        let over = fill_outer(OUTER_CAPACITY as u64 + 1);
        assert!(over.overflow && !over.incomplete);
        assert_eq!(over.buffers.outer.len(), OUTER_CAPACITY);

        let fill_invocations = |count: u64| {
            let mut recorder = armed(start);
            recorder.enter_outer(OuterKind::AboutToWait, at(start, 0));
            for index in 0..count {
                recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, index));
                recorder.exit_invocation(at(start, index));
            }
            recorder.freeze()
        };
        let exact = fill_invocations(INVOCATION_CAPACITY as u64);
        assert!(!exact.overflow && !exact.incomplete, "exactly the capacity fits");
        let over = fill_invocations(INVOCATION_CAPACITY as u64 + 1);
        assert!(over.overflow && !over.incomplete);
        assert_eq!(over.buffers.invocations.len(), INVOCATION_CAPACITY);
    }

    /// The records keep the sizes the memory budget counts: 24 bytes per outer callback and 40 per
    /// invocation on the supported 64-bit targets, so one sample's storage is exactly 13,312 bytes.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_records_keep_their_budgeted_sizes() {
        assert_eq!(std::mem::size_of::<OuterCallbackV1>(), 24);
        assert_eq!(std::mem::size_of::<InvocationV1>(), 40);
        assert_eq!(Buffers::new().payload_bytes(), 13_312);
    }

    /// One sample's storage is allocated at its full capacity once, never grows while recording, and is
    /// moved, not copied, into the frozen record and back for the next sample: the same allocation, the
    /// same capacity, and the live payload never more than one sample's 13,312 bytes.
    #[test]
    fn the_storage_is_moved_at_freeze_and_reused_by_the_next_sample() {
        let start = Instant::now();
        let buffers = Buffers::new();
        assert_eq!(
            (buffers.outer.capacity(), buffers.invocations.capacity()),
            (OUTER_CAPACITY, INVOCATION_CAPACITY)
        );
        let pointers = (buffers.outer.as_ptr(), buffers.invocations.as_ptr());
        let payload = buffers.payload_bytes();
        let mut recorder = Recorder::arm(start, None, buffers);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 0));
        for index in 0..INVOCATION_CAPACITY as u64 + 3 {
            recorder.enter_invocation(call(InvocationKind::UserEvent), at(start, index));
            recorder.exit_invocation(at(start, index));
        }
        let frozen = recorder.freeze();
        assert_eq!((frozen.buffers.outer.as_ptr(), frozen.buffers.invocations.as_ptr()), pointers);
        assert_eq!(frozen.buffers.payload_bytes(), payload, "recording never grew the storage");
        let recycled = frozen.recycle();
        assert!(recycled.outer.is_empty() && recycled.invocations.is_empty());
        let mut next = Recorder::arm(at(start, 1), Some(OuterKind::UserEvent), recycled);
        next.enter_invocation(call(InvocationKind::UserEvent), at(start, 2));
        next.exit_invocation(at(start, 3));
        let again = next.freeze();
        assert_eq!((again.buffers.outer.as_ptr(), again.buffers.invocations.as_ptr()), pointers);
        assert_eq!(again.buffers.payload_bytes(), payload);
        assert_eq!((again.buffers.outer.len(), again.buffers.invocations.len()), (1, 1));
    }
}

#[cfg(perf_dispatch_timeline_api)]
mod lifecycle_tests {
    use std::cell::Cell;
    use std::time::Instant;

    use super::lifecycle::{arm, close, Lifecycle};
    use super::recorders::Buffers;
    use super::{Disposition, OuterKind, TimelineArm, TimelineTake};

    /// A clock that must never be read.
    fn no_clock() -> Instant {
        panic!("an unarmed sample read the clock")
    }

    /// An armed token allocates storage once, records, is taken exactly once at close, and hands its
    /// storage back; the next armed sample reuses that same allocation.
    #[test]
    fn an_armed_sample_is_taken_once_and_its_storage_is_reused() {
        let mut storage: Option<Buffers> = None;
        let lifecycle = arm(
            TimelineArm::Armed(7_u64),
            Some(OuterKind::AboutToWait),
            &mut storage,
            Instant::now,
        );
        assert!(matches!(lifecycle, Lifecycle::Armed { token: 7, .. }));
        assert!(storage.is_none(), "the storage now belongs to the recorder");
        let takes = Cell::new(0);
        let disposition = close(lifecycle, &mut storage, |token| {
            takes.set(takes.get() + 1);
            assert_eq!(token, 7);
            TimelineTake::Timeline(())
        });
        assert_eq!((disposition, takes.get()), (Disposition::Recorded, 1));
        let pointer = storage.as_ref().expect("recycled").outer.as_ptr();
        let lifecycle = arm(TimelineArm::Armed(8_u64), None, &mut storage, Instant::now);
        assert!(storage.is_none());
        let disposition = close(lifecycle, &mut storage, |_| TimelineTake::<()>::NotRecorded);
        assert_eq!(disposition, Disposition::NotRecorded);
        assert_eq!(storage.as_ref().expect("recycled again").outer.as_ptr(), pointer);
    }

    /// Every arm outcome other than an armed token settles the sample at once: no clock is read, no
    /// storage is allocated, and close never calls take.
    #[test]
    fn an_unarmed_sample_allocates_nothing_and_is_never_taken() {
        let cases = [
            (TimelineArm::Unavailable, Disposition::Unavailable),
            (TimelineArm::NotRecorded, Disposition::NotRecorded),
            (TimelineArm::GateOff, Disposition::GateOff),
            (TimelineArm::NoPane, Disposition::NoPane),
            (TimelineArm::StillArmed(3_u64), Disposition::StillArmed),
            (TimelineArm::Exhausted, Disposition::Exhausted),
        ];
        for (outcome, expected) in cases {
            let mut storage: Option<Buffers> = None;
            let lifecycle = arm(outcome, Some(OuterKind::AboutToWait), &mut storage, no_clock);
            assert!(storage.is_none(), "{expected:?} allocated storage");
            let disposition = close(lifecycle, &mut storage, |_| -> TimelineTake<()> {
                panic!("an unarmed sample was taken")
            });
            assert_eq!(disposition, expected);
            assert!(storage.is_none(), "{expected:?}");
        }
    }

    /// Each take outcome of an armed sample is its own disposition, and a timeline whose harness record is
    /// incomplete reads `recorded-incomplete`, never `recorded`.
    #[test]
    fn every_take_outcome_has_its_own_disposition() {
        let cases = [
            (TimelineTake::Timeline(()), Disposition::Recorded),
            (TimelineTake::Unavailable, Disposition::Unavailable),
            (TimelineTake::NotRecorded, Disposition::NotRecorded),
            (TimelineTake::GateOff, Disposition::GateOff),
            (TimelineTake::Mismatch, Disposition::Mismatch),
            (TimelineTake::AlreadyTaken, Disposition::AlreadyTaken),
        ];
        for (outcome, expected) in cases {
            let mut storage: Option<Buffers> = None;
            let lifecycle = arm(TimelineArm::Armed(1_u64), None, &mut storage, Instant::now);
            assert_eq!(close(lifecycle, &mut storage, |_| outcome.clone()), expected);
            assert!(storage.is_some(), "{expected:?}: the storage is recycled after every take");
        }
        let mut storage: Option<Buffers> = None;
        let mut lifecycle = arm(TimelineArm::Armed(1_u64), None, &mut storage, Instant::now);
        if let Lifecycle::Armed { recorder, .. } = &mut lifecycle {
            // An orphan return: nothing is open, so the harness record is incomplete.
            recorder.exit_invocation(Instant::now());
        }
        let disposition = close(lifecycle, &mut storage, |_| TimelineTake::Timeline(()));
        assert_eq!(disposition, Disposition::RecordedIncomplete);
    }
}
