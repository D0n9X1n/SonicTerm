//! Credited dispatch and echo observation timing.

use super::super::probe_tests::*;
use super::*;

/// The production demand sequence, through the `AtlasRetryDriver` methods `Probe` calls, repeated while
/// the barrier is unmet: Arm, `take_request` consumes the request, the forwarded `RequestRedraw` is
/// observed with zero attempts, then the `RedrawRequested` frame with one. The request callback never
/// rearms itself, each settling frame asks for exactly one more redraw, so requests are settling frames
/// plus the first, and the drive is due at the arm while a request waits and at the settle deadline
/// otherwise. The fake 500 ms foreground schedule is fixture input, not App scheduling.
#[test]
fn the_request_callback_does_not_rearm_itself_and_settling_frames_keep_demand() {
    let start = Instant::now();
    let mut driver = barred_driver(start);
    assert_eq!(driver.due(), Some(start), "the first settling redraw is requested at once");
    let (mut requests, mut frames) = (0_u32, 0_u32);
    for round in 0..40_u64 {
        let now = start + Duration::from_millis(200 + 200 * round);
        // The stale shell stays applied, resampled every 500 ms, so the barrier never meets.
        let cached = applied(start, 100 + 500 * (round * 200 / 500 + 1), Some("bash"));
        let reading = retry_reading("#1 \u{E760} shell", Some(cached));
        assert!(driver.take_request(), "round {round}: one request is pending");
        requests += 1;
        assert!(!driver.take_request(), "a request is consumed once");
        assert_eq!(driver.due(), Some(start + atlas_retry::SETTLE_BOUND));
        assert_eq!(driver.observe(Some(no_attempt()), &reading, now), None);
        assert!(!driver.take_request(), "round {round}: the request callback did not rearm");
        assert_eq!(driver.observe(Some(one_frame()), &reading, now), Some(Arm::Redraw));
        frames += 1;
        assert_eq!(driver.due(), Some(now), "due from the arm while the request waits");
    }
    assert!(driver.take_request());
    requests += 1;
    assert_eq!(
        requests,
        frames + 1,
        "no busy loop: one request per settling frame, plus the first"
    );
    assert!(!driver.machine.settled());
}

/// A zero-attempt dispatch still applies its foreground readings before it returns: a cleared sample
/// between two qualifying results resets the history without rearming, so one later valid result is
/// insufficient and nothing settles.
#[test]
fn a_zero_attempt_none_between_qualifying_results_resets_without_rearming() {
    let start = Instant::now();
    let mut driver = barred_driver(start);
    assert!(driver.take_request());
    let title = "#1 \u{F489} shell";
    let first = retry_reading(title, Some(applied(start, 200, Some("sleep"))));
    let now = start + Duration::from_millis(210);
    assert_eq!(driver.observe(Some(one_frame()), &first, now), Some(Arm::Redraw));
    assert!(driver.take_request());
    let cleared = retry_reading(title, Some(applied(start, 300, None)));
    let now = start + Duration::from_millis(310);
    assert_eq!(driver.observe(Some(no_attempt()), &cleared, now), None);
    assert!(!driver.take_request(), "the zero-attempt dispatch did not rearm");
    let second = retry_reading(title, Some(applied(start, 700, Some("sleep"))));
    for step in 0..5_u64 {
        let now = start + Duration::from_millis(710 + 16 * step);
        assert_eq!(driver.observe(Some(one_frame()), &second, now), Some(Arm::Redraw));
        assert!(driver.take_request());
    }
    assert!(
        !driver.machine.settled(),
        "the None reset the history, so one valid result is too few"
    );
}

