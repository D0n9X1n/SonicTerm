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
use sonicterm_types::GlyphRasterVariant;

/// Whether the measurement oracle can prove that a renderer drew its whole working set, which a
/// start below the maximum needs. The renderer now reports shaped glyphs that draw nothing and tab
/// titles that never shape as missing glyphs; this stays false until the strengthened real-renderer
/// coverage test passes on Windows CI and the CI rows are recorded in [`START_SIZE_INPUTS`]. Rows
/// alone never lower a start.
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

/// The face a failed raster was requested from, resolved while its stack was alive: the face file's
/// name (not its host path), its index in a collection, the glyph id and the requested strike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFace {
    /// The face file's name, or a `builtin:`/`memory:` name for data not on disk.
    pub file: String,
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

/// One reviewed raster failure that may be exempted: an exact platform, raster role, face file and
/// index, glyph id, style and strike, never a family. Each entry carries the reason reviewed in the
/// PR that added it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterException {
    /// `macos` or `windows`.
    pub platform: &'static str,
    /// The raster role that requested the glyph.
    pub role: GlyphRasterVariant,
    /// The face file's name.
    pub file: &'static str,
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
            && self.file == face.file
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

/// Every problem with `exceptions`: an entry that names no glyph (glyph id 0 or no file), gives no
/// reason, names an unknown platform, or repeats another entry.
#[must_use]
pub fn exception_problems(exceptions: &[RasterException]) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, entry) in exceptions.iter().enumerate() {
        let known_platform = matches!(entry.platform, "macos" | "windows");
        if entry.glyph_id == 0 || entry.file.trim().is_empty() {
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
