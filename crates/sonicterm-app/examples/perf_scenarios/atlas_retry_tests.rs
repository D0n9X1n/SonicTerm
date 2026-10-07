use super::*;

/// The counts of one attempt that fits `frame`, at atlas dimension 2048.
fn fitting(frame: Frame) -> Counts {
    let (presented, resets) = if frame == Frame::Retried { (0, 1) } else { (1, 0) };
    Counts { attempts: 1, presented, resets, hits: 0, misses: 0, shapes: 0, atlas_dim: 2048 }
}

/// The planned scene: fixture rows with the sentinel and a prompt below, nothing drawn missing.
fn fixture_scene() -> Scene {
    let mut rows: Vec<String> = fixture_text().lines().skip(2).map(str::to_owned).collect();
    rows.push("sentinel".to_owned());
    rows.push(String::new());
    Scene {
        title: "sonicterm".to_owned(),
        fallback: (1, 0, 0),
        grid: (281, 70),
        cursor: (69, 0),
        rows,
    }
}

/// The instant the fixtures' applied samples are stamped from, shared so samples built by separate
/// helpers compare consistently.
fn origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// A dispatch that leaves `scene` as it found it, with the final process applied from two samples taken
/// after the barrier's T, so a `fresh` machine's barrier is met.
fn around(scene: Scene) -> SceneReading {
    SceneReading {
        before: Some(scene.clone()),
        after: Some(scene),
        cached_before: Some(sample(origin(), 1, Some(FINAL_PROCESS))),
        cached_after: Some(sample(origin(), 2, Some(FINAL_PROCESS))),
    }
}

/// A barrier whose final process was observed at `final_at`, expecting the title `title`.
fn barrier(final_at: Instant, title: &str) -> Barrier {
    Barrier {
        final_at: Some(final_at),
        final_name: FINAL_PROCESS.to_owned(),
        expected_title: Some(title.to_owned()),
        fixture_problem: None,
    }
}

/// A machine settling from `now` whose barrier the fixture scene meets: T at the samples' origin, and the
/// fixture's title expected.
fn fresh(now: Instant) -> RecoveryEpisodes {
    let mut machine = RecoveryEpisodes::new(now);
    machine.set_barrier(barrier(origin(), "sonicterm"));
    machine
}

/// A dispatch that leaves the planned scene unchanged.
fn same() -> SceneReading {
    around(fixture_scene())
}

/// A steady settling frame.
fn steady() -> Counts {
    fitting(Frame::Reused)
}

/// A machine already through its settle, at `now`.
fn settled(now: Instant) -> RecoveryEpisodes {
    let mut machine = fresh(now);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &same(), now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &same(), now), Progress::Arm(Arm::ChangeAtlas));
    assert!(machine.settled());
    machine
}

/// A deferred dispatch attempts nothing, so neither the settle nor a step advances.
#[test]
fn a_dispatch_without_an_attempt_advances_nothing() {
    let now = Instant::now();
    let mut machine = fresh(now);
    assert_eq!(machine.observe(Some(Counts::default()), &same(), now), Progress::Waiting);
    assert!(!machine.settled());
    let mut machine = settled(now);
    assert_eq!(machine.observe(Some(Counts::default()), &same(), now), Progress::Waiting);
    assert!(machine.records().is_empty());
}

/// A non-steady frame restarts the settle count; three steady frames in a row settle the scene.
#[test]
fn only_consecutive_steady_frames_settle_the_scene() {
    let now = Instant::now();
    let mut machine = fresh(now);
    machine.observe(Some(steady()), &same(), now);
    machine.observe(Some(steady()), &same(), now);
    let missed = Counts { misses: 3, ..steady() };
    assert_eq!(machine.observe(Some(missed), &same(), now), Progress::Arm(Arm::Redraw));
    machine.observe(Some(steady()), &same(), now);
    machine.observe(Some(steady()), &same(), now);
    assert!(!machine.settled(), "the miss restarted the count");
    assert_eq!(machine.observe(Some(steady()), &same(), now), Progress::Arm(Arm::ChangeAtlas));
}

/// A normal run records eight episodes of A, B, C, D in order, arming the change for A, the
/// invalidation only for B and a redraw for C and D, and its records pass validation.
#[test]
fn eight_normal_episodes_complete_and_validate() {
    let now = Instant::now();
    let mut machine = settled(now);
    for episode in 0..EPISODES {
        for frame in [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated] {
            let progress = machine.observe(Some(fitting(frame)), &same(), now);
            let expected = match (episode, frame) {
                (_, Frame::Retried) => Progress::Arm(Arm::InvalidateOnly),
                (_, Frame::Recovered | Frame::Reused) => Progress::Arm(Arm::Redraw),
                (last, Frame::Repeated) if last == EPISODES - 1 => Progress::Done,
                (_, Frame::Repeated) => Progress::Arm(Arm::ChangeAtlas),
            };
            assert_eq!(progress, expected, "episode {episode} {}", frame.as_str());
        }
    }
    assert!(machine.is_done());
    assert_eq!(machine.records().len(), EPISODES * 4);
    assert_eq!(records_problem(machine.records()), None);
    let json = recovery_json(machine.records(), 71);
    assert_eq!(json["episodes"], 8);
    assert_eq!(json["distinct_keys"], 71);
    assert_eq!(json["records"].as_array().map(Vec::len), Some(32));
    assert_eq!(json["records"][1]["frame"], "B");
}

/// A dispatch with two attempts is never folded into one step.
#[test]
fn an_extra_attempt_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    let doubled = Counts { attempts: 2, ..fitting(Frame::Retried) };
    assert!(
        matches!(machine.observe(Some(doubled), &same(), now), Progress::Invalid(reason) if reason.contains("2 attempts"))
    );
    assert_eq!(
        machine.observe(Some(fitting(Frame::Retried)), &same(), now),
        Progress::Waiting,
        "an ended run ignores later frames"
    );
}

