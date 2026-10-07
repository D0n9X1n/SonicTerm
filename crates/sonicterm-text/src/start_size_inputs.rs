//! Measured inputs that may choose the glyph atlas start size, and the pure rules over them.
//!
//! Each row is one CI measurement: a platform, a scale, a fixture and the source that measured it,
//! with the fit outcome, the largest tile it saw and how many required glyphs it drew as tofu.
//! [`validate_start_dim`] is the one check every start constant passes: the maximum is always valid
//! because it claims no savings, and a smaller start is valid only when [`SIZING_ORACLE_COMPLETE`]
//! is true, every input [`required_inputs`] names is recorded at that scale, and [`start_rule`]
//! over the scale's rows selects exactly that start. An empty or incomplete table is therefore the
//! conservative contract: both normal start constants stay at the maximum. Once the oracle is
//! complete, [`ruled_start`] is what each shipped start constant must equal.

use crate::glyph_atlas::{FitOutcome, ATLAS_DIM, MIN_ATLAS_DIM};
use sonicterm_types::GlyphRasterVariant;

/// Whether the measurement oracle can prove that a renderer drew its whole working set, which a
/// start below the maximum needs. The renderer reports shaped glyphs that draw nothing and tab
/// titles that never shape as missing glyphs, the strengthened real-renderer coverage test passed
/// on Windows CI, and every required row is recorded in [`START_SIZE_INPUTS`], so the guard is set.
/// Rows alone never lower a start: [`ruled_start`] still needs every required input at the scale.
pub const SIZING_ORACLE_COMPLETE: bool = true;

/// What produced one measured row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    /// The renderer's glyph atlas at the end of perf scenario S9.
    PerfS9End,
    /// The renderer's glyph atlas at the end of perf scenario S12.
    PerfS12End,
    /// The working-set helper's conservative superset for the fixture.
    Helper,
    /// A real renderer that drew the fixture with its chrome.
    RealRenderer,
}

/// Where one recorded row was measured: the CI run, the commit it measured and the exact output it
/// came from, so a row can be traced back to its evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provenance {
    /// The GitHub Actions run id.
    pub run_id: u64,
    /// The run attempt.
    pub attempt: u32,
    /// The commit the measurement ran on, full SHA.
    pub measured_sha: &'static str,
    /// `base` for a Performance comparison's base side, `push` for a push CI run.
    pub side: &'static str,
    /// The observation set: `timed`, `laps` or `counters` for a perf row, `working-set-step` for a
    /// helper row, and the gate step whose test run printed it for a real-renderer row.
    pub set: &'static str,
    /// The perf attempt directory with its `end` checkpoint, or the CI job and log line.
    pub origin: &'static str,
}

impl Provenance {
    /// One line naming the run, attempt, commit, side, set and origin, for a failure message.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "run {} attempt {} at {} ({} {}) {}",
            self.run_id, self.attempt, self.measured_sha, self.side, self.set, self.origin
        )
    }
}

/// One measured start-size input from a CI run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartSizeInput {
    /// `macos` or `windows`.
    pub platform: &'static str,
    /// Display scale factor the measurement ran at: 1 or 2.
    pub scale: u32,
    /// Fixture name, such as `S9` or `S12`.
    pub fixture: &'static str,
    /// What measured this row.
    pub source: InputSource,
    /// The atlas's fit outcome at the end of the measurement.
    pub outcome: FitOutcome,
    /// Largest resident tile width and height.
    pub max_tile: [u32; 2],
    /// Required glyphs the measurement drew as tofu: unresolved, or resolved but not rasterized.
    /// Any nonzero count makes the row select the maximum.
    pub incomplete_glyphs: usize,
    /// Pixels the measured working set packed, kept for reading; the rule does not use it.
    pub packed_pixels: u64,
    /// Where the row was measured.
    pub provenance: Provenance,
}

