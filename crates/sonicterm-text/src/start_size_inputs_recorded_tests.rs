//! Recorded rows keep their run identities and source completeness.

use super::*;

/// The local-gate steps whose Windows test run executed the real-renderer coverage test and printed
/// its rows, in the order CI ran them.
const REAL_RENDERER_RUNS: [&str; 6] = [
    "workspace-crates",
    "perf-scenarios-tests",
    "perf-scenarios-counters-tests",
    "perf-scenarios-frame-texture-tests",
    "perf-scenarios-echo-trace-tests",
    "perf-scenarios-harness-api-tests",
];

/// Every recorded row keeps its provenance: perf-end rows come from the comparison's base side at
/// scale 1 with a timed, laps or counters set and an attempt directory naming its `end` checkpoint;
/// helper rows come from the working-set step of the pull-request run on the head that changed the
/// working set; real-renderer rows come from the push run, from one of the six named test runs,
/// each of which appears. Each source's rows come from one run, attempt and commit, and no two
/// rows share an identity.
#[test]
fn every_recorded_row_keeps_its_provenance() {
    assert_eq!(check_provenance(START_SIZE_INPUTS), Ok(()));
    let mut identities = std::collections::HashSet::new();
    for row in START_SIZE_INPUTS {
        let provenance = &row.provenance;
        assert_eq!(provenance.attempt, 1, "{row:?}");
        match row.source {
            InputSource::PerfS9End | InputSource::PerfS12End => {
                let run = (provenance.run_id, provenance.measured_sha, provenance.side);
                assert_eq!(run, (PERF_END_RUN_ID, MEASURED_SHA, "base"), "{row:?}");
                assert!(matches!(provenance.set, "timed" | "laps" | "counters"), "{row:?}");
                assert!(provenance.origin.ends_with(" end"), "{row:?}");
                assert_eq!(row.scale, 1, "{row:?}");
            }
            InputSource::Helper => {
                let run = (provenance.run_id, provenance.measured_sha, provenance.side);
                assert_eq!(run, (HELPER_RUN_ID, HELPER_MEASURED_SHA, "pull_request"), "{row:?}");
                assert_eq!(provenance.pull_request, Some(HELPER_PULL_REQUEST), "{row:?}");
                assert_eq!(provenance.set, "working-set-step");
            }
            InputSource::RealRenderer => {
                let run = (provenance.run_id, provenance.measured_sha, provenance.side);
                assert_eq!(run, (PUSH_RUN_ID, MEASURED_SHA, "push"), "{row:?}");
                assert_eq!(row.platform, "windows");
                assert!(REAL_RENDERER_RUNS.contains(&provenance.set), "{row:?}");
            }
        }
        // The source enters the identity by name, since `InputSource` is not hashable.
        let source = format!("{:?}", row.source);
        let identity =
            (row.platform, row.scale, row.fixture, provenance.set, provenance.origin, source);
        assert!(identities.insert(identity), "a repeated row: {row:?}");
    }
    let count =
        |source: InputSource| START_SIZE_INPUTS.iter().filter(|row| row.source == source).count();
    let perf = count(InputSource::PerfS9End) + count(InputSource::PerfS12End);
    assert_eq!((perf, count(InputSource::Helper), count(InputSource::RealRenderer)), (32, 8, 24));
    let real_runs: std::collections::HashSet<&str> = START_SIZE_INPUTS
        .iter()
        .filter(|row| row.source == InputSource::RealRenderer)
        .map(|row| row.provenance.set)
        .collect();
    let expected: std::collections::HashSet<&str> = REAL_RENDERER_RUNS.into_iter().collect();
    assert_eq!(real_runs, expected, "each of the six test runs printed its real-renderer rows");
}

/// The one-run rule refuses a set it cannot trace to one run: helper rows mixed from two runs, or
/// from one run at two commits; a `pull_request` row that names no pull request; a push row that
/// names one; and a perf-end row from anything but a comparison's base side. Rows of different
/// sources may come from different runs.
#[test]
fn provenance_rejects_rows_mixed_from_two_runs() {
    let helpers: Vec<StartSizeInput> =
        START_SIZE_INPUTS.iter().copied().filter(|row| row.source == InputSource::Helper).collect();
    assert_eq!(check_provenance(&helpers), Ok(()));
    let mut mixed = helpers.clone();
    mixed[1].provenance.run_id = PUSH_RUN_ID;
    let error = check_provenance(&mixed).expect_err("helper rows from two runs");
    assert!(error.contains("Helper") && error.contains("two runs"), "{error}");
    let mut two_commits = helpers.clone();
    two_commits[2].provenance.measured_sha = MEASURED_SHA;
    assert!(check_provenance(&two_commits).is_err(), "one run at two commits");
    let mut unnamed = helpers.clone();
    for row in &mut unnamed {
        row.provenance.pull_request = None;
    }
    let error = check_provenance(&unnamed).expect_err("a pull_request row names its PR");
    assert!(error.contains("pull request"), "{error}");
    let mut named_push: Vec<StartSizeInput> = START_SIZE_INPUTS
        .iter()
        .copied()
        .filter(|row| row.source == InputSource::RealRenderer)
        .collect();
    named_push[0].provenance.pull_request = Some(1);
    assert!(check_provenance(&named_push).is_err(), "a push row names no pull request");
    let mut perf: Vec<StartSizeInput> = START_SIZE_INPUTS
        .iter()
        .copied()
        .filter(|row| row.source == InputSource::PerfS9End)
        .collect();
    for row in &mut perf {
        row.provenance.side = "push";
    }
    assert!(check_provenance(&perf).is_err(), "perf-end rows come from a base side");
}
