//! Checkpoint transition and readiness contracts.

use super::super::probe_tests::*;
use super::*;

#[test]
fn a_first_sample_after_slow_checkpoint_io_opens_its_window_at_that_sample() {
    // The sampling window opens at its first attempt, not when the turn began. The first turn
    // starts at 0 ms, but its `.done` check takes until 610 ms, so the first sample is taken at
    // 610 ms and the deadline is 610 + 500 ms. Every sample, partial or complete, is taken before
    // the deadline in force; none is authorized against a deadline that had already passed.
    for complete in [true, false] {
        let mut run = CheckpointRun::new(&format!("late-first-{complete}"), true);
        run.jump_on_done.set(Some(run.at(610)));
        let taken = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let turn = |run: &mut CheckpointRun, millis: u64| {
            let taken = std::rc::Rc::clone(&taken);
            let outcome = run.turn(millis, move |sampled_ms| {
                taken.borrow_mut().push(sampled_ms);
                complete
            });
            if let Some(sampling) = run.pending.as_ref().and_then(|pending| pending.sampling) {
                assert_eq!(sampling.deadline, run.at(1_110), "the window opens at 610 ms");
            }
            outcome
        };
        assert_eq!(turn(&mut run, 0), CheckpointOutcome::Wait);
        assert_eq!(
            taken.borrow().first(),
            Some(&610),
            "the first sample is taken at the fresh time"
        );
        // The footprint is answered before its own 1 s wait ends, so only sampling holds the step.
        run.answer_footprint("0-end");
        if complete {
            assert_eq!(turn(&mut run, 650), CheckpointOutcome::Advance);
            assert_eq!(run.records[0].sampling, Some("complete"));
            assert_eq!(run.records[0].attempts, Some(1));
        } else {
            // Retries every 50 ms from 660 ms; the tenth attempt, at 1060 ms, exhausts sampling.
            for millis in (660..=1_010).step_by(50) {
                assert_eq!(turn(&mut run, millis), CheckpointOutcome::Wait, "{millis} ms");
            }
            assert_eq!(turn(&mut run, 1_060), CheckpointOutcome::Advance);
            assert_eq!(run.records[0].sampling, Some("exhausted"));
            assert_eq!(run.records[0].attempts, Some(10));
        }
        assert!(
            taken.borrow().iter().all(|&sampled_ms| sampled_ms < 1_110),
            "every sample precedes its deadline: {:?}",
            taken.borrow()
        );
        assert_eq!(run.sampler_calls as usize, taken.borrow().len());
    }
}

#[test]
fn with_nothing_pending_the_steps_wake_is_the_soonest_phase_wake() {
    // `steps_wake` is what `Probe::next_deadline` uses during steps: with no pending checkpoint it is
    // the soonest phase wake, or the run deadline when there is none, never later than that deadline.
    let start = Instant::now();
    let deadline = start + FIXTURE_RUN_DEADLINE;
    let at = |millis: u64| start + Duration::from_millis(millis);
    assert_eq!(steps_wake(None, start, [Some(at(300)), None, Some(at(120))], deadline), at(120));
    assert_eq!(steps_wake(None, start, [None, None], deadline), deadline);
    assert_eq!(
        steps_wake(None, start, [Some(deadline + Duration::from_secs(1))], deadline),
        deadline
    );
}

#[test]
fn only_an_advanced_checkpoint_moves_the_plan_on() {
    // The step loop starts the next phase only when `plan_moves_on` holds: a waiting or invalid
    // checkpoint keeps the plan where it is.
    assert!(plan_moves_on(&CheckpointOutcome::Advance));
    assert!(!plan_moves_on(&CheckpointOutcome::Wait));
    assert!(!plan_moves_on(&CheckpointOutcome::Invalid("stopped".into())));
}

/// The golden checkpoint runs the comparison script reads come from this build's own code: the
/// production checkpoint path, the App's hook (one pane held, so samples are partial), the
/// production log format and the production `result.json` serializer. Each late-exhaustion case
/// ends exhausted with its attempt count and a partial last attempt; a build without the hook
/// reports `checkpoint_memory = "unsupported"`. Every line written keeps what `parse_memory_line`
/// needs (the script's own message marker, a UTC stamp, the required totals), and the comparison
/// with the committed fixture includes that structure, so a renamed message or a dropped total
/// fails here. `SONICTERM_WRITE_CHECKPOINT_FIXTURE=1` rewrites this build's half of the fixture;
/// otherwise the committed fixture must match what the build produces.
#[test]
fn the_checkpoint_fixture_matches_what_this_build_writes() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CHECKPOINT_FIXTURE);
    let build_key = crate::record::checkpoint_memory_support();
    let mut produced = serde_json::Map::new();
    for case in FIXTURE_CASES {
        let (result, logs) = run_fixture_case(case);
        assert_eq!(result["checkpoint_memory"], build_key, "{}", case.name);
        // Every line this build writes must be one the comparison's parser keeps.
        let marker = memory_marker();
        for line in &logs {
            let projection = log_projection(line, &marker);
            assert_eq!(
                &projection[..4],
                [
                    "marker:true",
                    "stamp:utc",
                    "renderer_total_bytes:count",
                    "session_total_bytes:count"
                ],
                "{}: a memory line the parser would drop: {line}",
                case.name
            );
        }
        let tagged = logs.iter().filter(|line| line.contains("checkpoint_index=")).count();
        assert_eq!(tagged, case.attempts.unwrap_or(0) as usize, "one tagged line per attempt");
        produced.insert(case.name.to_owned(), serde_json::json!({"result": result, "logs": logs}));
    }
    let mut fixture: serde_json::Map<String, serde_json::Value> =
        std::fs::read_to_string(&fixture_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
    if std::env::var_os("SONICTERM_WRITE_CHECKPOINT_FIXTURE").is_some() {
        fixture.insert(build_key.to_owned(), serde_json::Value::Object(produced));
        let text = serde_json::to_string_pretty(&fixture).expect("fixture serializes") + "\n";
        std::fs::write(&fixture_path, text).expect("fixture written");
        return;
    }
    let committed = fixture
        .get(build_key)
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("no `{build_key}` runs in {}", fixture_path.display()));
    assert_eq!(
        committed.keys().collect::<Vec<_>>(),
        produced.keys().collect::<Vec<_>>(),
        "the fixture's cases"
    );
    let marker = memory_marker();
    for (name, run) in &produced {
        // The fixture is recorded with the attribution cfg off, as the comparison's fixture builds are; a build
        // with the cfg on writes its own cfg, and every other field must still match.
        let mut expected = committed[name]["result"].clone();
        assert_eq!(
            expected["s10_attribution_api"], false,
            "{name}: the fixture is recorded with the cfg off"
        );
        expected["s10_attribution_api"] = serde_json::Value::Bool(crate::attribution::API_ENABLED);
        assert_eq!(expected, run["result"], "{name}: result.json");
        let lines = |value: &serde_json::Value| -> Vec<Vec<String>> {
            value["logs"]
                .as_array()
                .expect("logs")
                .iter()
                .map(|line| log_projection(line.as_str().expect("line"), &marker))
                .collect()
        };
        assert_eq!(lines(&committed[name]), lines(run), "{name}: memory lines");
    }
}
