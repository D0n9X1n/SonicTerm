//! Recorded atlas working sets and their source-specific provenance checks.

use super::*;

/// The Performance comparison whose base side measured the perf-end rows.
pub const PERF_END_RUN_ID: u64 = 37_525_311_953;
/// The push CI run that measured the real-renderer rows.
pub const PUSH_RUN_ID: u64 = 37_517_293_271;
/// The commit the perf comparison's base side and the push CI run measured.
pub const MEASURED_SHA: &str = "865acccd18a6e09cf7126dfc6f65830729fd71b7";
/// The pull-request CI run whose working-set step measured the helper rows.
pub const HELPER_RUN_ID: u64 = 40_318_123_389;
/// The pull request that run measured: the change that replaced the bundled text face with the
/// platform's own monospace face.
pub const HELPER_PULL_REQUEST: u32 = 1711;
/// The exact pull-request head the helper run measured.
pub const HELPER_MEASURED_SHA: &str = "2cabd00f29443612d8cd4a6f4858d38d176d0e7e";

/// A perf-end row: the visible renderer's atlas at a certified `end` checkpoint of an accepted
/// base-side attempt, at scale 1, which fit the 256 floor with every required glyph drawn.
const fn perf_row(
    platform: &'static str,
    fixture: &'static str,
    source: InputSource,
    set: &'static str,
    max_tile: [u32; 2],
    packed_pixels: u64,
    origin: &'static str,
) -> StartSizeInput {
    StartSizeInput {
        platform,
        scale: 1,
        fixture,
        source,
        outcome: FitOutcome::Fits(256),
        max_tile,
        incomplete_glyphs: 0,
        packed_pixels,
        provenance: Provenance {
            run_id: PERF_END_RUN_ID,
            attempt: 1,
            measured_sha: MEASURED_SHA,
            side: "base",
            pull_request: None,
            set,
            origin,
        },
    }
}

/// A helper row from the glyph-atlas-working-set step, with every required glyph drawn.
const fn helper_row(
    platform: &'static str,
    scale: u32,
    fixture: &'static str,
    fit: u32,
    max_tile: [u32; 2],
    packed_pixels: u64,
    origin: &'static str,
) -> StartSizeInput {
    StartSizeInput {
        platform,
        scale,
        fixture,
        source: InputSource::Helper,
        outcome: FitOutcome::Fits(fit),
        max_tile,
        incomplete_glyphs: 0,
        packed_pixels,
        provenance: Provenance {
            run_id: HELPER_RUN_ID,
            attempt: 1,
            measured_sha: HELPER_MEASURED_SHA,
            side: "pull_request",
            pull_request: Some(HELPER_PULL_REQUEST),
            set: "working-set-step",
            origin,
        },
    }
}

/// A Windows real-renderer row from the coverage test, printed by the test run `set` names, with
/// every required glyph drawn.
const fn real_row(
    scale: u32,
    fixture: &'static str,
    fit: u32,
    max_tile: [u32; 2],
    packed_pixels: u64,
    set: &'static str,
    origin: &'static str,
) -> StartSizeInput {
    StartSizeInput {
        platform: "windows",
        scale,
        fixture,
        source: InputSource::RealRenderer,
        outcome: FitOutcome::Fits(fit),
        max_tile,
        incomplete_glyphs: 0,
        packed_pixels,
        provenance: Provenance {
            run_id: PUSH_RUN_ID,
            attempt: 1,
            measured_sha: MEASURED_SHA,
            side: "push",
            pull_request: None,
            set,
            origin,
        },
    }
}

/// Check the one-run rule over `rows`: every row of one source comes from one run, attempt and
/// commit. Perf-end rows come from a comparison's `base` side; helper and real-renderer rows from a
/// `push` run on `main` or a `pull_request` run on the head that changed the working set. A
/// `pull_request` row names its pull request and no other row does. Sources may differ in run.
///
/// # Errors
/// The first row that breaks the rule, named with its provenance.
pub fn check_provenance(rows: &[StartSizeInput]) -> Result<(), String> {
    let mut first_of_source: Vec<(InputSource, Provenance)> = Vec::new();
    for row in rows {
        let provenance = row.provenance;
        let perf = matches!(row.source, InputSource::PerfS9End | InputSource::PerfS12End);
        // A perf-end row comes only from a comparison's base side; the others from a CI run.
        let side_allowed = (perf && provenance.side == "base")
            || (!perf && matches!(provenance.side, "push" | "pull_request"));
        if !side_allowed {
            // When: side_allowed is false, the row's side cannot have measured its source.
            return Err(format!("{:?} row from side {}: {row:?}", row.source, provenance.side));
        }
        if (provenance.side == "pull_request") != provenance.pull_request.is_some() {
            // When: a pull_request row names no pull request, or another side names one.
            return Err(format!("only a pull_request row names a pull request: {row:?}"));
        }
        let run =
            |of: &Provenance| (of.run_id, of.attempt, of.measured_sha, of.side, of.pull_request);
        let Some((source, first)) =
            first_of_source.iter().find(|(source, _)| *source == row.source)
        else {
            // When: find returns none, this is the source's first row, so it names the source's run.
            first_of_source.push((row.source, provenance));
            continue;
        };
        if run(first) != run(&provenance) {
            // When: the source's first row names another run, the set mixes two runs.
            return Err(format!(
                "{source:?} rows come from two runs: {} and {}",
                first.describe(),
                provenance.describe()
            ));
        }
    }
    Ok(())
}