/// The final-process lookup accepts only the expected name: `sleep` on macOS and the harness binary's
/// normalized basename on Windows. The stale shell (`bash`), `sh`, `cat` and no process do not qualify,
/// and a refused name sets no T.
#[test]
fn the_final_process_lookup_accepts_only_the_expected_name() {
    assert_eq!(final_process_name(false, None), "sleep");
    let harness = std::path::Path::new(r"C:\runner\target\release\examples\Perf_Scenarios.EXE");
    assert_eq!(final_process_name(true, Some(harness)), "perf_scenarios");
    assert_eq!(final_process_name(false, Some(harness)), "sleep");
    for (expected, name, accepted) in [
        ("sleep", Some("sleep"), true),
        ("sleep", Some("bash"), false),
        ("sleep", Some("sh"), false),
        ("sleep", Some("cat"), false),
        ("sleep", None, false),
        ("perf_scenarios", Some("perf_scenarios"), true),
        ("perf_scenarios", Some("sleep"), false),
    ] {
        let lookup = atlas_retry::FinalLookup::new(expected.to_owned());
        assert_eq!(lookup.accepts(name), accepted, "{expected} against {name:?}");
    }
    let start = Instant::now();
    let deadline = Some(start + atlas_retry::SETTLE_BOUND);
    let mut lookup = atlas_retry::FinalLookup::new("sleep".to_owned());
    assert!(lookup.begin(start, deadline));
    assert!(!lookup.finish(Some("bash"), start, start, deadline));
    assert_eq!(lookup.found_at(), None);
}

/// At the production deadline wake, a zero-attempt dispatch completes at the settle bound before the drive
/// can call `expire`. The failure it records must still name the unmet barrier condition after its
/// "completed past its bound" prefix, and the dispatch neither rearms nor settles the scene.
#[test]
fn a_zero_attempt_dispatch_at_the_settle_deadline_names_the_unmet_barrier() {
    let start = Instant::now();
    let mut driver = barred_driver(start);
    assert!(driver.take_request());
    let stale = retry_reading("#1 \u{E760} shell", Some(applied(start, 200, Some("bash"))));
    let now = start + Duration::from_millis(210);
    assert_eq!(driver.observe(Some(one_frame()), &stale, now), Some(Arm::Redraw));
    assert!(driver.take_request());
    let deadline = start + atlas_retry::SETTLE_BOUND;
    assert_eq!(driver.due(), Some(deadline), "the settle deadline is the next wake");
    assert_eq!(driver.observe(Some(no_attempt()), &stale, deadline), None);
    assert!(!driver.take_request(), "the late dispatch did not rearm");
    // An ended machine is no longer settling, so the test reads the recorded scene, not `settled()`.
    assert!(driver.machine.scene().is_none(), "no scene was settled");
    assert_eq!(
        driver.failure.as_deref(),
        Some(
            "settle (steady 0): completed past its bound; barrier unmet: 1 qualifying foreground \
             observation after the final process (need 2), latest cached \"bash\""
        )
    );
}

/// Windows fixes S1/atlas-retry's T at the sentinel the print phase saw, expecting the harness binary's
/// basename, and makes no lookup; macOS expects `sleep` and has no T until its own lookup finds it.
#[test]
fn the_final_process_search_is_fixed_at_the_sentinel_on_windows_only() {
    let sentinel_at = Instant::now();
    let harness = std::path::Path::new(r"C:\runner\target\release\examples\perf_scenarios.exe");
    let windows = final_lookup_for(true, Some(harness), Some(sentinel_at));
    assert_eq!((windows.expected(), windows.found_at()), ("perf_scenarios", Some(sentinel_at)));
    let macos = final_lookup_for(false, Some(harness), Some(sentinel_at));
    assert_eq!((macos.expected(), macos.found_at()), ("sleep", None));
    let unseen = final_lookup_for(true, Some(harness), None);
    assert_eq!(unseen.found_at(), None, "no sentinel seen leaves T unset");
}