/// The Performance comparison whose base side measured the perf-end rows.
pub const PERF_END_RUN_ID: u64 = 37_525_311_953;
/// The push CI run that measured the helper and real-renderer rows.
pub const PUSH_RUN_ID: u64 = 37_517_293_271;
/// The commit both runs measured: the perf comparison's base side and the push CI's head.
pub const MEASURED_SHA: &str = "865acccd18a6e09cf7126dfc6f65830729fd71b7";

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
            run_id: PUSH_RUN_ID,
            attempt: 1,
            measured_sha: MEASURED_SHA,
            side: "push",
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
            set,
            origin,
        },
    }
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
    // Helper: the glyph-atlas-working-set step on each platform.
    helper_row("macos", 1, "S9", 1024, [23, 20], 172851, "job 112453387360 line 8841"),
    helper_row("macos", 1, "S12", 1024, [23, 16], 136073, "job 112453387360 line 8842"),
    helper_row("macos", 2, "S9", 2048, [45, 31], 617572, "job 112453387360 line 8845"),
    helper_row("macos", 2, "S12", 1024, [45, 31], 521016, "job 112453387360 line 8846"),
    helper_row("windows", 1, "S9", 1024, [24, 16], 181969, "job 112454017976 line 15228"),
    helper_row("windows", 1, "S12", 1024, [24, 16], 154061, "job 112454017976 line 15229"),
    helper_row("windows", 2, "S9", 2048, [46, 31], 639557, "job 112454017976 line 15232"),
    helper_row("windows", 2, "S12", 1024, [46, 31], 537991, "job 112454017976 line 15233"),
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

/// A start dimension and the reason the rule chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartVerdict {
    /// The chosen square start dimension in pixels.
    pub dim: u32,
    /// Human-readable reason, naming the input that decided it.
    pub verdict: String,
}

/// One input to [`start_rule`]: a label naming its source, its outcome, its largest tile and how
/// many required glyphs it drew as tofu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInput {
    /// Names the measurement in the verdict.
    pub label: String,
    /// Fit outcome the measurement reported.
    pub outcome: FitOutcome,
    /// Largest resident tile width and height.
    pub max_tile: [u32; 2],
    /// Required glyphs the measurement drew as tofu; nonzero selects the maximum.
    pub incomplete_glyphs: usize,
}

/// One input a start below the maximum needs recorded at its scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredInput {
    /// `macos` or `windows`.
    pub platform: &'static str,
    /// Fixture name, `S9` or `S12`.
    pub fixture: &'static str,
    /// What must have measured it.
    pub source: InputSource,
}

/// Every input a start below the maximum needs at `scale`: the helper on macOS and Windows and
/// the Windows real renderer for S9 and S12, plus at scale 1 the perf scenarios' end-of-run atlas
/// on both platforms (the perf scenarios run at scale 1 only).
#[must_use]
pub fn required_inputs(scale: u32) -> Vec<RequiredInput> {
    let fixtures = [("S9", InputSource::PerfS9End), ("S12", InputSource::PerfS12End)];
    let mut required = Vec::new();
    for (fixture, perf_source) in fixtures {
        for platform in ["macos", "windows"] {
            if scale == 1 {
                // The perf scenarios run at scale 1 only, so only that scale needs their rows.
                required.push(RequiredInput { platform, fixture, source: perf_source });
            }
            required.push(RequiredInput { platform, fixture, source: InputSource::Helper });
        }
        required.push(RequiredInput {
            platform: "windows",
            fixture,
            source: InputSource::RealRenderer,
        });
    }
    required
}

