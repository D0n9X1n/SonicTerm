#![cfg(test)]
//! Test-only glyph working-set and native renderer coverage fixtures.

use super::*;

/// The running platform's name as `START_SIZE_INPUTS` rows record it.
fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        // When: neither macos nor windows matches, the platform has no recorded rows.
        "linux"
    }
}

/// One working-set measurement as the parseable row CI output is recorded from: `key=value`
/// fields after a fixed prefix, in the order `START_SIZE_INPUTS` rows are keyed, with `source`
/// naming what measured it (`helper` or `real_renderer`). `fit` is `FitOutcome::label`, which
/// the helper and the renderer's `GlyphAtlasFacts::fit` both produce. `incomplete_glyphs`
/// counts required glyphs the measurement drew as tofu; a nonzero count makes the row ineligible
/// to select a start below the maximum.
#[allow(clippy::too_many_arguments)]
fn working_set_row(
    platform: &str,
    scale: u32,
    fixture: &str,
    source: &str,
    fit: &str,
    max_tile: [u32; 2],
    packed_pixels: u64,
    incomplete_glyphs: usize,
) -> String {
    format!(
        "glyph_atlas_working_set platform={platform} scale={scale} fixture={fixture} \
         source={source} fit={fit} max_tile={}x{} packed_pixels={packed_pixels} \
         incomplete_glyphs={incomplete_glyphs}",
        max_tile[0], max_tile[1]
    )
}

/// A helper measurement's sizing figures: its fit outcome, its largest tile and how many required
/// glyphs it drew as tofu after reviewed exceptions.
type HelperFigures = (sonicterm_text::glyph_atlas::FitOutcome, [u32; 2], usize);

/// How the working-set step judges one live helper measurement against its recorded row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HelperCheck {
    /// Complete, and equal to its recorded row (or no row is recorded).
    Matches,
    /// Complete, but its sizing figures differ from the recorded row; the message names both.
    Drifted(String),
    /// Some required glyph was not drawn, so the measurement cannot validate the recorded rows or a
    /// start below the maximum; the message names the refusal and the missing evidence.
    Refused(String),
    /// Some required glyph was not drawn, but there is no recorded row and the start is already the
    /// maximum, which the conservative rule selects for an incomplete measurement anyway.
    MaximumOnly,
}

/// Judge `live` against `recorded`, the helper row recorded for the same platform, scale and
/// fixture. `start_below_maximum` says whether the shipped start at this scale is below the maximum;
/// `evidence` describes the measurement's missing glyphs for a refusal.
fn check_helper(
    recorded: Option<HelperFigures>,
    live: HelperFigures,
    start_below_maximum: bool,
    evidence: &str,
) -> HelperCheck {
    let incomplete_glyphs = live.2;
    if incomplete_glyphs > 0 {
        // When: incomplete_glyphs is nonzero the figures describe less than the working set.
        if recorded.is_some() || start_below_maximum {
            // When: a recorded row or a smaller start depends on this measurement, it is refused.
            return HelperCheck::Refused(format!(
                "incomplete measurement: {incomplete_glyphs} required glyphs were not drawn, so it \
                 cannot validate the recorded helper row or a start below the maximum; {evidence}"
            ));
        }
        return HelperCheck::MaximumOnly;
    }
    match recorded {
        Some(row) if row != live => {
            HelperCheck::Drifted(format!("recorded {row:?}, live {live:?}"))
        }
        _ => HelperCheck::Matches,
    }
}

/// One helper measurement judged as the working-set gate judges it.
struct HelperJudgement {
    /// Required glyphs the measurement could not draw, after reviewed raster exceptions.
    incomplete_glyphs: usize,
    /// The measurement's raster failures, split by the reviewed exceptions.
    raster: sonicterm_text::start_size_inputs::NormalizedFailures,
    /// Drift notes that do not fail on this host.
    advisories: Vec<String>,
    /// Every check that failed.
    failures: Vec<String>,
}

