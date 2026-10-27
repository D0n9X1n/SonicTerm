//! Latency split, attribution and clock interval contracts.

use super::super::record_tests::*;
use super::*;

#[test]
fn a_failed_foreground_lock_is_noted() {
    // A failed LockSetForegroundWindow is not fatal: the run goes on, and its notes name the call
    // and the error, so a reader knows the window may have taken the foreground as it opened.
    let note = foreground_lock_note("Access is denied. (0x80070005)");
    let expected = "LockSetForegroundWindow failed: Access is denied. (0x80070005)";
    assert!(note.starts_with(expected), "{note}");
    assert!(note.contains("take the foreground"), "{note}");
}

#[test]
fn fresh_after_is_written_for_the_released_checkpoint_only() {
    // The `released` checkpoint names when its reading becomes fresh; other checkpoints leave the key
    // out, in result.json and progress.json alike. `frame_texture_bytes` appears only where read.
    let mut result = partial_result(Status::Valid);
    result.checkpoints = vec![
        CheckpointRecord {
            index: 0,
            label: "switched",
            unix_s: 10.0,
            ..CheckpointRecord::default()
        },
        CheckpointRecord {
            index: 1,
            label: "released",
            unix_s: 80.0,
            fresh_after_unix_s: Some(42.5),
            ..CheckpointRecord::default()
        },
        CheckpointRecord {
            index: 2,
            label: "end",
            unix_s: 90.0,
            frame_texture_bytes: Some(4),
            ..CheckpointRecord::default()
        },
    ];
    let written = result_json_as_written(&result);
    let progress = progress_of(Some("ab"), result.measurements());
    // progress.json flattens the measurements to its top level, as result.json lays them out.
    for document in [&written["checkpoints"], &progress["checkpoints"]] {
        assert!(document[0].get("fresh_after_unix_s").is_none());
        assert_eq!(document[1]["fresh_after_unix_s"], 42.5);
        assert!(document[2].get("fresh_after_unix_s").is_none());
        assert_eq!(document[2]["frame_texture_bytes"], 4);
        assert!(document[1].get("frame_texture_bytes").is_none());
    }
}

/// The bounded min-heap keeps exactly the SLOW_DISPATCH_LIMIT longest dispatches, longest first, while
/// `dispatch_count` counts every dispatch it was offered.
#[test]
fn slow_dispatches_keep_the_longest_and_count_every_dispatch() {
    let mut slow = SlowDispatches::default();
    // 200 dispatches of 1..=200 ms in a shuffled order, so eviction is exercised throughout.
    for index in 0..200_u32 {
        let duration_ms = f64::from((index * 37) % 200 + 1);
        slow.push(dispatch_of(f64::from(index), duration_ms));
    }
    let (kept, count) = slow.finish();
    assert_eq!(count, 200);
    assert_eq!(kept.len(), SLOW_DISPATCH_LIMIT);
    let durations: Vec<f64> = kept.iter().map(|dispatch| dispatch.duration_ms).collect();
    let expected: Vec<f64> = (0..SLOW_DISPATCH_LIMIT).map(|rank| (200 - rank) as f64).collect();
    assert_eq!(durations, expected, "the 64 longest, longest first");
    // Each kept record still carries its own start and end.
    assert!(kept.iter().all(|dispatch| (dispatch.end_unix_s - dispatch.start_unix_s) * 1000.0
        - dispatch.duration_ms
        < 1e-6));
}

/// Fewer dispatches than the limit are all kept; none leaves an empty record and a zero count.
#[test]
fn slow_dispatches_below_the_limit_keep_everything() {
    let mut slow = SlowDispatches::default();
    for (start_s, duration_ms) in [(1.0, 5.0), (2.0, 9.0), (3.0, 7.0)] {
        slow.push(dispatch_of(start_s, duration_ms));
    }
    let (kept, count) = slow.finish();
    assert_eq!(count, 3);
    assert_eq!(
        kept.iter().map(|dispatch| dispatch.duration_ms).collect::<Vec<_>>(),
        [9.0, 7.0, 5.0]
    );
    assert_eq!(SlowDispatches::default().finish(), (Vec::new(), 0));
}