/// Choose a start dimension from measured inputs at one scale.
///
/// The result is the largest `Fits(dim)` among the inputs, raised to the smallest power of two that
/// holds every input's largest tile. Any input with a nonzero `incomplete_glyphs`, or with a
/// `FitsWithoutHeadroom`, `DoesNotFit` or `Evicted` outcome, selects the maximum, and the verdict
/// names it. No reviewed exception lets an incomplete measurement choose a smaller start.
///
/// # Errors
///
/// When `inputs` is empty: no measurement means no rule, never a silent default.
pub fn start_rule(inputs: &[RuleInput]) -> Result<StartVerdict, String> {
    if inputs.is_empty() {
        // When: inputs is empty there is nothing measured, so the caller must decide the default.
        return Err("no measured input".to_owned());
    }
    if let Some(incomplete) = inputs.iter().find(|input| input.incomplete_glyphs > 0) {
        // When: incomplete_glyphs is nonzero the measured set is not the whole working set.
        return Ok(StartVerdict {
            dim: ATLAS_DIM,
            verdict: format!(
                "{}: {} unresolved or unrasterized glyphs select {ATLAS_DIM}",
                incomplete.label, incomplete.incomplete_glyphs
            ),
        });
    }
    let mut fit_dim = MIN_ATLAS_DIM;
    let mut fit_label = &inputs[0].label;
    for input in inputs {
        match input.outcome {
            FitOutcome::Fits(dim) => {
                if dim > fit_dim {
                    fit_dim = dim;
                    fit_label = &input.label;
                }
            }
            bad => {
                // When: outcome is not a Fits the working set has no headroom below the maximum.
                return Ok(StartVerdict {
                    dim: ATLAS_DIM,
                    verdict: format!("{}: {} selects {ATLAS_DIM}", input.label, bad.label()),
                });
            }
        }
    }
    let tile_side =
        inputs.iter().map(|input| input.max_tile[0].max(input.max_tile[1])).max().unwrap_or(0);
    let tile_dim = tile_side.next_power_of_two().clamp(MIN_ATLAS_DIM, ATLAS_DIM);
    let dim = fit_dim.max(tile_dim).min(ATLAS_DIM);
    let verdict = if tile_dim > fit_dim {
        format!("{fit_label} fits {fit_dim}; max tile {tile_side} raises it to {dim}")
    } else {
        // When: tile_dim does not exceed fit_dim the largest fit decides on its own.
        format!("{fit_label}: fits {dim}")
    };
    Ok(StartVerdict { dim, verdict })
}

/// Validate a normal renderer's start dimension `start` at `scale` against measured `rows`.
///
/// The maximum is always valid: it saves nothing, so it needs no measurement, and its verdict says
/// whether any recorded row would have chosen it anyway. A smaller start is valid only when
/// `oracle_complete` is true, every [`required_inputs`] entry has a row at `scale`, and
/// [`start_rule`] over the scale's rows selects exactly `start`.
///
/// # Errors
///
/// When `start` is below the maximum and any of those conditions fails; the message names it.
pub fn validate_start_dim(
    scale: u32,
    start: u32,
    rows: &[StartSizeInput],
    oracle_complete: bool,
) -> Result<StartVerdict, String> {
    let inputs = rule_inputs(scale, rows);
    if start == ATLAS_DIM {
        // When: start is the maximum it claims no savings, so an empty or incomplete table is fine.
        let verdict = match start_rule(&inputs) {
            Ok(rule) => format!("maximum selected, no savings ({})", rule.verdict),
            Err(_) => format!("no measured {scale}x input: maximum selected, no savings"),
        };
        return Ok(StartVerdict { dim: ATLAS_DIM, verdict });
    }
    if !oracle_complete {
        // When: oracle_complete is false no recorded row can prove a smaller start draws everything.
        return Err(format!(
            "{scale}x start {start} is below the maximum while the sizing oracle is incomplete"
        ));
    }
    let missing = missing_inputs(scale, rows);
    if !missing.is_empty() {
        // When: missing names an unrecorded input, the rows cannot justify a smaller start.
        return Err(format!(
            "{scale}x start {start} lacks required inputs: {}",
            missing.join(", ")
        ));
    }
    let rule = start_rule(&inputs)?;
    if rule.dim != start {
        // When: rule.dim differs from start the constant is not what the measurements select.
        return Err(format!(
            "{scale}x start {start} is not the rule's {}: {}",
            rule.dim, rule.verdict
        ));
    }
    Ok(rule)
}

