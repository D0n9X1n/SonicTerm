//! Measured inputs that may choose the glyph atlas start size, and the pure rules over them.
//!
//! Each row is one CI measurement: a platform, a scale, a fixture and the source that measured it,
//! with the fit outcome, the largest tile it saw and how many required glyphs it drew as tofu.
//! [`validate_start_dim`] is the one check every start constant passes: the maximum is always valid
//! because it claims no savings, and a smaller start is valid only when [`SIZING_ORACLE_COMPLETE`]
//! is true, every input [`required_inputs`] names is recorded at that scale, and [`start_rule`]
//! over the scale's rows selects exactly that start. An empty or incomplete table is therefore the
//! conservative contract: both normal start constants stay at the maximum.

use crate::glyph_atlas::{FitOutcome, ATLAS_DIM, MIN_ATLAS_DIM};

/// Whether the measurement oracle can prove that a renderer drew its whole working set, which a
/// start below the maximum needs. It is false because two failures are still not reported as
/// missing glyphs: a nonzero shaped glyph id whose raster or atlas admission fails is skipped
/// silently, and a tab title whose fitting fails becomes an empty title before the chrome
/// diagnostic sees it. Both must be recorded as missing, with regression tests, and the CI rows
/// recorded in [`START_SIZE_INPUTS`], before this becomes true. Rows alone never lower a start.
pub const SIZING_ORACLE_COMPLETE: bool = false;

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
    /// The CI run that produced the row.
    pub run_url: &'static str,
}

/// Every measured input. Empty: no CI row is recorded, so both normal start constants must be the
/// maximum, which [`validate_table_start`] enforces.
pub const START_SIZE_INPUTS: &[StartSizeInput] = &[];

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
                // When: scale is 1 the perf scenarios measured this fixture's end-of-run atlas.
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
    let missing: Vec<String> = required_inputs(scale)
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
        .collect();
    if !missing.is_empty() {
        // When: a required input is unrecorded the rows cannot justify a start below the maximum.
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