/// A that presents, A whose injected change reset nothing, or C that resets, does not fit its frame.
#[test]
fn a_frame_whose_counts_do_not_fit_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    let presented_a = Counts { presented: 1, ..fitting(Frame::Retried) };
    assert!(
        matches!(machine.observe(Some(presented_a), &same(), now), Progress::Invalid(reason) if reason.contains("episode 0 A"))
    );
    // An A that neither presents nor resets is a deferral-like attempt the change never reached,
    // so it is no retry and must not be recorded as one.
    let mut machine = settled(now);
    let unreset_a = Counts { resets: 0, ..fitting(Frame::Retried) };
    assert!(
        matches!(machine.observe(Some(unreset_a), &same(), now), Progress::Invalid(reason) if reason.contains("episode 0 A"))
    );
    let mut machine = settled(now);
    machine.observe(Some(fitting(Frame::Retried)), &same(), now);
    machine.observe(Some(fitting(Frame::Recovered)), &same(), now);
    let reset_c = Counts { resets: 1, ..fitting(Frame::Reused) };
    assert!(
        matches!(machine.observe(Some(reset_c), &same(), now), Progress::Invalid(reason) if reason.contains("episode 0 C"))
    );
}

/// A recovered frame at another atlas dimension is invalid.
#[test]
fn a_recovered_frame_at_another_atlas_dimension_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    machine.observe(Some(fitting(Frame::Retried)), &same(), now);
    machine.observe(Some(fitting(Frame::Recovered)), &same(), now);
    let grown = Counts { atlas_dim: 4096, ..fitting(Frame::Reused) };
    assert!(
        matches!(machine.observe(Some(grown), &same(), now), Progress::Invalid(reason) if reason.contains("dimension"))
    );
}

/// A step not attempted within its bound ends the run; before the bound it does not.
#[test]
fn a_step_past_its_bound_times_out() {
    let now = Instant::now();
    let mut machine = settled(now);
    assert_eq!(machine.deadline(), Some(now + STEP_BOUND));
    assert_eq!(machine.expire(now + STEP_BOUND - Duration::from_millis(1)), None);
    let reason = machine.expire(now + STEP_BOUND).expect("timed out");
    assert!(reason.contains("episode 0 A"), "{reason}");
    assert_eq!(machine.deadline(), None);
    let mut settling = fresh(now);
    assert!(settling.expire(now + SETTLE_BOUND).expect("settle timed out").contains("settle"));
}

/// A missing counter field makes the run unreadable, never a zero.
#[test]
fn a_missing_counter_field_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    assert_eq!(
        machine.observe(None, &same(), now),
        Progress::Invalid("counters unavailable".to_owned())
    );
}

/// Validation refuses a short run, a misordered frame, an extra attempt, an A without a reset and a
/// mismatched dimension.
#[test]
fn records_validation_refuses_every_malformed_run() {
    let now = Instant::now();
    let mut machine = settled(now);
    for _ in 0..EPISODES {
        for frame in [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated] {
            machine.observe(Some(fitting(frame)), &same(), now);
        }
    }
    let good = machine.records().to_vec();
    assert!(records_problem(&good[..31]).is_some());
    let mut swapped = good.clone();
    swapped.swap(1, 2);
    assert!(records_problem(&swapped).is_some());
    let mut doubled = good.clone();
    doubled[5].counts.attempts = 2;
    assert!(records_problem(&doubled).is_some());
    let mut unreset = good.clone();
    unreset[8].counts.resets = 0;
    assert!(records_problem(&unreset).is_some(), "an A without a reset is refused");
    let mut grown = good;
    grown[30].counts.atlas_dim = 4096;
    assert!(records_problem(&grown).is_some());
}

/// The fixture is 70 distinct numbered lines; a scene of them, with a sentinel and a prompt below,
/// passes, while a scene missing fixture lines, out of order or with one line's text replaced
/// does not.
#[test]
fn the_scene_check_accepts_the_fixture_and_nothing_else() {
    let lines: Vec<String> = fixture_text().lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 70);
    assert_eq!(distinct_keys(&lines), 70);
    let mut scene: Vec<String> = lines[2..].to_vec();
    scene.push("sentinel".to_owned());
    scene.push(String::new());
    assert_eq!(scene_problem(&scene), None);
    let mut short = scene.clone();
    short[5] = "unrelated".to_owned();
    short[6] = "unrelated".to_owned();
    short[7] = "unrelated".to_owned();
    assert!(scene_problem(&short).is_some());
    let mut misordered = scene.clone();
    misordered.swap(3, 4);
    assert!(scene_problem(&misordered).is_some());
    // A row with the fixture's prefix and number but other text is not a fixture line.
    let mut replaced = scene;
    replaced[10] = format!("{ROW_PREFIX}13 ZZZZZZZZZZZZZZZZZZZZZZZZZZ 13");
    assert!(scene_problem(&replaced).is_some());
}

/// A frame completing exactly at its step's deadline is late: `observe` ends the run before reading
/// its counts, so it never installs the next step's deadline. One instant earlier it is on time.
#[test]
fn a_frame_completing_at_or_past_its_deadline_is_refused_through_observe() {
    let now = Instant::now();
    let mut machine = settled(now);
    let on_time = now + STEP_BOUND - Duration::from_millis(1);
    assert_eq!(
        machine.observe(Some(fitting(Frame::Retried)), &same(), on_time),
        Progress::Arm(Arm::InvalidateOnly)
    );
    let at_deadline = on_time + STEP_BOUND;
    let late = machine.observe(Some(fitting(Frame::Recovered)), &same(), at_deadline);
    assert!(
        matches!(&late, Progress::Invalid(reason) if reason.contains("episode 0 B") && reason.contains("past its bound")),
        "{late:?}"
    );
    assert_eq!(machine.deadline(), None, "the late frame installed no deadline");
    assert_eq!(machine.records().len(), 1, "the late frame is not recorded");
    let mut overdue = settled(now);
    let past = overdue.observe(Some(fitting(Frame::Retried)), &same(), now + STEP_BOUND * 2);
    assert!(matches!(past, Progress::Invalid(_)), "a frame past the deadline is late");
    assert!(overdue.records().is_empty());
}

/// The final settling frame is judged the same way against the settle bound.
#[test]
fn the_final_settling_frame_is_refused_at_the_settle_bound() {
    let now = Instant::now();
    for (completed, on_time) in
        [(now + SETTLE_BOUND - Duration::from_millis(1), true), (now + SETTLE_BOUND, false)]
    {
        let mut machine = fresh(now);
        for _ in 0..STEADY_FRAMES - 1 {
            machine.observe(Some(steady()), &same(), now);
        }
        let last = machine.observe(Some(steady()), &same(), completed);
        if on_time {
            assert_eq!(last, Progress::Arm(Arm::ChangeAtlas));
        } else {
            assert!(
                matches!(&last, Progress::Invalid(reason) if reason.contains("settle")),
                "{last:?}"
            );
            assert!(machine.scene().is_none(), "the late settle recorded no scene");
        }
    }
}