/// The start [`start_rule`] selects at `scale` over `rows` once the oracle is complete: what a
/// shipped start constant must equal, the maximum included.
///
/// # Errors
///
/// When `oracle_complete` is false, when any [`required_inputs`] entry has no row at `scale`, or
/// when the scale has no row at all; the message names the condition.
pub fn ruled_start(
    scale: u32,
    rows: &[StartSizeInput],
    oracle_complete: bool,
) -> Result<StartVerdict, String> {
    if !oracle_complete {
        // When: oracle_complete is false the rows cannot rule a start, so none is claimed.
        return Err(format!("{scale}x has no ruled start while the sizing oracle is incomplete"));
    }
    let missing = missing_inputs(scale, rows);
    if !missing.is_empty() {
        // When: missing names an unrecorded input, the rule over the other rows is incomplete.
        return Err(format!("{scale}x lacks required inputs: {}", missing.join(", ")));
    }
    start_rule(&rule_inputs(scale, rows))
}

/// Each [`required_inputs`] entry at `scale` with no row in `rows`, named for a message.
fn missing_inputs(scale: u32, rows: &[StartSizeInput]) -> Vec<String> {
    required_inputs(scale)
        .into_iter()
        .filter(|required| {
            !rows.iter().any(|row| {
                row.scale == scale
                    && row.platform == required.platform
                    && row.fixture == required.fixture
                    && row.source == required.source
            })
        })
        .map(|required| format!("{} {} {:?}", required.platform, required.fixture, required.source))
        .collect()
}

/// Validate `start`, a normal start constant at `scale`, against [`START_SIZE_INPUTS`] and
/// [`SIZING_ORACLE_COMPLETE`]. The unit tests and the working-set step both check through this.
///
/// # Errors
///
/// As [`validate_start_dim`].
pub fn validate_table_start(scale: u32, start: u32) -> Result<StartVerdict, String> {
    validate_start_dim(scale, start, START_SIZE_INPUTS, SIZING_ORACLE_COMPLETE)
}

/// The rows recorded at `scale` as [`start_rule`] inputs, each labelled by its identity.
fn rule_inputs(scale: u32, rows: &[StartSizeInput]) -> Vec<RuleInput> {
    rows.iter()
        .filter(|row| row.scale == scale)
        .map(|row| RuleInput {
            label: format!("{} {}x {} {:?}", row.platform, row.scale, row.fixture, row.source),
            outcome: row.outcome,
            max_tile: row.max_tile,
            incomplete_glyphs: row.incomplete_glyphs,
        })
        .collect()
}

#[cfg(test)]
#[path = "start_size_inputs_tests.rs"]
mod start_size_inputs_tests;

/// The face a failed raster was requested from, resolved while its stack was alive: its content
/// identity, its index in a collection, the glyph id and the requested strike, with the face file's
/// name kept for reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFace {
    /// The face file's name, or a `builtin:`/`memory:` name for data not on disk; for reading only,
    /// since two different files can share a name.
    pub file: String,
    /// The content identity of the bytes the face was loaded from
    /// ([`face_content_id`](crate::face_content::face_content_id)); an exception matches on it.
    pub content: String,
    /// Index of the face within its collection file.
    pub face_index: u32,
    /// The glyph id inside that face.
    pub glyph_id: u32,
    /// The requested raster size, in thousandths of a pixel.
    pub strike_px_milli: u64,
}

/// One required glyph a measurement resolved but could not rasterize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterFailure {
    /// The character or cluster lead it drew, kept for reading.
    pub codepoint: char,
    /// The raster role (body, tab title or palette footer) that requested it.
    pub role: GlyphRasterVariant,
    /// Whether the bold face was requested.
    pub bold: bool,
    /// Whether the italic face was requested.
    pub italic: bool,
    /// The resolved face, or `None` when the key resolved to no face; such a failure is never
    /// approved.
    pub face: Option<FailedFace>,
}