#[test]
fn atlas_readings_are_written_per_attempt_and_left_out_when_none_were_taken() {
    // perf-compare reconciles a checkpoint's snapshot growths only with the reading taken in the same
    // sampling attempt, so each reading carries its attempt, the main window's native label, each live
    // window's counted growths and the closed windows' total, or nulls when the run does not count. A
    // checkpoint that took no reading leaves the key out, in result.json and progress.json alike.
    let mut result = partial_result(Status::Valid);
    result.checkpoints = vec![
        CheckpointRecord { index: 0, label: "settled", unix_s: 1.0, ..CheckpointRecord::default() },
        CheckpointRecord {
            index: 1,
            label: "end",
            unix_s: 2.0,
            atlas_readings: vec![
                AtlasReading {
                    attempt: 1,
                    main_window: Some("WindowId(7)".to_owned()),
                    counted_glyph_atlas_growths: Some(std::collections::BTreeMap::from([(
                        "WindowId(7)".to_owned(),
                        3,
                    )])),
                    closed_glyph_atlas_growths: Some(2),
                },
                AtlasReading {
                    attempt: 2,
                    main_window: None,
                    counted_glyph_atlas_growths: None,
                    closed_glyph_atlas_growths: None,
                },
            ],
            ..CheckpointRecord::default()
        },
    ];
    let written = result_json_as_written(&result);
    let progress = progress_of(Some("ab"), result.measurements());
    for document in [&written["checkpoints"], &progress["checkpoints"]] {
        assert!(document[0].get("atlas_readings").is_none());
        assert_eq!(
            document[1]["atlas_readings"],
            json!([
                {"attempt": 1, "main_window": "WindowId(7)",
                 "counted_glyph_atlas_growths": {"WindowId(7)": 3}, "closed_glyph_atlas_growths": 2},
                {"attempt": 2, "main_window": null, "counted_glyph_atlas_growths": null,
                 "closed_glyph_atlas_growths": null}
            ])
        );
    }
}

/// A phase record writes `updates` only when its phase carries one, so a comparison can tell a
/// harness that records the denominator from one that predates it.
#[test]
fn a_phase_record_writes_updates_only_when_it_has_them() {
    let mut record = measured_result().phases.remove(0);
    record.updates = None;
    assert!(serde_json::to_value(&record).unwrap().get("updates").is_none());
    record.updates = Some(300);
    assert_eq!(serde_json::to_value(&record).unwrap()["updates"], json!(300));
}

/// The three parts are whole nanoseconds and add up exactly to `ended - injected`; a suppressed
/// decision still splits, and the delivery lag is publication to decision.
#[test]
fn split_parts_telescope_exactly_to_ended_minus_injected() {
    let instants = split_instants();
    let [injected, _, _, _, ended] = instants;
    let mut facts = split_facts(instants);
    let split = split_for(injected, ended, &EchoOutcome::Taken(facts), false).expect("splits");
    let total = duration_ns(ended - injected).expect("fits");
    assert_eq!(
        split.input_to_parse_ns + split.parse_to_publication_ns + split.publication_to_present_ns,
        total
    );
    assert_eq!((split.input_to_parse_ns, split.delivery_lag_ns), (3_000_001, 41_003));
    assert_eq!(split.echo_generation, 7);
    facts.delivery = Some(DeliveryFacts { outcome: Delivery::Suppressed, decided_at: instants[3] });
    let suppressed = split_for(injected, ended, &EchoOutcome::Taken(facts), false).expect("splits");
    assert_eq!(suppressed.delivery, Delivery::Suppressed);
}

/// An appearance before the injection or a publication before the appearance fails a checked
/// interval and reads `clock-order`, never a saturated zero; an unrepresentable interval does too.
#[test]
fn out_of_order_or_overflowing_endpoints_read_clock_order() {
    let instants = split_instants();
    let [injected, parsed, published, _, ended] = instants;
    let mut early_parse = split_facts(instants);
    early_parse.appearance.as_mut().expect("appeared").parsed_at =
        injected - Duration::from_nanos(1);
    assert_eq!(
        split_for(injected, ended, &EchoOutcome::Taken(early_parse), false),
        Err("clock-order")
    );
    let mut late_parse = split_facts(instants);
    late_parse.appearance.as_mut().expect("appeared").parsed_at =
        published + Duration::from_nanos(1);
    assert_eq!(
        split_for(injected, ended, &EchoOutcome::Taken(late_parse), false),
        Err("clock-order")
    );
    assert_eq!(duration_ns(Duration::MAX), None, "an interval past u64 nanoseconds is refused");
    assert_eq!(duration_ns(parsed - injected), Some(3_000_001));
}

