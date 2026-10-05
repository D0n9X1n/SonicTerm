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

/// A dispatch that leaves `scene` as it found it.
fn around(scene: Scene) -> SceneReading {
    SceneReading { before: Some(scene.clone()), after: Some(scene) }
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
    let mut machine = RecoveryEpisodes::new(now);
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
    let mut machine = RecoveryEpisodes::new(now);
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
    let mut machine = RecoveryEpisodes::new(now);
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
    let mut settling = RecoveryEpisodes::new(now);
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
        let mut machine = RecoveryEpisodes::new(now);
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
    let reading = SceneReading { before: Some(fixture_scene()), after: Some(applied) };
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
    let reading = SceneReading { before: Some(fixture_scene()), after: Some(changed.clone()) };
    let progress = machine.observe(Some(Counts::default()), &reading, now);
    assert!(
        matches!(&progress, Progress::Invalid(reason) if reason.contains("cursor")),
        "{progress:?}"
    );
    let mut machine = settled(now);
    let reverting = SceneReading { before: Some(changed), after: Some(fixture_scene()) };
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
    let mut machine = RecoveryEpisodes::new(now);
    machine.observe(Some(steady()), &same(), now);
    machine.observe(Some(steady()), &same(), now);
    let retitled = around(Scene { title: "retitled".to_owned(), ..fixture_scene() });
    // The frame that shows the change is not steady; three steady frames of the new scene follow.
    assert_eq!(machine.observe(Some(steady()), &retitled, now), Progress::Arm(Arm::Redraw));
    assert!(!machine.settled(), "the change restarted the count");
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), &retitled, now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), &retitled, now), Progress::Arm(Arm::ChangeAtlas));
    assert_eq!(machine.scene().map(|scene| scene.title.as_str()), Some("retitled"));

    let pending = around(Scene { fallback: (1, 0, 2), ..fixture_scene() });
    let mut machine = RecoveryEpisodes::new(now);
    for _ in 0..STEADY_FRAMES + 2 {
        assert_eq!(machine.observe(Some(steady()), &pending, now), Progress::Arm(Arm::Redraw));
    }
    let mut stranger = fixture_scene();
    stranger.rows[0] = "unrelated".to_owned();
    stranger.rows[1] = "unrelated".to_owned();
    stranger.rows[2] = "unrelated".to_owned();
    let mut machine = RecoveryEpisodes::new(now);
    for _ in 0..STEADY_FRAMES + 2 {
        assert_eq!(
            machine.observe(Some(steady()), &around(stranger.clone()), now),
            Progress::Arm(Arm::Redraw)
        );
    }
    let mut machine = RecoveryEpisodes::new(now);
    let unread = SceneReading { before: Some(fixture_scene()), after: None };
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
        };
        let progress = machine.observe(Some(fitting(Frame::Retried)), &changed, now);
        assert!(
            matches!(&progress, Progress::Invalid(reason) if reason.contains("font fallback")),
            "{fallback:?}: {progress:?}"
        );
    }
}