/// One reviewed raster failure that may be exempted: an exact platform, raster role, face content and
/// index, glyph id, style and strike, never a family or a file name. Each entry carries the reason
/// reviewed in the PR that added it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterException {
    /// `macos` or `windows`.
    pub platform: &'static str,
    /// The raster role that requested the glyph.
    pub role: GlyphRasterVariant,
    /// The face's content identity, `<namespace>:sha256:<hex>`, as
    /// [`face_content_id`](crate::face_content::face_content_id) computes it.
    pub content: &'static str,
    /// Index of the face within its collection file.
    pub face_index: u32,
    /// The glyph id; never 0.
    pub glyph_id: u32,
    /// Whether the bold face was requested.
    pub bold: bool,
    /// Whether the italic face was requested.
    pub italic: bool,
    /// The requested raster size, in thousandths of a pixel.
    pub strike_px_milli: u64,
    /// The codepoint or cluster, for reading only; it does not take part in matching.
    pub codepoint: &'static str,
    /// Why the failure is acceptable, as reviewed in the PR that added the entry.
    pub reason: &'static str,
}

impl RasterException {
    /// Whether this entry names `failure` on `platform`, field for field.
    fn matches(&self, platform: &str, failure: &RasterFailure) -> bool {
        let Some(face) = failure.face.as_ref() else {
            // When: failure.face is None, the key resolved to no face, which no entry can name.
            return false;
        };
        self.platform == platform
            && self.role == failure.role
            && self.bold == failure.bold
            && self.italic == failure.italic
            && self.content == face.content
            && self.face_index == face.face_index
            && self.glyph_id == face.glyph_id
            && self.strike_px_milli == face.strike_px_milli
    }
}

/// The reviewed raster-failure exceptions. Empty: an entry is added only with a reason reviewed in
/// its PR, and an unknown failure keeps selecting the maximum.
pub const RASTER_EXCEPTIONS: &[RasterException] = &[];

/// A measurement's raster failures, normalized against the reviewed exceptions: the raw list, the
/// failures an exception approved with its reason, and the unapproved rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedFailures {
    /// Every raster failure the measurement recorded.
    pub raw: Vec<RasterFailure>,
    /// Each approved failure with the reason its exception gives.
    pub matched: Vec<(RasterFailure, &'static str)>,
    /// The failures no exception names; each still counts as incomplete.
    pub unapproved: Vec<RasterFailure>,
}

/// Normalize `failures` measured on `platform` against `exceptions`.
#[must_use]
pub fn normalize_raster_failures(
    platform: &str,
    failures: &[RasterFailure],
    exceptions: &[RasterException],
) -> NormalizedFailures {
    let mut normalized =
        NormalizedFailures { raw: failures.to_vec(), matched: Vec::new(), unapproved: Vec::new() };
    for failure in failures {
        match exceptions.iter().find(|entry| entry.matches(platform, failure)) {
            Some(entry) => normalized.matched.push((failure.clone(), entry.reason)),
            None => normalized.unapproved.push(failure.clone()),
        }
    }
    normalized
}

/// A measurement's incomplete count, the rule input's `incomplete_glyphs`: unresolved required
/// characters and oversize required tiles always count, and only approved raster failures do not.
#[must_use]
pub fn incomplete_glyphs(unresolved: usize, oversize: usize, raster: &NormalizedFailures) -> usize {
    unresolved + oversize + raster.unapproved.len()
}

/// Every problem with `exceptions`: an entry that names no glyph (glyph id 0, or a content that is
/// not a face content identity), gives no reason, names an unknown platform, or repeats another entry.
#[must_use]
pub fn exception_problems(exceptions: &[RasterException]) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, entry) in exceptions.iter().enumerate() {
        let known_platform = matches!(entry.platform, "macos" | "windows");
        if entry.glyph_id == 0 || !crate::face_content::is_face_content_id(entry.content) {
            problems.push(format!("entry {index} names no glyph: {entry:?}"));
        } else if entry.reason.trim().is_empty() {
            // When: entry.reason is blank, the entry was never reviewed, so it cannot exempt anything.
            problems.push(format!("entry {index} gives no reason: {entry:?}"));
        } else if !known_platform {
            // When: known_platform is false, entry.platform names no measured host, so nothing matches it.
            problems.push(format!("entry {index} names an unknown platform: {entry:?}"));
        } else if exceptions[..index].contains(entry) {
            // When: an earlier entry equals this one, the list repeats a review.
            problems.push(format!("entry {index} repeats an earlier entry: {entry:?}"));
        }
    }
    problems
}