/// Serialized in milliseconds, the three parts add up to the sample's latency within 0.001 ms.
#[test]
fn serialized_parts_sum_within_one_microsecond() {
    let instants = split_instants();
    let [injected, _, _, _, ended] = instants;
    let sample = LatencySample::credited(
        1.0,
        injected,
        ended,
        &EchoOutcome::Taken(split_facts(instants)),
        false,
    );
    let json = serde_json::to_value(sample).expect("a sample converts");
    assert_eq!(json["split_reason"], "split");
    let split = &json["split"];
    let parts: f64 = ["input_to_parse_ms", "parse_to_publication_ms", "publication_to_present_ms"]
        .iter()
        .map(|key| split[*key].as_f64().expect("a number"))
        .sum();
    let latency = json["latency_ms"].as_f64().expect("credited");
    assert!((parts - latency).abs() <= 0.001, "{parts} vs {latency}");
    assert_eq!(split["delivery"], "sent");
    assert_eq!(split["delivery_lag_us"], 41.003);
    assert_eq!(
        (split["coalesced"].clone(), split["sync_open"].clone()),
        (json!(false), json!(false))
    );
}

/// Each row of the precedence table applies when only its fact is set, and an earlier row wins
/// over a later one. Rows the App protocol reaches are also pinned with real traces below.
#[test]
fn split_reason_follows_the_precedence_table() {
    let instants = split_instants();
    let [injected, _, published, _, ended] = instants;
    let reason = |outcome: EchoOutcome, harness_changed: bool| {
        split_for(injected, ended, &outcome, harness_changed).err().unwrap_or(SPLIT)
    };
    let with = |change: &dyn Fn(&mut TraceFacts)| {
        let mut facts = split_facts(instants);
        change(&mut facts);
        EchoOutcome::Taken(facts)
    };
    assert_eq!(reason(EchoOutcome::Unsupported, true), "unsupported");
    for failed in &SPLIT_REASONS[1..8] {
        assert_eq!(reason(EchoOutcome::Failed(failed), true), *failed);
    }
    let rows: [(&str, &dyn Fn(&mut TraceFacts)); 11] = [
        ("pane-not-shown", &|facts| {
            facts.shown = false;
            facts.identity_changed = true;
        }),
        ("row-identity-changed", &|facts| {
            facts.identity_changed = true;
            facts.pre_present = true;
        }),
        ("first-appearance-unobserved", &|facts| {
            facts.pre_present = true;
            facts.appearance = None;
        }),
        ("no-appearance-observed", &|facts| {
            facts.appearance = None;
            facts.lost = true;
        }),
        ("echo-overwritten", &|facts| {
            facts.lost = true;
            facts.publication = None;
        }),
        ("no-publication-observed-by-take", &|facts| facts.publication = None),
        ("untargeted", &|facts| {
            facts.publication.as_mut().expect("published").window = PublicationWindow::Untargeted;
            facts.delivery = None;
        }),
        ("other-window", &|facts| {
            facts.publication.as_mut().expect("published").window = PublicationWindow::Other;
        }),
        ("send-refused", &|facts| {
            facts.delivery.as_mut().expect("decided").outcome = Delivery::Refused;
        }),
        ("delivery-not-observed-by-take", &|facts| facts.delivery = None),
        ("delivered-after-present", &|facts| {
            facts.delivery.as_mut().expect("decided").decided_at += Duration::from_secs(1);
        }),
    ];
    for (expected, change) in rows {
        assert_eq!(reason(with(change), false), expected);
    }
    assert_eq!(reason(with(&|_| {}), true), "row-identity-changed", "the harness's own check");
    let before_publication = split_for(
        injected,
        published - Duration::from_nanos(1),
        &with(&|facts| facts.delivery.as_mut().expect("decided").decided_at = published),
        false,
    );
    assert_eq!(before_publication.err(), Some("presented-before-publication"));
    assert_eq!(reason(with(&|_| {}), false), SPLIT);
    assert_eq!(SPLIT_REASONS.len(), 22);
    assert_eq!(SPLIT_REASONS[21], SPLIT);
}