/// The production `Probe::atlas_retry_barrier` reads the App's active tab: with no custom title it
/// expects the title the App derives for the final process, and with one it reports a fixture problem
/// instead of an expected title.
#[test]
fn the_probe_barrier_expects_the_apps_title_and_refuses_a_custom_one() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("atlas");
    let plan = scenarios::plan_for("S1", "atlas-retry", false, Host::Posix).expect("listed plan");
    let request = RunArgs {
        scenario: "S1",
        variant: "atlas-retry",
        managed: false,
        short: false,
        laps: false,
        counters: true,
        harness_hash: None,
        scratch: String::from("unused"),
        capture_delivery: false,
    };
    let now = Instant::now();
    let mut probe =
        Probe::new(app, plan, request, PathBuf::from("unused"), now + Duration::from_secs(3600));
    let barrier = probe.atlas_retry_barrier("sleep", Some(now));
    assert_eq!(barrier.final_at, Some(now));
    assert_eq!(barrier.expected_title.as_deref(), Some("#1 \u{F489} shell"));
    assert_eq!(barrier.fixture_problem, None);
    probe.app.main_mut().expect("seeded main").tabs.set_active_custom_title("manual");
    let barrier = probe.atlas_retry_barrier("sleep", Some(now));
    assert_eq!(barrier.fixture_problem.as_deref(), Some(r#"the tab has a custom title "manual""#));
    assert_eq!(barrier.expected_title, None);
}

/// Every way an S2 sample closes records what its dispatch timeline came to: the next injection's
/// close, a credited dispatch and an explicit close. A build with the cfg calls the App's stub, which
/// answers `not-recorded` with the gate on; a build without it never calls the App and records
/// `unavailable`. No close leaves `not-taken`, and an unarmed sample allocates no recorder storage.
#[test]
fn every_close_path_records_the_samples_dispatch_timeline() {
    let expected = if cfg!(perf_dispatch_timeline_api) { "not-recorded" } else { "unavailable" };
    let (mut probe, pane) = typing_probe("S2", "flood");
    assert_eq!(probe.open_typing_sample(Instant::now), Some('a'));
    assert_eq!(probe.open_typing_sample(Instant::now), Some('b'));
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"ab");
    credit_now(&mut probe);
    assert_eq!(probe.open_typing_sample(Instant::now), Some('c'));
    probe.close_sample(UnattributedReason::NoCandidate);
    let recorded: Vec<(&str, &str)> =
        probe.samples.iter().map(|sample| (sample.reason, sample.dispatch_timeline)).collect();
    assert_eq!(
        recorded,
        [
            ("no-candidate", expected),
            (crate::record::CREDITED, expected),
            ("no-candidate", expected),
        ]
    );
    assert!(probe.open_sample.is_none());
    #[cfg(perf_dispatch_timeline_api)]
    assert!(probe.timeline_storage.is_none(), "the stub never arms, so nothing is allocated");
}

/// The injection path arms the timeline after the previous sample closes and before the injection
/// instant is read; every way out of an open sample (`close_sample` and the credited branch of
/// `attribute`) closes its timeline, and no other method takes the open sample.
#[test]
fn the_timeline_is_armed_before_injection_and_taken_on_every_close() {
    let open = method("open_typing_sample");
    let close = open.find("self.close_sample(").expect("the previous sample closes");
    let arm = open.find("SampleTimeline::arm(").expect("the timeline arms");
    let read = open.find("let injected = clock();").expect("the injection instant is read");
    assert!(close < arm && arm < read, "close, then arm, then read the injection instant");
    let source = concat!(
        include_str!("probe.rs"),
        "\n",
        include_str!("probe_api.rs"),
        "\n",
        include_str!("probe_atlas.rs"),
        "\n",
        include_str!("probe_bootstrap.rs"),
        "\n",
        include_str!("probe_callbacks.rs"),
        "\n",
        include_str!("probe_checkpoint.rs"),
        "\n",
        include_str!("probe_input.rs"),
        "\n",
        include_str!("probe_occlusion.rs"),
        "\n",
        include_str!("probe_phases.rs"),
        "\n",
        include_str!("probe_sampling.rs"),
        "\n",
        include_str!("probe_scan.rs")
    )
    .replace("\r\n", "\n");
    assert_eq!(
        source.matches("self.open_sample.take()").count(),
        2,
        "only the two close paths take it"
    );
    for name in ["close_sample", "attribute"] {
        let body = method(name);
        assert!(body.contains("self.open_sample.take()"), "{name}");
        assert!(
            body.contains("sample.timeline.close(&mut self.app, &mut self.timeline_storage)"),
            "{name}"
        );
    }
}