/// Judge one helper measurement of fixture `name` at `scale` on `platform` as the working-set gate
/// does: count its undrawable glyphs, refuse an incomplete measurement a recorded row or a start
/// below the maximum (`constant`) depends on, and judge drift from the recorded row under `policy`.
fn judge_helper(
    set: &sonicterm_gpu::glyph_working_set::GlyphWorkingSet,
    platform: &str,
    name: &str,
    scale: u32,
    constant: u32,
    policy: &sonicterm_text::recording_image::DriftPolicy,
) -> HelperJudgement {
    use sonicterm_text::recording_image::DriftVerdict;
    use sonicterm_text::start_size_inputs::{
        normalize_raster_failures, InputSource, RASTER_EXCEPTIONS, START_SIZE_INPUTS,
    };
    // Reviewed exceptions apply here, where each failure still names its face; unresolved
    // characters and oversize required tiles always count.
    let raster = normalize_raster_failures(platform, &set.raster_failed, RASTER_EXCEPTIONS);
    let incomplete_glyphs = sonicterm_text::start_size_inputs::incomplete_glyphs(
        set.unresolved_chars.len(),
        set.oversize_required.len(),
        &raster,
    );
    let recorded = START_SIZE_INPUTS.iter().find(|row| {
        row.platform == platform
            && row.scale == scale
            && row.fixture == name
            && row.source == InputSource::Helper
    });
    // An incomplete measurement is refused before the drift check, with the failed face,
    // glyph, style and strike of every unapproved raster failure; it never passes.
    let evidence = format!(
        "unresolved={:?} oversize_required={:?} unapproved={:?}",
        set.unresolved_chars, set.oversize_required, raster.unapproved
    );
    let check = check_helper(
        recorded.map(|row| (row.outcome, row.max_tile, row.incomplete_glyphs)),
        (set.fit_outcome, set.max_tile_dims, incomplete_glyphs),
        constant < sonicterm_text::glyph_atlas::ATLAS_DIM,
        &evidence,
    );
    let provenance = recorded.map_or_else(|| "none".to_owned(), |row| row.provenance.describe());
    let mut advisories = Vec::new();
    let mut failures = Vec::new();
    // A refusal fails on every host; only a dimension drift depends on the host image.
    let drift = match check {
        HelperCheck::Matches | HelperCheck::MaximumOnly => None,
        HelperCheck::Drifted(reason) => Some(format!(
            "{name} at {scale}x drifted from its recorded helper row ({provenance}): {reason}"
        )),
        HelperCheck::Refused(reason) => {
            failures.push(format!(
                "glyph_atlas_working_set refused: {platform} {name} at {scale}x (recorded row: \
                 {provenance}): {reason}"
            ));
            None
        }
    };
    match policy.judge(drift.as_deref()) {
        DriftVerdict::Holds => {}
        DriftVerdict::Advisory(advisory) => advisories.push(advisory),
        DriftVerdict::Fails(failure) => failures.push(failure),
    }
    HelperJudgement { incomplete_glyphs, raster, advisories, failures }
}

