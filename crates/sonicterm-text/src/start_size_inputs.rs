//! Measured inputs that choose the glyph atlas start size, and the pure rule over them.
//!
//! Each row is one CI measurement: a platform, a scale, a fixture and the source that measured it,
//! with the fit outcome and the largest tile it saw. [`start_rule`] turns a set of rows into a start
//! dimension; `START_ATLAS_DIM_1X` and `START_ATLAS_DIM_2X` must equal it over this table's rows at
//! each scale. While the table holds no row for a scale, that scale starts at the maximum.

use crate::glyph_atlas::{FitOutcome, ATLAS_DIM, MIN_ATLAS_DIM};

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
    /// The CI run that produced the row.
    pub run_url: &'static str,
}

/// Every measured input. Empty until CI measurements are recorded, so both scales start at the
/// maximum and no memory is saved yet.
pub const START_SIZE_INPUTS: &[StartSizeInput] = &[];

/// A start dimension and the reason the rule chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartVerdict {
    /// The chosen square start dimension in pixels.
    pub dim: u32,
    /// Human-readable reason, naming the input that decided it.
    pub verdict: String,
}

/// One input to [`start_rule`]: a label naming its source, its outcome and its largest tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInput {
    /// Names the measurement in the verdict.
    pub label: String,
    /// Fit outcome the measurement reported.
    pub outcome: FitOutcome,
    /// Largest resident tile width and height.
    pub max_tile: [u32; 2],
}

/// Choose a start dimension from measured inputs at one scale.
///
/// The result is the largest `Fits(dim)` among the inputs, raised to the smallest power of two that
/// holds every input's largest tile. Any `FitsWithoutHeadroom`, `DoesNotFit` or `Evicted` input
/// selects the maximum, and the verdict names it.
///
/// # Errors
///
/// When `inputs` is empty: no measurement means no rule, never a silent default.
pub fn start_rule(inputs: &[RuleInput]) -> Result<StartVerdict, String> {
    if inputs.is_empty() {
        // When: inputs is empty there is nothing measured, so the caller must decide the default.
        return Err("no measured input".to_owned());
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

/// The start dimension [`START_SIZE_INPUTS`] selects at `scale`: the rule over that scale's rows,
/// or the maximum with "no savings" when the table holds none.
#[must_use]
pub fn table_start_dim(scale: u32) -> StartVerdict {
    let inputs: Vec<RuleInput> = START_SIZE_INPUTS
        .iter()
        .filter(|row| row.scale == scale)
        .map(|row| RuleInput {
            label: format!("{} {}x {} {:?}", row.platform, row.scale, row.fixture, row.source),
            outcome: row.outcome,
            max_tile: row.max_tile,
        })
        .collect();
    start_rule(&inputs).unwrap_or_else(|_| StartVerdict {
        dim: ATLAS_DIM,
        verdict: format!("no measured {scale}x input: maximum selected, no savings"),
    })
}

#[cfg(test)]
#[path = "start_size_inputs_tests.rs"]
mod start_size_inputs_tests;