/// Every measured input: 32 perf-end rows (macOS and Windows, S9 and S12, every accepted base-side
/// timed, laps and counters attempt), 8 helper rows (both platforms, both fixtures, scales 1 and 2)
/// and 24 Windows real-renderer rows (both fixtures and scales, from each of the six test runs that
/// executed the coverage test). Helper, real-renderer and perf-end rows stay distinct by `source`.
pub const START_SIZE_INPUTS: &[StartSizeInput] = &[
    // Perf end: base side of the eligible Performance comparison, each joined to its attempt's
    // complete `end` checkpoint and read from the visible renderer.
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "counters",
        [14, 14],
        4492,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/counters/01-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "counters",
        [14, 14],
        4645,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/counters/04-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [14, 14],
        4340,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/timed/01-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [14, 14],
        4341,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/timed/04-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [14, 14],
        4452,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/timed/05-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [14, 14],
        4492,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/timed/08-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [14, 14],
        4578,
        r"/Users/runner/work/_temp/perf-comparison/runs/S12-default/timed/09-base checkpoint 2 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "counters",
        [20, 20],
        11193,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/counters/01-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "counters",
        [20, 20],
        11130,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/counters/04-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "laps",
        [20, 20],
        11130,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/laps/01-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "laps",
        [20, 20],
        11154,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/laps/04-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [20, 20],
        11010,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/timed/01-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [20, 20],
        11058,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/timed/04-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [20, 20],
        11157,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/timed/05-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [20, 20],
        10965,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/timed/08-base checkpoint 0 end",
    ),
    perf_row(
        "macos",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [20, 20],
        11010,
        r"/Users/runner/work/_temp/perf-comparison/runs/S9-default/timed/09-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "counters",
        [16, 15],
        7479,
        r"D:\a\_temp\perf-comparison\runs\S12-default\counters\01-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "counters",
        [16, 15],
        7623,
        r"D:\a\_temp\perf-comparison\runs\S12-default\counters\04-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [16, 15],
        7711,
        r"D:\a\_temp\perf-comparison\runs\S12-default\timed\01-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [16, 15],
        7487,
        r"D:\a\_temp\perf-comparison\runs\S12-default\timed\04-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [16, 15],
        7631,
        r"D:\a\_temp\perf-comparison\runs\S12-default\timed\05-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [16, 15],
        7463,
        r"D:\a\_temp\perf-comparison\runs\S12-default\timed\08-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S12",
        InputSource::PerfS12End,
        "timed",
        [16, 15],
        7463,
        r"D:\a\_temp\perf-comparison\runs\S12-default\timed\09-base checkpoint 2 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "counters",
        [16, 15],
        14036,
        r"D:\a\_temp\perf-comparison\runs\S9-default\counters\01-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "counters",
        [16, 15],
        13984,
        r"D:\a\_temp\perf-comparison\runs\S9-default\counters\04-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "laps",
        [16, 15],
        13648,
        r"D:\a\_temp\perf-comparison\runs\S9-default\laps\01-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "laps",
        [16, 15],
        13972,
        r"D:\a\_temp\perf-comparison\runs\S9-default\laps\04-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [16, 15],
        14036,
        r"D:\a\_temp\perf-comparison\runs\S9-default\timed\01-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [16, 15],
        14044,
        r"D:\a\_temp\perf-comparison\runs\S9-default\timed\04-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [16, 15],
        14036,
        r"D:\a\_temp\perf-comparison\runs\S9-default\timed\05-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [16, 15],
        14212,
        r"D:\a\_temp\perf-comparison\runs\S9-default\timed\08-base checkpoint 0 end",
    ),
    perf_row(
        "windows",
        "S9",
        InputSource::PerfS9End,
        "timed",
        [16, 15],
        14212,
        r"D:\a\_temp\perf-comparison\runs\S9-default\timed\09-base checkpoint 0 end",
    ),
    // Helper: the glyph-atlas-working-set step on each platform, from the pull-request run on the
    // head that made the platform monospace face the scenarios' text face.
    helper_row("macos", 1, "S9", 1024, [20, 20], 183984, "job 121555774360 line 9070"),
    helper_row("macos", 1, "S12", 1024, [19, 17], 153224, "job 121555774360 line 9071"),
    helper_row("macos", 2, "S9", 2048, [36, 33], 667708, "job 121555774360 line 9075"),
    helper_row("macos", 2, "S12", 2048, [36, 33], 594042, "job 121555774360 line 9076"),
    helper_row("windows", 1, "S9", 1024, [16, 15], 181276, "job 121556063193 line 764"),
    helper_row("windows", 1, "S12", 1024, [16, 15], 159376, "job 121556063193 line 765"),
    helper_row("windows", 2, "S9", 2048, [30, 30], 645878, "job 121556063193 line 769"),
    helper_row("windows", 2, "S12", 2048, [30, 30], 565182, "job 121556063193 line 770"),
    // Real renderer: Windows test 17, once per test invocation that ran it.
    real_row(1, "S9", 256, [16, 15], 13847, "workspace-crates", "job 112454017976 line 11453"),
    real_row(2, "S9", 512, [30, 29], 48156, "workspace-crates", "job 112454017976 line 11458"),
    real_row(1, "S12", 256, [16, 13], 8111, "workspace-crates", "job 112454017976 line 11459"),
    real_row(2, "S12", 256, [30, 26], 24340, "workspace-crates", "job 112454017976 line 11460"),
    real_row(1, "S9", 256, [16, 15], 13665, "perf-scenarios-tests", "job 112454017976 line 14903"),
    real_row(2, "S9", 512, [30, 29], 48156, "perf-scenarios-tests", "job 112454017976 line 14908"),
    real_row(1, "S12", 256, [16, 13], 8111, "perf-scenarios-tests", "job 112454017976 line 14909"),
    real_row(2, "S12", 256, [30, 26], 24340, "perf-scenarios-tests", "job 112454017976 line 14910"),
    real_row(
        1,
        "S9",
        256,
        [16, 15],
        14557,
        "perf-scenarios-counters-tests",
        "job 112454017976 line 15189",
    ),
    real_row(
        2,
        "S9",
        512,
        [30, 29],
        48156,
        "perf-scenarios-counters-tests",
        "job 112454017976 line 15194",
    ),
    real_row(
        1,
        "S12",
        256,
        [16, 13],
        8111,
        "perf-scenarios-counters-tests",
        "job 112454017976 line 15195",
    ),
    real_row(
        2,
        "S12",
        256,
        [30, 26],
        24340,
        "perf-scenarios-counters-tests",
        "job 112454017976 line 15196",
    ),
    real_row(
        1,
        "S9",
        256,
        [16, 15],
        14193,
        "perf-scenarios-frame-texture-tests",
        "job 112454017976 line 15511",
    ),
    real_row(
        2,
        "S9",
        512,
        [30, 29],
        48156,
        "perf-scenarios-frame-texture-tests",
        "job 112454017976 line 15516",
    ),
    real_row(
        1,
        "S12",
        256,
        [16, 13],
        8111,
        "perf-scenarios-frame-texture-tests",
        "job 112454017976 line 15517",
    ),
    real_row(
        2,
        "S12",
        256,
        [30, 26],
        24340,
        "perf-scenarios-frame-texture-tests",
        "job 112454017976 line 15518",
    ),
    real_row(
        1,
        "S9",
        256,
        [16, 15],
        14557,
        "perf-scenarios-echo-trace-tests",
        "job 112454017976 line 15801",
    ),
    real_row(
        2,
        "S9",
        512,
        [30, 29],
        48156,
        "perf-scenarios-echo-trace-tests",
        "job 112454017976 line 15806",
    ),
    real_row(
        1,
        "S12",
        256,
        [16, 13],
        8111,
        "perf-scenarios-echo-trace-tests",
        "job 112454017976 line 15807",
    ),
    real_row(
        2,
        "S12",
        256,
        [30, 26],
        24340,
        "perf-scenarios-echo-trace-tests",
        "job 112454017976 line 15808",
    ),
    real_row(
        1,
        "S9",
        256,
        [16, 15],
        14157,
        "perf-scenarios-harness-api-tests",
        "job 112454017976 line 16396",
    ),
    real_row(
        2,
        "S9",
        512,
        [30, 29],
        48156,
        "perf-scenarios-harness-api-tests",
        "job 112454017976 line 16410",
    ),
    real_row(
        1,
        "S12",
        256,
        [16, 13],
        8111,
        "perf-scenarios-harness-api-tests",
        "job 112454017976 line 16411",
    ),
    real_row(
        2,
        "S12",
        256,
        [30, 26],
        24340,
        "perf-scenarios-harness-api-tests",
        "job 112454017976 line 16412",
    ),
];

#[cfg(test)]
#[path = "start_size_inputs_recorded_tests.rs"]
mod start_size_inputs_recorded_tests;
