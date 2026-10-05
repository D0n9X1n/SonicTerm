use super::*;

/// The counts of one attempt that fits `frame`, at atlas dimension 2048.
fn fitting(frame: Frame) -> Counts {
    let (presented, resets) = if frame == Frame::Retried { (0, 1) } else { (1, 0) };
    Counts { attempts: 1, presented, resets, hits: 0, misses: 0, shapes: 0, atlas_dim: 2048 }
}

/// A steady settling frame.
fn steady() -> Counts {
    fitting(Frame::Reused)
}

/// A machine already through its settle, at `now`.
fn settled(now: Instant) -> RecoveryEpisodes {
    let mut machine = RecoveryEpisodes::new(now);
    for _ in 0..STEADY_FRAMES - 1 {
        assert_eq!(machine.observe(Some(steady()), now), Progress::Arm(Arm::Redraw));
    }
    assert_eq!(machine.observe(Some(steady()), now), Progress::Arm(Arm::ChangeAtlas));
    assert!(machine.settled());
    machine
}

/// A deferred dispatch attempts nothing, so neither the settle nor a step advances.
#[test]
fn a_dispatch_without_an_attempt_advances_nothing() {
    let now = Instant::now();
    let mut machine = RecoveryEpisodes::new(now);
    assert_eq!(machine.observe(Some(Counts::default()), now), Progress::Waiting);
    assert!(!machine.settled());
    let mut machine = settled(now);
    assert_eq!(machine.observe(Some(Counts::default()), now), Progress::Waiting);
    assert!(machine.records().is_empty());
}

/// A non-steady frame restarts the settle count; three steady frames in a row settle the scene.
#[test]
fn only_consecutive_steady_frames_settle_the_scene() {
    let now = Instant::now();
    let mut machine = RecoveryEpisodes::new(now);
    machine.observe(Some(steady()), now);
    machine.observe(Some(steady()), now);
    let missed = Counts { misses: 3, ..steady() };
    assert_eq!(machine.observe(Some(missed), now), Progress::Arm(Arm::Redraw));
    machine.observe(Some(steady()), now);
    machine.observe(Some(steady()), now);
    assert!(!machine.settled(), "the miss restarted the count");
    assert_eq!(machine.observe(Some(steady()), now), Progress::Arm(Arm::ChangeAtlas));
}

/// A normal run records eight episodes of A, B, C, D in order, arming the change for A, the
/// invalidation only for B and a redraw for C and D, and its records pass validation.
#[test]
fn eight_normal_episodes_complete_and_validate() {
    let now = Instant::now();
    let mut machine = settled(now);
    for episode in 0..EPISODES {
        for frame in [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated] {
            let progress = machine.observe(Some(fitting(frame)), now);
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
    assert!(matches!(machine.observe(Some(doubled), now), Progress::Invalid(reason) if reason.contains("2 attempts")));
    assert_eq!(machine.observe(Some(fitting(Frame::Retried)), now), Progress::Waiting, "an ended run ignores later frames");
}

/// A that presents, A whose injected change reset nothing, or C that resets, does not fit its frame.
#[test]
fn a_frame_whose_counts_do_not_fit_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    let presented_a = Counts { presented: 1, ..fitting(Frame::Retried) };
    assert!(matches!(machine.observe(Some(presented_a), now), Progress::Invalid(reason) if reason.contains("episode 0 A")));
    // An A that neither presents nor resets is a deferral-like attempt the change never reached,
    // so it is no retry and must not be recorded as one.
    let mut machine = settled(now);
    let unreset_a = Counts { resets: 0, ..fitting(Frame::Retried) };
    assert!(matches!(machine.observe(Some(unreset_a), now), Progress::Invalid(reason) if reason.contains("episode 0 A")));
    let mut machine = settled(now);
    machine.observe(Some(fitting(Frame::Retried)), now);
    machine.observe(Some(fitting(Frame::Recovered)), now);
    let reset_c = Counts { resets: 1, ..fitting(Frame::Reused) };
    assert!(matches!(machine.observe(Some(reset_c), now), Progress::Invalid(reason) if reason.contains("episode 0 C")));
}

/// A recovered frame at another atlas dimension is invalid.
#[test]
fn a_recovered_frame_at_another_atlas_dimension_is_invalid() {
    let now = Instant::now();
    let mut machine = settled(now);
    machine.observe(Some(fitting(Frame::Retried)), now);
    machine.observe(Some(fitting(Frame::Recovered)), now);
    let grown = Counts { atlas_dim: 4096, ..fitting(Frame::Reused) };
    assert!(matches!(machine.observe(Some(grown), now), Progress::Invalid(reason) if reason.contains("dimension")));
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
    assert_eq!(machine.observe(None, now), Progress::Invalid("counters unavailable".to_owned()));
}

/// Validation refuses a short run, a misordered frame, an extra attempt, an A without a reset and a
/// mismatched dimension.
#[test]
fn records_validation_refuses_every_malformed_run() {
    let now = Instant::now();
    let mut machine = settled(now);
    for _ in 0..EPISODES {
        for frame in [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated] {
            machine.observe(Some(fitting(frame)), now);
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
/// passes, while a scene missing fixture lines or out of order does not.
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
    let mut misordered = scene;
    misordered.swap(3, 4);
    assert!(scene_problem(&misordered).is_some());
}