/// A font fallback applied between B and C (the applies count moves) changes the settled scene, so
/// C ends the run.
#[test]
fn a_fallback_applied_between_b_and_c_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    machine.observe(Some(fitting(Frame::Retried)), &same(), now);
    machine.observe(Some(fitting(Frame::Recovered)), &same(), now);
    let applied = Scene { fallback: (1, 1, 0), ..fixture_scene() };
    let reading = SceneReading {
        before: Some(fixture_scene()),
        after: Some(applied),
        ..SceneReading::default()
    };
    let progress = machine.observe(Some(fitting(Frame::Reused)), &reading, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("font fallback")),
        "{progress:?}"
    );
}

/// A title change during the episodes ends the run.
#[test]
fn a_title_change_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    let retitled = around(Scene { title: "vim".to_owned(), ..fixture_scene() });
    let progress = machine.observe(Some(fitting(Frame::Retried)), &retitled, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("title")),
        "{progress:?}"
    );
}

/// Text replaced with text of the same length and count ends the run: the rows are compared exactly.
#[test]
fn a_same_count_text_replacement_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    let mut scene = fixture_scene();
    scene.rows[10] =
        scene.rows[10].replace("abcdefghijklmnopqrstuvwxyz", "ZYXWVUTSRQPONMLKJIHGFEDCBA");
    let progress = machine.observe(Some(fitting(Frame::Retried)), &around(scene), now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("row text")),
        "{progress:?}"
    );
}

/// A transient change is caught where it is seen, before it can revert: a dispatch that attempts no
/// frame but leaves the scene changed, and a dispatch whose scene changed before it and reverts in it.
#[test]
fn a_transient_change_is_invalid_at_the_dispatch_that_shows_it() {
    let now = Instant::now();
    let changed = Scene { cursor: (0, 5), ..fixture_scene() };
    let mut machine = settled(now);
    let reading = SceneReading {
        before: Some(fixture_scene()),
        after: Some(changed.clone()),
        ..SceneReading::default()
    };
    let progress = machine.observe(Some(Counts::default()), &reading, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("cursor")),
        "{progress:?}"
    );
    let mut machine = settled(now);
    let reverting = SceneReading {
        before: Some(changed),
        after: Some(fixture_scene()),
        ..SceneReading::default()
    };
    let progress = machine.observe(Some(fitting(Frame::Retried)), &reverting, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("cursor")),
        "{progress:?}"
    );
}

/// Settling needs consecutive steady frames of one scene: a scene that changes mid-count restarts
/// it, a scene whose last frame drew characters missing (fallback outstanding) or a non-fixture
/// scene never settles, and an unreadable scene ends the run.
#[test]
fn settling_requires_one_applied_fixture_scene() {
    let now = Instant::now();
    let mut machine = fresh(now);
    machine.observe(Some(steady()), &same(), now);
    machine.observe(Some(steady()), &same(), now);
    // The title is the barrier's, so the change here is the cursor's.
    let moved = around(Scene { cursor: (68, 4), ..fixture_scene() });
    // The frame that shows the change is not steady; three steady frames of the new scene follow.
    assert_eq!(machine.observe(Some(steady()), &moved, now), Progress::Arm(Arm::Redraw));
    assert!(!machine.settled(), "the change restarted the count");
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &moved, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &moved, now), Progress::Arm(Arm::ChangeAtlas));
    assert_eq!(machine.scene().map(|scene| scene.cursor), Some((68, 4)));

    let pending = around(Scene { fallback: (1, 0, 2), ..fixture_scene() });
    let mut machine = fresh(now);
    for _ in 0..STEADY_FRAMES + 2 {
        assert_eq!(machine.observe(Some(steady()), &pending, now), Progress::Arm(Arm::Redraw));
    }
    let mut stranger = fixture_scene();
    stranger.rows[0] = "unrelated".to_owned();
    stranger.rows[1] = "unrelated".to_owned();
    stranger.rows[2] = "unrelated".to_owned();
    let mut machine = fresh(now);
    for _ in 0..STEADY_FRAMES + 2 {
        assert_eq!(
            machine.observe(Some(steady()), &around(stranger.clone()), now),
            Progress::Arm(Arm::Redraw)
        );
    }
    let mut machine = fresh(now);
    let unread =
        SceneReading { before: Some(fixture_scene()), after: None, ..SceneReading::default() };
    let progress = machine.observe(Some(steady()), &unread, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("cannot be read")),
        "{progress:?}"
    );
}

/// After settling, a frame that draws a character missing, or a replaced fallback notice, changes
/// the fallback state the scene recorded, so the run ends naming it.
#[test]
fn a_missing_character_or_a_new_notice_after_settling_is_invalid() {
    let now = Instant::now();
    for fallback in [(1, 0, 1), (2, 0, 0)] {
        let mut machine = settled(now);
        let changed = SceneReading {
            before: Some(fixture_scene()),
            after: Some(Scene { fallback, ..fixture_scene() }),
            ..SceneReading::default()
        };
        let progress = machine.observe(Some(fitting(Frame::Retried)), &changed, now);
        assert!(
            matches!(&progress, Progress::Invalid(reason) if reason.contains("font fallback")),
            "{fallback:?}: {progress:?}"
        );
    }
}