/// Every `ApplicationHandler` callback runs its body inside its outer-callback record, and `forward`
/// records each invocation from its own readings around the dispatch, so App dispatches join to the
/// callback and invocation that carried them.
#[test]
fn every_callback_and_forward_records_its_timeline_parenting() {
    let source = concat!(
        include_str!("probe.rs"),
        "\n",
        include_str!("probe_api.rs"),
        "\n",
        include_str!("probe_atlas.rs"),
        "\n",
        include_str!("probe_bootstrap.rs"),
        "\n",
        include_str!("probe_callbacks.rs"),
        "\n",
        include_str!("probe_checkpoint.rs"),
        "\n",
        include_str!("probe_input.rs"),
        "\n",
        include_str!("probe_occlusion.rs"),
        "\n",
        include_str!("probe_phases.rs"),
        "\n",
        include_str!("probe_sampling.rs"),
        "\n",
        include_str!("probe_scan.rs")
    )
    .replace("\r\n", "\n");
    let start =
        source.find("impl ApplicationHandler<UserEvent> for Probe {").expect("the handler impl");
    let handlers = &source[start..start + source[start..].find("\n}\n").expect("its end")];
    for (name, kind) in [
        ("resumed", "Resumed"),
        ("window_event", "WindowEvent"),
        ("user_event", "UserEvent"),
        ("new_events", "NewEvents"),
        ("about_to_wait", "AboutToWait"),
    ] {
        let body = method_in(handlers, name);
        let enter = body.find(&format!("self.outer_enter(OuterKind::{kind});")).expect(name);
        let turn = body.find(&format!("self.{name}_turn(")).expect(name);
        let exit = body.find("self.outer_exit();").expect(name);
        assert!(enter < turn && turn < exit, "{name}: enter, body, exit");
    }
    let forward: String =
        method("forward").chars().filter(|glyph| !glyph.is_whitespace()).collect();
    let entered =
        forward.find("sample.timeline.enter_invocation(invocation,started);").expect("entry");
    let dispatched = forward.find("dispatch(&mutself.app,event_loop);").expect("dispatch");
    let returned = forward.find("sample.timeline.exit_invocation(ended);").expect("return");
    assert!(entered < dispatched && dispatched < returned);
}