/// A sample no frame was credited with reads `not-credited` with no split, whatever the build.
#[test]
fn uncredited_samples_read_not_credited() {
    let sample = LatencySample::uncredited(3.0, UnattributedReason::NoCandidate.as_str());
    assert_eq!((sample.split, sample.split_reason), (None, NOT_CREDITED));
    let json = serde_json::to_value(sample).expect("a sample converts");
    assert_eq!(
        (json["split"].clone(), json["split_reason"].clone()),
        (json!(null), json!("not-credited"))
    );
    let report = latency_json(&[sample]);
    assert_eq!(report["split_reasons"], json!({}), "an uncredited sample is not a split reason");
    assert_eq!(report["split_coverage"], json!(null));
}

/// A sample's dispatch timeline reads `not-taken` until its close records one, and the recorded
/// disposition is what the result's `dispatch_timeline` key reports.
#[test]
fn a_closed_samples_dispatch_timeline_is_what_the_result_reports() {
    let open = LatencySample::uncredited(3.0, UnattributedReason::NoCandidate.as_str());
    assert_eq!(open.dispatch_timeline, TIMELINE_NOT_TAKEN);
    let closed = open.with_dispatch_timeline("not-recorded");
    assert_eq!(closed.dispatch_timeline, "not-recorded");
    let json = serde_json::to_value(closed).expect("a sample converts");
    assert_eq!(json["dispatch_timeline"], json!("not-recorded"));
}

/// A report of no samples still carries the split schema, with zero counts, no reasons and null
/// coverage, so a reader never mistakes it for an older harness's report.
#[test]
fn an_empty_report_serializes_zero_counts_empty_reasons_and_null_coverage() {
    let report = latency_json(&[]);
    assert_eq!(report["split_schema"], 1);
    assert_eq!(report["split_count"], 0);
    assert_eq!(report["split_reasons"], json!({}));
    assert_eq!(report["split_coverage"], json!(null));
    let instants = split_instants();
    let split = LatencySample::credited(
        1.0,
        instants[0],
        instants[4],
        &EchoOutcome::Taken(split_facts(instants)),
        false,
    );
    let unsupported = credited_sample(2.0, 5.0);
    let uncredited = LatencySample::uncredited(3.0, UnattributedReason::NoCandidate.as_str());
    // Coverage and reasons count credited samples only: the uncredited one changes neither.
    let mixed = latency_json(&[split, unsupported, uncredited]);
    assert_eq!(mixed["split_reasons"], json!({"split": 1, "unsupported": 1}));
    assert_eq!(
        (mixed["split_count"].clone(), mixed["split_coverage"].clone()),
        (json!(1), json!(0.5))
    );
}

/// Without `perf-echo-trace` nothing is armed, so a credited sample reads `unsupported`.
#[cfg(not(feature = "perf-echo-trace"))]
#[test]
fn credited_samples_read_unsupported_without_the_feature() {
    let mut app = sonicterm_app::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let target = echo_target((0, 6), 80, 0);
    let arm = echo_api::arm(&mut app, 1, &target, RowIdentity::default());
    assert_eq!(arm, ArmState::Unsupported);
    let outcome = echo_outcome(arm, |token| echo_api::take(&mut app, 1, token, None).0);
    let injected = Instant::now();
    let sample = LatencySample::credited(
        1.0,
        injected,
        injected + Duration::from_millis(4),
        &outcome,
        false,
    );
    assert_eq!((sample.split, sample.split_reason), (None, "unsupported"));
}