/// A title change after settling names both titles, escaped to ASCII, so a failure records what the
/// title was and what it became; a cursor change names both positions.
#[test]
fn a_title_change_reason_names_both_titles() {
    let now = Instant::now();
    let mut machine = settled(now);
    let retitled = around(Scene { title: "#1 \u{F489} shell".to_owned(), ..fixture_scene() });
    let progress = machine.observe(Some(fitting(Frame::Retried)), &retitled, now);
    let Progress::Invalid(reason) = progress else { panic!("{progress:?}") };
    assert!(
        reason.ends_with(r##"the scene's title changed from "sonicterm" to "#1 \u{f489} shell""##),
        "{reason}"
    );
    let mut machine = settled(now);
    let moved = around(Scene { cursor: (0, 5), ..fixture_scene() });
    let Progress::Invalid(reason) = machine.observe(Some(fitting(Frame::Retried)), &moved, now)
    else {
        panic!("a moved cursor must end the run")
    };
    assert!(reason.ends_with("the scene's cursor changed from (69, 0) to (0, 5)"), "{reason}");
}

/// Escaping keeps a log on one ASCII line: quotes and backslashes are escaped, and anything outside
/// printable ASCII reads as its code point.
#[test]
fn escaped_text_is_one_ascii_line() {
    assert_eq!(escape_text("#1 \u{E691} \"sh\"\\\n"), r#"#1 \u{e691} \"sh\"\\\u{a}"#);
    assert_eq!(escape_text("plain shell"), "plain shell");
}

/// Relative times keep their sign: a sample taken before the driver started reads negative, so a
/// stale sample is visible as one.
#[test]
fn relative_times_keep_their_sign() {
    let origin = Instant::now() + Duration::from_secs(5);
    assert_eq!(relative_ms(origin + Duration::from_millis(1_250), origin), 1_250);
    assert_eq!(relative_ms(origin - Duration::from_millis(40), origin), -40);
    assert_eq!(relative_ms(origin, origin), 0);
}

/// The settle and failure line keeps the App's applied sample and the harness's own lookup apart, each
/// with its own time, and reads an absent value as `n/a` and a cleared sample as `none`.
#[test]
fn the_settle_log_separates_the_apps_cached_sample_from_the_harness_lookup() {
    let evidence = Evidence {
        event: "failure",
        observation: "before",
        settled_title: Some("#1 \u{E760} shell".to_owned()),
        read_title: Some("#1 \u{F489} shell".to_owned()),
        app_cached: Some(CachedForeground {
            process: Some("sleep".to_owned()),
            sampled_rel_ms: 2_310,
        }),
        session: Some(SessionIdentity { leader_pid: 501, anchor_pid: Some(502) }),
        tab_title_prepares: Some(3),
        fg_worker_probes: Some(2),
        fg_results_stale: Some(0),
        settle_ms: None,
        harness_lookup: Some(HarnessLookup {
            process: Some("sleep".to_owned()),
            observed_rel_ms: 2_400,
        }),
        final_lookups: Some(7),
        final_lookup_us: Some(4_310),
        final_at_rel_ms: Some(1_950),
    };
    assert_eq!(
        evidence.line(),
        concat!(
            r##"event=failure observation=before settled_title="#1 \u{e760} shell" read_title="#1 \u{f489} shell" "##,
            r#"app_cached_process="sleep" app_cached_sampled_rel_ms=2310 leader_pid=501 anchor_pid=502 "#,
            "tab_title_prepares=3 fg_worker_probes=2 fg_results_stale=0 settle_ms=n/a ",
            r#"harness_lookup_process="sleep" harness_lookup_rel_ms=2400 "#,
            "final_lookups=7 final_lookup_us=4310 final_at_rel_ms=1950",
        )
    );
    let sparse = Evidence {
        event: "settle",
        observation: "none",
        settled_title: None,
        read_title: None,
        app_cached: Some(CachedForeground { process: None, sampled_rel_ms: -15 }),
        session: None,
        tab_title_prepares: None,
        fg_worker_probes: None,
        fg_results_stale: None,
        settle_ms: Some(812),
        harness_lookup: None,
        final_lookups: None,
        final_lookup_us: None,
        final_at_rel_ms: None,
    };
    assert_eq!(
        sparse.line(),
        concat!(
            "event=settle observation=none settled_title=n/a read_title=n/a app_cached_process=none ",
            "app_cached_sampled_rel_ms=-15 leader_pid=n/a anchor_pid=n/a tab_title_prepares=n/a ",
            "fg_worker_probes=n/a fg_results_stale=n/a settle_ms=812 harness_lookup_process=n/a ",
            "harness_lookup_rel_ms=n/a final_lookups=n/a final_lookup_us=n/a final_at_rel_ms=n/a",
        )
    );
}

/// The leader identity comes from the role session on either host: the macOS script's `leader_pid` and
/// `anchor_pid`, or the Windows program's `program_pid`. A missing, empty, zero, negative or
/// non-numeric pid, or text that is not JSON, gives no identity rather than a guessed one.
#[test]
fn the_leader_identity_is_read_from_the_role_session() {
    assert_eq!(
        parse_session(r#"{"role":0,"leader_pid":501,"anchor_pid":502,"tty":"/dev/ttys004"}"#),
        Some(SessionIdentity { leader_pid: 501, anchor_pid: Some(502) })
    );
    assert_eq!(
        parse_session(r#"{"role":0,"program_pid":7716,"tty":"none"}"#),
        Some(SessionIdentity { leader_pid: 7716, anchor_pid: None })
    );
    for refused in [
        r#"{"role":0,"tty":"none"}"#,
        r#"{"role":0,"leader_pid":"","anchor_pid":502}"#,
        r#"{"role":0,"leader_pid":0}"#,
        r#"{"role":0,"leader_pid":-4}"#,
        r#"{"role":0,"leader_pid":"501"}"#,
        r#"{"role":0,"leader_pid":4294967296}"#,
        "not json",
    ] {
        assert_eq!(parse_session(refused), None, "{refused}");
    }
}

/// A foreground sample the App applied, `offset_ms` after `origin`, naming `process`.
fn sample(origin: Instant, offset_ms: u64, process: Option<&str>) -> AppliedSample {
    AppliedSample {
        sampled_at: origin + Duration::from_millis(offset_ms),
        process: process.map(str::to_owned),
    }
}

/// A dispatch whose before reading shows a changed title that its after reading has reverted is refused
/// for the before reading, and the machine keeps that reading: its title and the sample captured with it,
/// not the reverted after side.
#[test]
fn a_reverted_title_is_kept_as_the_before_reading_that_showed_it() {
    let now = Instant::now();
    let mut machine = settled(now);
    let reading = SceneReading {
        before: Some(Scene { title: "T2".to_owned(), ..fixture_scene() }),
        after: Some(fixture_scene()),
        cached_before: Some(sample(now, 40, Some("bash"))),
        cached_after: Some(sample(now, 900, Some("sleep"))),
    };
    let progress = machine.observe(Some(fitting(Frame::Retried)), &reading, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("\"T2\"")),
        "{progress:?}"
    );
    let observed = failure_observation(&machine);
    assert_eq!(
        observed,
        Observed {
            observation: "before",
            settled_title: Some("sonicterm".to_owned()),
            read_title: Some("T2".to_owned()),
            cached: Some(sample(now, 40, Some("bash"))),
        }
    );
}

/// Once a reading is refused, later dispatches never change what the failure line reports: a foreground
/// result applied after the refusal, and a reading that shows yet another title, leave the kept reading
/// exactly as it was.
#[test]
fn a_cache_replaced_after_the_refusal_does_not_change_the_kept_reading() {
    let now = Instant::now();
    let mut machine = settled(now);
    let refused = SceneReading {
        cached_before: Some(sample(now, 40, Some("bash"))),
        cached_after: Some(sample(now, 40, Some("bash"))),
        ..around(Scene { title: "T2".to_owned(), ..fixture_scene() })
    };
    assert!(matches!(
        machine.observe(Some(fitting(Frame::Retried)), &refused, now),
        Progress::Invalid(_)
    ));
    let kept = failure_observation(&machine);
    let later = SceneReading {
        cached_before: Some(sample(now, 900, Some("sleep"))),
        cached_after: Some(sample(now, 900, Some("sleep"))),
        ..around(Scene { title: "T3".to_owned(), ..fixture_scene() })
    };
    assert_eq!(machine.observe(Some(fitting(Frame::Retried)), &later, now), Progress::Waiting);
    assert_eq!(failure_observation(&machine), kept);
    assert_eq!(kept.cached, Some(sample(now, 40, Some("bash"))));
    // Both sides show T2, and the before reading is judged first, so it is the one kept.
    assert_eq!(kept.observation, "before");
}

/// The settle line describes the after reading that settled the scene, with the sample captured alongside
/// it; a failure no scene change caused reports no reading and no sample, only the settled title.
#[test]
fn settle_and_non_scene_failures_report_only_what_was_observed() {
    let now = Instant::now();
    let reading = SceneReading { cached_after: Some(sample(now, 12, Some("sleep"))), ..same() };
    let observed = settle_observation(&fixture_scene(), &reading);
    assert_eq!(observed.observation, "after");
    assert_eq!(observed.read_title.as_deref(), Some("sonicterm"));
    assert_eq!(observed.cached, Some(sample(now, 12, Some("sleep"))));
    let mut machine = settled(now);
    let extra = Counts { attempts: 2, ..fitting(Frame::Retried) };
    assert!(matches!(machine.observe(Some(extra), &same(), now), Progress::Invalid(_)));
    assert_eq!(
        failure_observation(&machine),
        Observed {
            observation: "none",
            settled_title: Some("sonicterm".to_owned()),
            read_title: None,
            cached: None,
        }
    );
}

/// The session record is read with a bound: a real record parses, a missing file gives nothing, and a
/// file longer than the limit is refused rather than read whole.
#[test]
fn the_session_record_is_read_with_a_bound() {
    let directory =
        std::env::temp_dir().join(format!("atlas-retry-session-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("0.json");
    std::fs::write(&path, r#"{"role":0,"leader_pid":501,"anchor_pid":502,"tty":"none"}"#).unwrap();
    assert_eq!(
        read_session(&path),
        Some(SessionIdentity { leader_pid: 501, anchor_pid: Some(502) })
    );
    // Valid JSON exactly one byte over the limit: the bounded read takes it whole, so only the length
    // check can refuse it.
    let head = r#"{"role":0,"leader_pid":501,"pad":""#;
    let pad = SESSION_RECORD_LIMIT as usize + 1 - head.len() - 2;
    let padded = format!("{head}{}\"}}", "x".repeat(pad));
    assert_eq!(padded.len() as u64, SESSION_RECORD_LIMIT + 1);
    std::fs::write(&path, padded).unwrap();
    assert_eq!(read_session(&path), None);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(read_session(&path), None);
    std::fs::remove_dir(&directory).unwrap();
}

/// The final process the macOS role script execs.
const FINAL_PROCESS: &str = "sleep";
/// The title the stale shell sample gives: Stage 1 recorded the shell cached as `bash`, drawing E760.
const STALE_TITLE: &str = "#1 \u{E760} shell";
/// The title once the final process is applied.
const FINAL_TITLE: &str = "#1 \u{F489} shell";

/// The fixture scene titled `title`.
fn titled(title: &str) -> Scene {
    Scene { title: title.to_owned(), ..fixture_scene() }
}

/// A dispatch that leaves the scene titled `title`, with `before` and `after` applied on each side.
fn reading_with(
    title: &str,
    before: Option<AppliedSample>,
    after: Option<AppliedSample>,
) -> SceneReading {
    SceneReading {
        before: Some(titled(title)),
        after: Some(titled(title)),
        cached_before: before,
        cached_after: after,
    }
}

/// A machine settling from `start` whose final process was observed at `final_at`, expecting `title`.
fn barred(start: Instant, final_at: Instant, title: &str) -> RecoveryEpisodes {
    let mut machine = RecoveryEpisodes::new(start);
    machine.set_barrier(barrier(final_at, title));
    machine
}

/// Run every episode of a settled machine on `reading` at `now`, returning the last progress.
fn run_episodes(machine: &mut RecoveryEpisodes, reading: &SceneReading, now: Instant) -> Progress {
    let mut last = Progress::Waiting;
    for _ in 0..EPISODES {
        for frame in [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated] {
            last = machine.observe(Some(fitting(frame)), reading, now);
        }
    }
    last
}

/// `start` plus `offset_ms`.
fn later(start: Instant, offset_ms: u64) -> Instant {
    start + Duration::from_millis(offset_ms)
}

/// The barrier reads the App's applied cache, not the worker's batch count: two batches may complete
/// after T while the event loop has applied neither, so the cache still holds the stale pre-T `bash`
/// sample. Its steady frames never settle, and the reason counts no qualifying observation.
#[test]
fn two_completed_batches_with_neither_result_applied_do_not_settle() {
    let start = Instant::now();
    let final_at = later(start, 1_000);
    let mut machine = barred(start, final_at, FINAL_TITLE);
    let stale = sample(start, 400, Some("bash"));
    for frame in 0..10_u64 {
        let reading = reading_with(STALE_TITLE, Some(stale.clone()), Some(stale.clone()));
        let now = later(final_at, 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    assert!(!machine.settled());
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert!(
        reason.contains(
            "barrier unmet: 0 qualifying foreground observations after the final process (need 2), \
             latest cached \"bash\""
        ),
        "{reason}"
    );
}

/// A result taken at or before T and applied after it counts nothing, and one qualifying result alone
/// does not meet the barrier.
#[test]
fn an_old_result_applied_after_the_final_process_is_insufficient() {
    let start = Instant::now();
    let final_at = later(start, 1_000);
    let mut machine = barred(start, final_at, FINAL_TITLE);
    for (frame, taken_ms) in [900_u64, 1_000].into_iter().cycle().take(6).enumerate() {
        let old = sample(start, taken_ms, Some(FINAL_PROCESS));
        let reading = reading_with(FINAL_TITLE, Some(old.clone()), Some(old));
        let now = later(final_at, 16 * frame as u64);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    assert!(!machine.settled(), "a result taken at or before T counts nothing");
    let one = sample(start, 1_200, Some(FINAL_PROCESS));
    for frame in 0..5_u64 {
        let reading = reading_with(FINAL_TITLE, Some(one.clone()), Some(one.clone()));
        let now = later(start, 1_210 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    assert!(!machine.settled());
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert!(reason.contains("1 qualifying foreground observation after"), "{reason}");
}

/// The first result stamped after T may come from an observation begun before it: stamped T+1 ms and
/// naming the stale shell, it leaves the barrier unmet until a second, later result names the final
/// process. Even a first result naming the final process is not enough on its own.
#[test]
fn an_observation_begun_before_t_and_stamped_after_it_counts_only_as_the_first() {
    let start = Instant::now();
    let final_at = later(start, 1_000);
    let mut machine = barred(start, final_at, FINAL_TITLE);
    let first = sample(start, 1_001, Some("bash"));
    for frame in 0..5_u64 {
        let reading = reading_with(STALE_TITLE, Some(first.clone()), Some(first.clone()));
        let now = later(start, 1_010 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let second = sample(start, 1_501, Some(FINAL_PROCESS));
    let met = reading_with(FINAL_TITLE, Some(second.clone()), Some(second));
    let now = later(start, 1_510);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::ChangeAtlas));
    assert_eq!(machine.scene().map(|scene| scene.title.as_str()), Some(FINAL_TITLE));

    let mut named = barred(start, final_at, FINAL_TITLE);
    let first_final = sample(start, 1_001, Some(FINAL_PROCESS));
    for frame in 0..5_u64 {
        let reading =
            reading_with(FINAL_TITLE, Some(first_final.clone()), Some(first_final.clone()));
        let now = later(start, 1_010 + 16 * frame);
        assert_eq!(named.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    assert!(!named.settled(), "one result naming the final process is still only the first");
}

/// One applied `sampled_at` after T, read at many dispatches, counts once and never meets the barrier.
#[test]
fn repeated_reads_of_one_applied_timestamp_count_once() {
    let start = Instant::now();
    let mut machine = barred(start, later(start, 1_000), FINAL_TITLE);
    let once = sample(start, 1_200, Some(FINAL_PROCESS));
    for frame in 0..20_u64 {
        let reading = reading_with(FINAL_TITLE, Some(once.clone()), Some(once.clone()));
        let now = later(start, 1_210 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert!(reason.contains("1 qualifying foreground observation after"), "{reason}");
}

/// A synchronous `(now, None)`, as an unavailable sampler or an exit caches, resets the history: one
/// qualifying result, the `None`, then one valid result leaves only one counted, so nothing settles. A
/// second valid result then meets the barrier.
#[test]
fn a_synchronous_none_then_one_valid_result_does_not_meet_the_barrier() {
    let start = Instant::now();
    let mut machine = barred(start, later(start, 1_000), FINAL_TITLE);
    let first = sample(start, 1_100, Some(FINAL_PROCESS));
    let reading = reading_with(FINAL_TITLE, Some(first.clone()), Some(first.clone()));
    assert_eq!(
        machine.observe(Some(steady()), &reading, later(start, 1_110)),
        Progress::Arm(Arm::Redraw)
    );
    let cleared = sample(start, 1_300, None);
    let reading = reading_with(FINAL_TITLE, Some(first), Some(cleared));
    assert_eq!(
        machine.observe(Some(steady()), &reading, later(start, 1_310)),
        Progress::Arm(Arm::Redraw)
    );
    let second = sample(start, 1_600, Some(FINAL_PROCESS));
    for frame in 0..5_u64 {
        let reading = reading_with(FINAL_TITLE, Some(second.clone()), Some(second.clone()));
        let now = later(start, 1_610 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    assert!(!machine.settled());
    let third = sample(start, 2_100, Some(FINAL_PROCESS));
    let met = reading_with(FINAL_TITLE, Some(third.clone()), Some(third));
    let now = later(start, 2_110);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::ChangeAtlas));
}

/// A `sampled_at` after T that is not strictly greater than the last counted one counts nothing: neither
/// the same instant read on the other side of a dispatch nor an earlier one.
#[test]
fn a_non_increasing_timestamp_does_not_count() {
    let start = Instant::now();
    let mut machine = barred(start, later(start, 1_000), FINAL_TITLE);
    let first = sample(start, 1_500, Some(FINAL_PROCESS));
    let earlier = sample(start, 1_200, Some(FINAL_PROCESS));
    for frame in 0..6_u64 {
        let reading = if frame == 0 {
            reading_with(FINAL_TITLE, Some(first.clone()), Some(first.clone()))
        } else {
            reading_with(FINAL_TITLE, Some(earlier.clone()), Some(first.clone()))
        };
        let now = later(start, 1_510 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert!(reason.contains("1 qualifying foreground observation after"), "{reason}");
}

/// Two strictly increasing qualifying results, the latest naming the final process, with the expected
/// title, then three steady frames settle, and the episodes run to `Done`. Steady frames before the
/// barrier is met never count toward the three.
#[test]
fn two_qualifying_results_then_three_steady_frames_settle() {
    let start = Instant::now();
    let mut machine = barred(start, later(start, 1_000), FINAL_TITLE);
    let before_final = sample(start, 600, Some(FINAL_PROCESS));
    for frame in 0..STEADY_FRAMES + 2 {
        let reading =
            reading_with(FINAL_TITLE, Some(before_final.clone()), Some(before_final.clone()));
        let now = later(start, 1_010 + 16 * u64::from(frame));
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let first = sample(start, 1_100, Some(FINAL_PROCESS));
    let second = sample(start, 1_400, Some(FINAL_PROCESS));
    let met = reading_with(FINAL_TITLE, Some(first), Some(second));
    let now = later(start, 1_410);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::Redraw));
        assert!(!machine.settled(), "steady frames count only once the barrier is met");
    }
    assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::ChangeAtlas));
    assert_eq!(machine.scene().map(|scene| scene.title.as_str()), Some(FINAL_TITLE));
    assert_eq!(run_episodes(&mut machine, &met, now), Progress::Done);
}

/// Without the final process applied the barrier never meets, and the attempt expires as invalid at
/// exactly `start + SETTLE_BOUND`, never earlier and never extended: no cache entry, an unavailable
/// worker's `None`, an exit's `None`, a malformed session (so macOS T is never set), and no further
/// result after T.
#[test]
fn missing_or_unavailable_foreground_data_expires_invalid_at_the_settle_bound() {
    for case in ["no entry", "unavailable", "exited", "malformed session", "no further result"] {
        let start = Instant::now();
        let mut machine = RecoveryEpisodes::new(start);
        let final_at = (case != "malformed session").then(|| later(start, 1_000));
        machine.set_barrier(Barrier { final_at, ..barrier(start, FINAL_TITLE) });
        for step in 0..18_u64 {
            let offset_ms = 1_000 + 500 * step;
            let cached = match case {
                "no entry" => None,
                "unavailable" => Some(sample(start, offset_ms, None)),
                "exited" if step == 0 => Some(sample(start, offset_ms + 1, Some(FINAL_PROCESS))),
                "exited" => Some(sample(start, offset_ms, None)),
                "malformed session" => Some(sample(start, offset_ms + 1, Some(FINAL_PROCESS))),
                _ => Some(sample(start, 1_001, Some(FINAL_PROCESS))),
            };
            let reading = reading_with(FINAL_TITLE, cached.clone(), cached);
            let now = later(start, offset_ms + 2);
            assert_eq!(
                machine.observe(Some(steady()), &reading, now),
                Progress::Arm(Arm::Redraw),
                "{case} at {offset_ms} ms"
            );
        }
        assert!(!machine.settled(), "{case}");
        assert_eq!(machine.deadline(), Some(start + SETTLE_BOUND), "{case}");
        assert_eq!(machine.expire(start + SETTLE_BOUND - Duration::from_millis(1)), None, "{case}");
        let reason = machine.expire(start + SETTLE_BOUND).expect(case);
        assert!(reason.contains("barrier unmet"), "{case}: {reason}");
    }
}

/// While the barrier is unmet a zero-attempt dispatch waits, each settling frame asks for exactly one
/// redraw, and the deadline stays at `start + SETTLE_BOUND`, where the attempt expires with a reason
/// naming the unmet condition.
#[test]
fn an_unmet_barrier_waits_on_zero_attempt_dispatches_and_wakes_at_the_original_deadline() {
    let start = Instant::now();
    let mut machine = barred(start, later(start, 1_000), FINAL_TITLE);
    let stale = sample(start, 1_001, Some("bash"));
    let reading = reading_with(STALE_TITLE, Some(stale.clone()), Some(stale));
    for step in 0..40_u64 {
        let now = later(start, 1_000 + 200 * step);
        assert_eq!(machine.observe(Some(Counts::default()), &reading, now), Progress::Waiting);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
        assert_eq!(machine.deadline(), Some(start + SETTLE_BOUND));
    }
    assert_eq!(machine.expire(start + SETTLE_BOUND - Duration::from_millis(1)), None);
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert_eq!(
        reason,
        "settle (steady 0): not attempted within its bound; barrier unmet: 1 qualifying foreground \
         observation after the final process (need 2), latest cached \"bash\""
    );
}

/// Windows fixes T at the sentinel, which proves the workload finished, not that the App applied it.
/// Frames after the sentinel with no applied result do not settle, even when the title already matches;
/// two qualifying results after T are still needed.
#[test]
fn a_windows_sentinel_with_a_delayed_first_cache_application_does_not_settle_early() {
    let start = Instant::now();
    let sentinel_at = later(start, 800);
    let harness = "perf_scenarios";
    let mut machine = RecoveryEpisodes::new(start);
    machine.set_barrier(Barrier {
        final_name: harness.to_owned(),
        ..barrier(sentinel_at, FINAL_TITLE)
    });
    for frame in 0..10_u64 {
        let reading = reading_with(FINAL_TITLE, None, None);
        let now = later(sentinel_at, 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let first = sample(start, 1_300, Some(harness));
    for frame in 0..5_u64 {
        let reading = reading_with(FINAL_TITLE, Some(first.clone()), Some(first.clone()));
        let now = later(start, 1_310 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let second = sample(start, 1_800, Some(harness));
    let met = reading_with(FINAL_TITLE, Some(second.clone()), Some(second));
    let now = later(start, 1_810);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::ChangeAtlas));
}

/// The settle deadline bounds the barrier's progress, not one OS call: no lookup starts at or past it,
/// and a lookup that started in time but completed at or past it sets no T, even naming the final
/// process, so the machine expires at its bound. One completing just before it sets T at completion.
#[test]
fn a_lookup_completing_at_or_after_the_deadline_is_not_accepted() {
    let start = Instant::now();
    let deadline = Some(start + SETTLE_BOUND);
    let mut lookup = FinalLookup::new(FINAL_PROCESS.to_owned());
    assert!(!lookup.begin(start + SETTLE_BOUND, deadline));
    assert!(!lookup.begin(start, None), "an ended machine makes no lookup");
    let started = start + SETTLE_BOUND - Duration::from_millis(5);
    assert!(lookup.begin(started, deadline));
    assert!(!lookup.finish(Some(FINAL_PROCESS), started, start + SETTLE_BOUND, deadline));
    assert_eq!(lookup.found_at(), None);
    assert!(!lookup.begin(started + LOOKUP_SPACING, deadline));
    let mut machine = RecoveryEpisodes::new(start);
    machine.set_barrier(Barrier { final_at: lookup.found_at(), ..barrier(start, FINAL_TITLE) });
    let reason = machine.expire(start + SETTLE_BOUND).expect("expired");
    assert!(
        reason.ends_with("barrier unmet: the final process \"sleep\" was never observed"),
        "{reason}"
    );

    let mut timely = FinalLookup::new(FINAL_PROCESS.to_owned());
    assert!(timely.begin(started, deadline));
    let completed = start + SETTLE_BOUND - Duration::from_millis(1);
    assert!(timely.finish(Some(FINAL_PROCESS), started, completed, deadline));
    assert_eq!(timely.found_at(), Some(completed));
    assert_eq!((timely.lookups(), timely.spent()), (1, Duration::from_millis(4)));
}

/// A custom tab title replaces the derived one, so no final title can be expected: the attempt is invalid
/// at its first settling dispatch with a named fixture reason, not at the 10 s bound.
#[test]
fn a_custom_tab_title_is_a_named_invalid_fixture() {
    let start = Instant::now();
    let mut machine = RecoveryEpisodes::new(start);
    machine.set_barrier(Barrier {
        fixture_problem: Some(custom_title_problem("my \u{E760} tab")),
        ..barrier(start, FINAL_TITLE)
    });
    let reading = reading_with(FINAL_TITLE, None, None);
    let progress = machine.observe(Some(Counts::default()), &reading, later(start, 16));
    assert_eq!(
        progress,
        Progress::Invalid(
            r#"settle (steady 0): fixture: the tab has a custom title "my \u{e760} tab""#
                .to_owned()
        )
    );
    assert_eq!(machine.deadline(), None);
}

/// The guarantee the barrier keeps: once settled, a title change seen in either the before or the after
/// reading of any dispatch ends the run, naming both titles and the side that showed it.
#[test]
fn a_title_change_after_settling_ends_the_run_on_either_side() {
    let now = Instant::now();
    for side in ["before", "after"] {
        let mut machine = settled(now);
        let (before, after) = match side {
            "before" => (titled(FINAL_TITLE), fixture_scene()),
            _ => (fixture_scene(), titled(FINAL_TITLE)),
        };
        let reading = SceneReading { before: Some(before), after: Some(after), ..same() };
        let progress = machine.observe(Some(Counts::default()), &reading, now);
        let Progress::Invalid(reason) = progress else { panic!("{side}: {progress:?}") };
        assert!(
            reason.ends_with(r##"title changed from "sonicterm" to "#1 \u{f489} shell""##),
            "{side}: {reason}"
        );
        assert_eq!(machine.rejection().map(|rejection| rejection.side), Some(side));
    }
}

/// The observed failure, replayed: the pre-T cache is the shell, cached as `bash` with title E760, over
/// many steady frames; then T; then one qualifying result still naming `bash`; then a second naming
/// `sleep` with title F489. It settles on F489 and reaches `Done`. Without the barrier it settled on
/// E760 and failed at the first F489 reading.
#[test]
fn a_stale_shell_title_does_not_settle_before_the_final_process_is_applied() {
    let start = Instant::now();
    let final_at = later(start, 2_000);
    let mut machine = barred(start, final_at, FINAL_TITLE);
    let stale = sample(start, 400, Some("bash"));
    for frame in 0..12_u64 {
        let reading = reading_with(STALE_TITLE, Some(stale.clone()), Some(stale.clone()));
        let now = later(start, 100 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let first = sample(start, 2_001, Some("bash"));
    for frame in 0..6_u64 {
        let reading = reading_with(STALE_TITLE, Some(first.clone()), Some(first.clone()));
        let now = later(start, 2_010 + 16 * frame);
        assert_eq!(machine.observe(Some(steady()), &reading, now), Progress::Arm(Arm::Redraw));
    }
    let second = sample(start, 2_501, Some(FINAL_PROCESS));
    let met = reading_with(FINAL_TITLE, Some(second.clone()), Some(second));
    let now = later(start, 2_510);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &met, now), Progress::Arm(Arm::ChangeAtlas));
    assert_eq!(machine.scene().map(|scene| scene.title.as_str()), Some(FINAL_TITLE));
    assert_eq!(run_episodes(&mut machine, &met, now), Progress::Done);
}

/// The harness's own lookup starts at most once per `LOOKUP_SPACING` on a fake 16 ms frame clock, and
/// never again once it has found the final process.
#[test]
fn the_independent_lookup_runs_at_most_every_100_ms_and_stops_once_final() {
    let start = Instant::now();
    let deadline = Some(start + SETTLE_BOUND);
    let mut lookup = FinalLookup::new(FINAL_PROCESS.to_owned());
    let mut begun = Vec::new();
    for step in 0..30_u64 {
        let now = later(start, 16 * step);
        if lookup.begin(now, deadline) {
            begun.push(16 * step);
            lookup.finish(Some("bash"), now, now, deadline);
        }
    }
    assert_eq!(begun, [0, 112, 224, 336, 448]);
    assert_eq!(lookup.lookups(), 5);
    let now = later(start, 600);
    assert!(lookup.begin(now, deadline));
    assert!(lookup.finish(Some(FINAL_PROCESS), now, later(now, 1), deadline));
    for step in 1..20_u64 {
        assert!(!lookup.begin(later(now, 100 * step), deadline), "no lookup once found");
    }
    assert_eq!(lookup.lookups(), 6);
}

/// Windows fixes T at the sentinel instead of looking the final process up: a search built already found
/// keeps the expected name and the sentinel instant, has made no lookup, and starts none afterwards.
#[test]
fn a_search_found_at_the_sentinel_keeps_its_name_and_instant_and_makes_no_lookup() {
    let sentinel_at = Instant::now();
    let mut lookup = FinalLookup::found(FINAL_PROCESS.to_owned(), Some(sentinel_at));
    assert_eq!(lookup.expected(), FINAL_PROCESS);
    assert_eq!(lookup.found_at(), Some(sentinel_at));
    assert_eq!((lookup.lookups(), lookup.spent()), (0, Duration::ZERO));
    // A live deadline, so only the fixed T can refuse the lookup.
    let deadline = Some(sentinel_at + SETTLE_BOUND);
    assert!(
        !lookup.begin(sentinel_at + LOOKUP_SPACING, deadline),
        "a found search starts no lookup"
    );
    let mut unseen = FinalLookup::found(FINAL_PROCESS.to_owned(), None);
    assert_eq!(unseen.found_at(), None, "no sentinel seen leaves T unset");
    assert!(unseen.begin(sentinel_at, deadline), "with T unset a lookup may start");
}
