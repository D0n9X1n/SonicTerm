use super::*;

/// The recorded cfg is this build's own: the adapter calls the App exactly when the cfg is set.
#[test]
fn the_recorded_cfg_is_the_builds_own() {
    assert_eq!(API_ENABLED, cfg!(perf_dispatch_timeline_api));
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
/// rather than answering `Unavailable` itself.
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
        InvocationKind, InvocationV1, OuterCallbackV1, OuterKind, Recorder, INVOCATION_CAPACITY,
        OUTER_CAPACITY,
    };

    /// `offset_ms` milliseconds after `start`.
    fn at(start: Instant, offset_ms: u64) -> Instant {
        start + Duration::from_millis(offset_ms)
    }

    /// One `about_to_wait` carrying a synthetic window event, a `RunAction` and the `AboutToWait`
    /// forward, in that order: each invocation joins the one outer callback, and every time is relative
    /// to the arm.
    #[test]
    fn one_outer_callback_joins_its_three_invocations() {
        let start = Instant::now();
        let mut recorder = Recorder::arm(start, None);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 1));
        recorder.enter_invocation(
            InvocationKind::WindowEvent { redraw: false },
            9,
            true,
            at(start, 2),
        );
        recorder.exit_invocation(at(start, 3));
        recorder.enter_invocation(InvocationKind::RunAction, 0, false, at(start, 4));
        recorder.exit_invocation(at(start, 5));
        recorder.enter_invocation(InvocationKind::AboutToWait, 0, false, at(start, 6));
        recorder.exit_invocation(at(start, 7));
        recorder.exit_outer(at(start, 8));
        let frozen = recorder.freeze();
        let nanos_per_ms = 1_000_000;
        assert_eq!(
            frozen.outer,
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
            .invocations
            .iter()
            .map(|call| (call.kind, call.parent, call.synthetic, call.enter_ns, call.return_ns))
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
        assert_eq!(frozen.invocations[0].window, 9);
        assert!(!frozen.incomplete && !frozen.overflow, "{frozen:?}");
    }

    /// Arming inside a running callback starts that callback's record at the arm, clipped, so the
    /// invocations it makes after the arm have a parent; a callback still open at take stays open
    /// (`OpenAtTake`), which is clipped, not missing.
    #[test]
    fn a_callback_running_at_arm_is_clipped_and_one_open_at_take_stays_open() {
        let start = Instant::now();
        let mut recorder = Recorder::arm(start, Some(OuterKind::AboutToWait));
        recorder.enter_invocation(InvocationKind::AboutToWait, 0, false, at(start, 1));
        recorder.exit_invocation(at(start, 2));
        recorder.exit_outer(at(start, 3));
        recorder.enter_outer(OuterKind::UserEvent, at(start, 4));
        let frozen = recorder.freeze();
        assert_eq!(frozen.outer[0].enter_ns, 0);
        assert!(frozen.outer[0].clipped_at_arm && !frozen.outer[0].open);
        assert_eq!(frozen.invocations[0].parent, 0);
        assert!(frozen.outer[1].open && !frozen.outer[1].clipped_at_arm);
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
                recorder.enter_invocation(InvocationKind::UserEvent, 0, false, at(start, 1));
                recorder.exit_invocation(at(start, 2));
            }),
            ("nested invocations", |recorder, start| {
                recorder.enter_outer(OuterKind::UserEvent, at(start, 1));
                recorder.enter_invocation(InvocationKind::UserEvent, 0, false, at(start, 2));
                recorder.enter_invocation(InvocationKind::UserEvent, 0, false, at(start, 3));
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
                    InvocationKind::WindowEvent { redraw: true },
                    4,
                    false,
                    at(start, 2),
                );
            }),
            ("a time before the arm", |recorder, start| {
                recorder.enter_outer(OuterKind::Resumed, start - Duration::from_millis(1));
            }),
        ];
        for (name, script) in cases {
            let mut recorder = Recorder::arm(start, None);
            script(&mut recorder, start);
            let frozen = recorder.freeze();
            assert!(frozen.incomplete, "{name}: {frozen:?}");
            assert!(!frozen.overflow, "{name}");
        }
    }

    /// Each buffer holds exactly its capacity: the next callback or invocation overflows the sample,
    /// which is excluded rather than trimmed, and the records already kept are unchanged.
    #[test]
    fn each_buffer_overflows_one_past_its_capacity() {
        let start = Instant::now();
        let mut recorder = Recorder::arm(start, None);
        for index in 0..OUTER_CAPACITY as u64 {
            recorder.enter_outer(OuterKind::NewEvents, at(start, index));
            recorder.exit_outer(at(start, index));
        }
        assert!(!recorder.freeze().overflow, "exactly the capacity fits");
        recorder.enter_outer(OuterKind::NewEvents, at(start, 999));
        recorder.exit_outer(at(start, 999));
        let frozen = recorder.freeze();
        assert!(
            frozen.overflow && !frozen.incomplete,
            "{:?}",
            (frozen.overflow, frozen.incomplete)
        );
        assert_eq!(frozen.outer.len(), OUTER_CAPACITY);

        let mut recorder = Recorder::arm(start, None);
        recorder.enter_outer(OuterKind::AboutToWait, at(start, 0));
        for index in 0..INVOCATION_CAPACITY as u64 {
            recorder.enter_invocation(InvocationKind::UserEvent, 0, false, at(start, index));
            recorder.exit_invocation(at(start, index));
        }
        assert!(!recorder.freeze().overflow, "exactly the capacity fits");
        recorder.enter_invocation(InvocationKind::UserEvent, 0, false, at(start, 999));
        recorder.exit_invocation(at(start, 999));
        let frozen = recorder.freeze();
        assert!(frozen.overflow && !frozen.incomplete);
        assert_eq!(frozen.invocations.len(), INVOCATION_CAPACITY);
    }

    /// The records keep the sizes the memory budget counts: 24 bytes per outer callback and 40 per
    /// invocation on the supported 64-bit targets.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_records_keep_their_budgeted_sizes() {
        assert_eq!(std::mem::size_of::<OuterCallbackV1>(), 24);
        assert_eq!(std::mem::size_of::<InvocationV1>(), 40);
    }
}