/// The production handoff, through `attribute`: a controlled record whose App return precedes the
/// credited forward's end binds its unique pair, keeps the original latency, and decomposes into the
/// seven parts and both tail sub-parts, with a nonzero App-return-to-credited-end gap. It enters at
/// `attribute` with the forward's `started` and `ended`; it is not a run of `Probe::forward` or of a
/// native presenting dispatch, which has no headless source.
#[test]
fn the_production_handoff_credits_the_forward_and_its_end() {
    use crate::record::{
        AppearanceFacts, Delivery, DeliveryFacts, PublicationFacts, PublicationWindow, TraceFacts,
    };
    let (mut probe, pane) = typing_probe("S2", "default");
    let base = Instant::now();
    let (record, credited) = crate::timeline::fixture_at(base);
    assert_eq!(probe.open_typing_sample(|| credited.injected), Some('a'));
    let at = |offset_ns: u64| base + Duration::from_nanos(offset_ns);
    let facts = TraceFacts {
        shown: true,
        identity_changed: false,
        pre_present: false,
        lost: false,
        appearance: Some(AppearanceFacts { generation: 5, parsed_at: at(200), sync_open: false }),
        publication: Some(PublicationFacts {
            published_at: at(500),
            window: PublicationWindow::Main,
            coalesced: false,
        }),
        delivery: Some(DeliveryFacts { outcome: Delivery::Sent, decided_at: at(510) }),
    };
    probe.test_take = Some((EchoOutcome::Taken(facts), TimelineTake::Recorded(record)));
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"a");
    let snapshot = probe.echo_snapshot();
    let observation =
        DispatchObservation { before: snapshot, after: probe.echo_snapshot(), advanced: true };
    probe.attribute(&observation, credited.forward_started, credited.ended);
    let sample = probe.samples.last().expect("a credited sample");
    assert_eq!(sample.latency_ms, Some(0.0014), "injection to the credited end, 1400 ns");
    assert_eq!(sample.split_reason, crate::record::SPLIT);
    let timeline = sample.echo_timeline.expect("an in-scope credited sample has a timeline");
    assert_eq!(timeline.ordering, Some(crate::timeline::OrderingStatus::Ordered));
    assert_eq!(timeline.credited_dispatch_seq, Some(1), "the unique pair inside the forward");
    let parts = timeline.parts.expect("ordered");
    assert_eq!(parts.additive_ns.iter().sum::<u64>(), 1400);
    assert_eq!(
        parts.render_exit_to_dispatch_return_ns + parts.dispatch_return_to_credited_end_ns,
        parts.additive_ns[6]
    );
    assert_eq!(parts.dispatch_return_to_credited_end_ns, 100, "App return 1400, credited end 1500");
}

/// Two fixtures with one label get distinct directories, and each is removed when dropped, also on unwind.
#[test]
fn guard_probe_scratch_is_unique_and_removed_even_on_unwind() {
    let first = ScratchDir::new("media-free");
    let second = ScratchDir::new("media-free");
    assert_ne!(first.path, second.path, "two fixtures with one label share no directory");
    let (first_path, second_path) = (first.path.clone(), second.path.clone());
    drop(first);
    assert!(!first_path.exists(), "removed when dropped");
    let unwound = std::panic::catch_unwind(move || {
        let held = second;
        std::fs::write(held.path.join("progress.json"), b"{}").unwrap();
        panic!("the test unwinds while it holds the directory");
    });
    assert!(unwound.is_err());
    assert!(!second_path.exists(), "removed while unwinding");
}

/// H1b at runtime: startup's finalization transports phase 0 exactly once, after its counter end was latched,
/// and a later run finalization with no open meter transports nothing more.
#[test]
fn startup_finalization_transports_once_after_its_endpoints() {
    let act = Instant::now();
    let (mut probe, _phase, calls, _scratch) = guard_probe("media-free", act);
    probe.finalize_startup();
    {
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        let call = calls[0];
        assert_eq!(call.phase_index, 0);
        assert!(
            call.started <= call.counters_end_at && call.counters_end_at < call.transport_started,
            "{call:?}"
        );
    }
    assert!(probe.finalize_run(Status::Valid, None));
    assert_eq!(calls.borrow().len(), 1, "a run end with no open phase claims no transport");
}

/// H1 at runtime: a normal phase end latches the counter end and the phase end before its one transport.
#[test]
fn a_normal_phase_end_transports_once_after_its_endpoints() {
    let act = Instant::now();
    let (mut probe, phase, calls, _scratch) = guard_probe("media-free", act);
    assert_eq!(probe.finalize_phase(&phase), None);
    let calls = calls.borrow();
    assert_eq!(calls.len(), 1);
    let ended_at = probe.phase_ends.last().unwrap().1;
    let call = calls[0];
    assert!(
        call.counters_end_at < ended_at && ended_at < call.transport_started,
        "{call:?} ended {ended_at:?}"
    );
}