/// Check a helper a suite measured itself, because the oracle producer ran on another runner
/// image, as the producer checks its own before publishing: the shared judgement, then the start
/// constant for `scale` must hold this measurement's need. Returns the drift advisories to print.
///
/// # Errors
///
/// Names every failed check.
fn validate_local_helper(
    set: &sonicterm_gpu::glyph_working_set::GlyphWorkingSet,
    platform: &str,
    name: &str,
    scale: u32,
    policy: &sonicterm_text::recording_image::DriftPolicy,
) -> Result<Vec<String>, String> {
    use sonicterm_text::glyph_atlas::{START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    use sonicterm_text::start_size_inputs::{start_rule, RuleInput};
    let constant = if scale == 1 { START_ATLAS_DIM_1X } else { START_ATLAS_DIM_2X };
    let judgement = judge_helper(set, platform, name, scale, constant, policy);
    if !judgement.failures.is_empty() {
        // When: failures is not empty the measurement fails a check the producer would refuse.
        return Err(format!("the local helper fails its gate: {:?}", judgement.failures));
    }
    let need = start_rule(&[RuleInput {
        label: format!("{platform} {scale}x {name} local helper"),
        outcome: set.fit_outcome,
        max_tile: set.max_tile_dims,
        incomplete_glyphs: judgement.incomplete_glyphs,
    }])?;
    if constant < need.dim {
        // When: constant is below need.dim the shipped start cannot hold this working set.
        return Err(format!("the local helper needs {} but starts at {constant}", need.dim));
    }
    Ok(judgement.advisories)
}

/// Whether fallback is complete: the frame presented, and neither its terminal rows nor its
/// chrome (tab titles, palette, footer) drew a character as tofu.
fn fallback_settled(presented: bool, terminal_missing: &[char], chrome_missing: &[char]) -> bool {
    presented && terminal_missing.is_empty() && chrome_missing.is_empty()
}

/// The palette footer's key-hint symbols the coverage test requires as `PaletteFooter` tiles.
const REQUIRED_FOOTER_SYMBOLS: &str = "·↑↓↵";
/// The tab title the coverage test requires as `TabTitle` tiles; short enough never to truncate.
const REQUIRED_TITLE_TEXT: &str = "zsh";

/// Each required chrome character with no real resident tile at its raster variant, as
/// `(variant, character)`. Missing sentinels are not resident, so a symbol drawn as tofu is listed.
fn missing_required_chrome(
    resident: &std::collections::HashSet<sonicterm_types::GlyphKey>,
) -> Vec<(sonicterm_types::GlyphRasterVariant, char)> {
    use sonicterm_types::GlyphRasterVariant;
    let required = REQUIRED_FOOTER_SYMBOLS
        .chars()
        .map(|symbol| (GlyphRasterVariant::PaletteFooter, symbol))
        .chain(REQUIRED_TITLE_TEXT.chars().map(|symbol| (GlyphRasterVariant::TabTitle, symbol)));
    required
        .filter(|&(variant, symbol)| {
            !resident.iter().any(|key| key.raster_variant == variant && key.ch == symbol)
        })
        .collect()
}

/// Representative S9 emoji and CJK scalars, taken from the fixture's last line, which the real
/// renderer's viewport shows at both scales (the cursor sits on the row below it). Test 17 checks
/// that a resident body tile draws each with pixels, whatever fallback face drew it.
const S9_REPRESENTATIVE_CODEPOINTS: [char; 6] = ['😀', '🍣', '🌏', '漢', '字', 'あ'];

/// Open the command palette so the main frame draws it: no window is named frontmost, which routes
/// the palette to main (no attached window). Naming the seeded main key would misroute it, since the
/// attached native window carries another id. Fails when the palette is not open on main.
fn open_palette_on_main_frame(app: &mut sonicterm_app::app::App) -> Result<(), String> {
    app.__test_set_frontmost_window(None);
    if !app.run_action(&sonicterm_cfg::keymap::Action::OpenCommandPalette) {
        // When: run_action refuses the action, the palette cannot be measured.
        return Err("the palette open action was refused".to_owned());
    }
    if !app.__test_palette_open() {
        // When: the action ran but the palette is closed, nothing draws a footer.
        return Err("the palette is not open".to_owned());
    }
    match app.__test_palette_attached_window() {
        None => Ok(()),
        // When: an attached window is named, the main frame does not draw the palette.
        Some(window_id) => {
            Err(format!("the palette attached to {window_id:?}, not the main frame"))
        }
    }
}

/// The working-set helper against the real renderer, compared by tile identity rather than by
/// configuration-local glyph keys: the S9 and S12 fixtures drawn on a Windows
/// window with the tab bar and three titles, the cursor and the command palette open with its
/// footer and detail rows, at scale 1 and 2. It lives beside the fixtures it draws.
///
/// What it proves: the completion wait needs a presented frame whose terminal rows and chrome
/// report no missing glyph, and both reports now include a shaped glyph that drew nothing (refused,
/// rasterized nothing, or too large to place) and a tab title that never shaped. Every resident
/// tile is in the helper's set at the same raster size, and for S9 each representative emoji and
/// CJK codepoint is a resident real tile. It does not prove that every glyph the helper measures
/// was drawn, only the glyphs this frame requested. These checks passed on Windows CI and their
/// rows are recorded, which is what completes the sizing oracle and lets a start constant drop
/// below 2048.
#[cfg(target_os = "windows")]
#[path = "workload_renderer_fixture.rs"]
mod real_renderer_coverage;

#[cfg(test)]
#[path = "workload_working_set_tests.rs"]
mod workload_working_set_tests;