/// The precedence rows the App protocol reaches, each from real arm, worker and take calls.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn split_reason_rows_reached_through_the_real_app() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let main = sonicterm_app::app::synthetic_main_window_id();
    let after = |now: Instant| now + Duration::from_millis(5);
    let publish = |bytes: &'static [u8]| {
        move |app: &mut sonicterm_app::app::App, _: Option<EchoToken>| {
            let pane = app.main().expect("main").tab_states[0].active_pane;
            app.__test_publish_pane_output(main, pane, bytes);
        }
    };
    let nothing = |_: &mut sonicterm_app::app::App, _: Option<EchoToken>| {};

    let (mut app, pane) = counting_app();
    assert_eq!(app_reason(&mut app, pane, publish(b"a"), after), "split");

    let subscriber = Registry::default().with(EnvFilter::new("warn"));
    let mut gate_off = sonicterm_logging::test_capture::with_default(subscriber, || {
        sonicterm_app::app::App::new(
            sonicterm_cfg::theme::Theme::default(),
            sonicterm_cfg::config::Config::default(),
            sonicterm_cfg::keymap::Keymap::default(),
        )
    });
    let off_pane = gate_off.__test_seed_tab("off");
    assert_eq!(app_reason(&mut gate_off, off_pane, nothing, after), "arm-gate-off");

    let (mut app, pane) = counting_app();
    assert_eq!(app_reason(&mut app, pane + 1000, nothing, after), "arm-no-pane");
    app.__test_set_next_echo_arm(u64::MAX);
    assert_eq!(app_reason(&mut app, pane, nothing, after), "arm-exhausted");

    let (mut app, pane) = counting_app();
    let remove = |app: &mut sonicterm_app::app::App, _: Option<EchoToken>| {
        let pane = app.main().expect("main").tab_states[0].active_pane;
        app.main_panes_mut().expect("main").remove(&pane);
    };
    assert_eq!(app_reason(&mut app, pane, remove, after), "take-no-pane");

    let (mut app, pane) = counting_app();
    let rearm = |app: &mut sonicterm_app::app::App, _: Option<EchoToken>| {
        let pane = app.main().expect("main").tab_states[0].active_pane;
        let target = EchoTarget { abs_row: 0, col: 1, character: 'b' };
        echo_api::arm(app, pane, &target, RowIdentity::default());
    };
    assert_eq!(app_reason(&mut app, pane, rearm, after), "take-mismatch");

    let (mut app, pane) = counting_app();
    let take_early = |app: &mut sonicterm_app::app::App, token: Option<EchoToken>| {
        let pane = app.main().expect("main").tab_states[0].active_pane;
        echo_api::discard(app, pane, token.expect("armed"));
    };
    assert_eq!(app_reason(&mut app, pane, take_early, after), "take-already-taken");

    let (mut app, pane) = counting_app();
    app.__test_seed_tab("second");
    assert!(app.__test_invoke_activate_main_tab(1), "the second tab is active");
    assert_eq!(app_reason(&mut app, pane, nothing, after), "pane-not-shown");

    let (mut app, pane) = counting_app();
    assert_eq!(app_reason(&mut app, pane, publish(b"\x1b[?1049ha"), after), "row-identity-changed");

    let (mut app, pane) = counting_app();
    app.__test_publish_pane_output(main, pane, b"a");
    assert_eq!(app_reason(&mut app, pane, publish(b"b"), after), "first-appearance-unobserved");

    let (mut app, pane) = counting_app();
    assert_eq!(app_reason(&mut app, pane, nothing, after), "no-appearance-observed");

    let (mut app, pane) = counting_app();
    let overwrite = |app: &mut sonicterm_app::app::App, token: Option<EchoToken>| {
        publish(b"a")(app, token);
        publish(b"\rb")(app, token);
    };
    assert_eq!(app_reason(&mut app, pane, overwrite, after), "echo-overwritten");

    let (mut app, pane) = counting_app();
    let unflushed = |app: &mut sonicterm_app::app::App, _: Option<EchoToken>| {
        let pane = app.main().expect("main").tab_states[0].active_pane;
        let mut worker = app.__test_pane_worker(main, pane).expect("worker");
        assert!(worker.batch(b"a", Instant::now()).is_empty(), "a small batch waits to coalesce");
    };
    assert_eq!(app_reason(&mut app, pane, unflushed, after), "no-publication-observed-by-take");

    let (mut app, pane) = counting_app();
    let before = |now: Instant| now - Duration::from_secs(1);
    assert_eq!(app_reason(&mut app, pane, publish(b"a"), before), "presented-before-publication");
}

/// `hooks.trim` names the trim hook's outcome; a run whose plan never covered the window reads
/// `not-reached`, and each outcome has its own name.
#[test]
fn result_json_records_the_trim_hooks_outcome() {
    let mut result = partial_result(Status::Valid);
    assert_eq!(result.to_json()["hooks"], json!({"trim": "not-reached"}));
    result.trim_hook = TrimHookOutcome::Unsupported;
    assert_eq!(result.to_json()["hooks"], json!({"trim": "unsupported"}));
    let names: Vec<_> = [
        TrimHookOutcome::NotReached,
        TrimHookOutcome::Unsupported,
        TrimHookOutcome::Skipped,
        TrimHookOutcome::Trimmed,
    ]
    .iter()
    .map(|outcome| outcome.as_str())
    .collect();
    assert_eq!(names, ["not-reached", "unsupported", "skipped", "trimmed"]);
}