/// H1c at runtime: a phase end that finds a delivery problem returns before its transport, and the finish it
/// hands off to transports that phase exactly once; a second finish changes nothing.
#[test]
fn an_early_phase_end_hands_its_one_transport_to_finish() {
    let act = Instant::now();
    let (mut probe, phase, calls, _scratch) = guard_probe("reshow", act);
    probe.forced_delivery_problem = Some("ConPTY dropped a line".to_owned());
    assert_eq!(probe.finalize_phase(&phase).as_deref(), Some("ConPTY dropped a line"));
    assert!(calls.borrow().is_empty(), "the phase end itself does not transport");
    assert!(probe.finalize_run(Status::Blocked, Some("ConPTY dropped a line".to_owned())));
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(calls.borrow()[0].phase_index, 0);
    assert!(!probe.finalize_run(Status::Blocked, None));
    assert_eq!(calls.borrow().len(), 1, "exactly one transport");
}

#[test]
fn a_status_line_drag_goes_quiet_before_its_phase_ends() {
    // A lagging drag would still be moving at the deadline, so its last selection change could
    // present in `selected`; in the quiet tail it only releases the held press, then stops.
    let deadline = Instant::now();
    let quiet_from = deadline - STATUS_LINE_DRAG_SETTLE;
    let mut drag = lagging_drag(deadline, Some(quiet_from));
    assert_eq!(drag.next_motion(quiet_from), Some(DragMotion::Release));
    assert!(!drag.pressed && drag.finished);
    assert_eq!(drag.next_motion(deadline), None, "nothing changes the selection after the release");
    // Without a quiet tail (S6), the same lagging drag keeps its schedule and moves.
    let mut scheduled = lagging_drag(deadline, None);
    assert!(matches!(scheduled.next_motion(quiet_from), Some(DragMotion::Move(_))));
}

#[test]
fn split_dispatch_medians_hold_when_the_presenting_share_crosses_half() {
    // The pooled median jumps between the cheap and the presenting cluster when the presenting share
    // crosses 50%; the medians recorded per cluster do not move.
    let mostly_cheap = meter_with_mix(49);
    let mostly_presenting = meter_with_mix(51);
    assert!(median_of(&mostly_cheap.dispatch_ms) < 0.2, "pooled: the cheap cluster");
    assert!(median_of(&mostly_presenting.dispatch_ms) > 0.9, "pooled: the presenting cluster");
    for record in [&mostly_cheap, &mostly_presenting] {
        assert!((median_of(&record.dispatch_presenting_ms) - 1.0).abs() < 1e-9);
        assert!((median_of(&record.dispatch_nonpresenting_ms) - 0.1).abs() < 1e-9);
    }
    assert_eq!(mostly_presenting.dispatch_presenting_ms.len(), 51);
}

#[test]
fn output_published_while_go_is_released_still_starts_the_output_diagnostic() {
    // A role can print, and its worker publish the batch, before the probe resumes from releasing GO.
    // The first-output baseline must predate that output, so the batch is still noted as the first output
    // and the frame drawing it times the output-to-present diagnostic.
    let (mut probe, pane) = typing_probe("S8", "default");
    probe.role_panes = vec![pane];
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe
        .release_roles(true, |released, _role| {
            // The released role printed and its worker published the batch before the probe resumed.
            let _ = released.app.__test_publish_pane_output(main, pane, b"printed\r\n");
            Ok(())
        })
        .expect("every role released");
    assert!(probe.go_cursors.iter().all(Option::is_some), "the role pane has a baseline");
    let mut meter = PhaseMeter::start(
        "print",
        crate::scenarios::PhaseKind::Transition,
        None,
        false,
        None,
        None,
    );
    meter.present.await_output(0.0);
    probe.meter = Some(meter);
    // The batch's output event reaches the probe once it resumes; the frame drawing it presents next.
    probe.note_first_output();
    let mut meter = probe.meter.take().expect("the print meter");
    let started = Instant::now();
    meter.observe_dispatch(
        Dispatch::Redraw,
        started,
        started + Duration::from_millis(1),
        (0, 1),
        None,
    );
    let record = meter.finish(None, crate::transition::Completion::default());
    assert!(record.first_present_ms.is_some(), "the output's frame is the first present");
    assert_eq!(record.present_missing, None);
    assert!(record.go_to_first_output_ms.is_some(), "the output is noted");
    assert!(record.first_output_to_present_ms.is_some(), "its frame times the diagnostic");
}

#[cfg(unix)]
#[test]
fn the_go_fifos_are_removed_when_the_run_ends() {
    // GO releases POSIX roles through FIFOs; once `run` returns, on any path, none is left in the scratch,
    // so nothing that later copies, archives or hashes the scratch meets a named pipe.
    let scratch = std::env::temp_dir().join(format!("sonicterm-go-fifos-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("go")).expect("go directory");
    let made = std::process::Command::new("mkfifo").arg(scratch.join("go/0")).status();
    assert!(made.is_ok_and(|status| status.success()), "mkfifo");
    std::fs::write(scratch.join("go/1"), b"").expect("plain marker");
    drop(GoFifos { scratch: scratch.clone(), roles: 2 });
    assert!(std::fs::symlink_metadata(scratch.join("go/0")).is_err(), "the FIFO is removed");
    assert!(scratch.join("go/1").is_file(), "a plain marker is left alone");
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_print_phase_whose_only_frame_precedes_the_output_records_its_first_present() {
    // Replays a base run the validator rejected: the phase's only frame presents before the roles' first
    // output, then the output lands and the scan sees the sentinel before another frame or output event.
    // The first present is that frame, the first after GO; the output diagnostics stay empty.
    let (mut probe, pane) = typing_probe("S8", "default");
    probe.role_panes = vec![pane];
    probe.release_roles(true, |_, _| Ok(())).expect("every role released");
    open_print_meter(&mut probe);
    // The frame's redraw arrives while no role has printed yet.
    probe.note_first_output();
    present_one_frame(&mut probe, 1);
    let main = sonicterm_app::app::synthetic_main_window_id();
    let _ = probe.app.__test_publish_pane_output(main, pane, b"printed\r\n");
    let record = probe.meter.take().expect("the print meter").finish(None, Default::default());
    assert!(record.first_present_ms.is_some(), "the frame after GO is the first present");
    assert_eq!(record.present_missing, None, "so the validator's trace is complete");
    let json = serde_json::to_value(&record).expect("serializable");
    for key in ["go_to_first_output_ms", "first_output_to_present_ms"] {
        assert!(json.get(key).is_none(), "{key} is absent, never null or zero");
    }
}

#[test]
fn output_landing_between_the_check_and_the_frame_still_records_the_first_present() {
    // The redraw's cursor check runs before the App collects the frame, so output can land after the check
    // and still be drawn. The frame is the first present; the output is noted only at the event after it,
    // so no frame follows it and only its GO-to-output time is recorded.
    let (mut probe, pane) = typing_probe("S8", "default");
    probe.role_panes = vec![pane];
    probe.release_roles(true, |_, _| Ok(())).expect("every role released");
    open_print_meter(&mut probe);
    probe.note_first_output();
    let main = sonicterm_app::app::synthetic_main_window_id();
    let _ = probe.app.__test_publish_pane_output(main, pane, b"printed\r\n");
    present_one_frame(&mut probe, 1);
    // The batch's output event reaches the probe after the frame; then the scan sees the sentinel.
    probe.note_first_output();
    let record = probe.meter.take().expect("the print meter").finish(None, Default::default());
    assert!(record.first_present_ms.is_some(), "the frame drawing the output is the first present");
    assert_eq!(record.present_missing, None);
    let json = serde_json::to_value(&record).expect("serializable");
    assert!(json.get("go_to_first_output_ms").is_some_and(serde_json::Value::is_number));
    assert!(json.get("first_output_to_present_ms").is_none(), "no frame followed the noted output");
}
