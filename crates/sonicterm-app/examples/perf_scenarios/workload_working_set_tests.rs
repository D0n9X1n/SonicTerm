//! Working-set provenance, completeness and exact raster fit contracts.

use super::*;

/// 12c, run by CI on each platform: each start constant passes `validate_table_start`, the same
/// validation the unit tests use, and equals `ruled_start` over the recorded rows once the sizing
/// oracle is complete. For S9's and S12's working sets at scale 1 and 2 the
/// constant is at least this platform's need, where a measurement that drew any glyph as tofu
/// needs the maximum, and the live helper measurement equals this platform's recorded `helper`
/// row when one exists. Prints each figure so CI's output can become the table's rows; every
/// failure is collected and the step fails once at the end, so one run prints every row.
#[test]
#[ignore = "measures with the real font stack; CI runs it in its own step"]
fn glyph_atlas_working_set() {
    use sonicterm_gpu::glyph_working_set::measure_glyph_working_set;
    use sonicterm_text::glyph_atlas::{ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    use sonicterm_text::recording_image::{drift_policy, DriftVerdict, HostImage};
    use sonicterm_text::start_size_inputs::{
        normalize_raster_failures, ruled_start, start_rule, validate_table_start, InputSource,
        RuleInput, RASTER_EXCEPTIONS, SIZING_ORACLE_COMPLETE, START_SIZE_INPUTS,
    };
    let font_dirs =
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    // Every check below records its failure here instead of stopping, so one CI run prints every
    // row; the step fails once, after the last row, naming all of them.
    let mut failures: Vec<String> = Vec::new();
    let size: f32 = FONT_SIZE.parse().unwrap_or_else(|_| {
        failures.push(format!("the scenario font size {FONT_SIZE} is not a number"));
        13.0
    });
    let fixtures = [("S9", Fixture::EmojiCjk), ("S12", Fixture::HistoryScreen)];
    let titles = ["zsh", "perf_scenarios", "S12 covered window"];
    let platform = platform_name();
    // The rows hold exactly on their recording image and on every CI host; elsewhere drift is advisory.
    let policy = drift_policy(platform, &HostImage::from_env());
    if let Some(note) = policy.note() {
        println!("glyph_atlas_working_set note: {note}");
    }
    for (scale, dpi, constant) in [(1, 72, START_ATLAS_DIM_1X), (2, 144, START_ATLAS_DIM_2X)] {
        match validate_table_start(scale, constant) {
            Ok(table) => println!(
                "glyph_atlas_start scale={scale} constant={constant} verdict={}",
                table.verdict
            ),
            Err(error) => {
                failures.push(format!("{scale}x start {constant} fails validation: {error}"))
            }
        }
        if SIZING_ORACLE_COMPLETE {
            // When: SIZING_ORACLE_COMPLETE is set, the constant must equal the recorded rows' rule.
            match ruled_start(scale, START_SIZE_INPUTS, SIZING_ORACLE_COMPLETE) {
                Ok(ruled) => {
                    println!(
                        "glyph_atlas_start scale={scale} constant={constant} ruled={} validated={}",
                        ruled.dim,
                        ruled.dim == constant
                    );
                    if constant != ruled.dim {
                        // When: the constant differs from ruled.dim, the shipped start breaks the rule.
                        failures.push(format!(
                            "{scale}x constant {constant} against the rule {}: {}",
                            ruled.dim, ruled.verdict
                        ));
                    }
                }
                Err(error) => failures.push(format!("{scale}x has no ruled start: {error}")),
            }
        }
        let mut local = Vec::new();
        for (name, fixture) in fixtures {
            let bytes = fixture_bytes(fixture);
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let set = match measure_glyph_working_set(
                &lines,
                &titles,
                FONT_FAMILY,
                size,
                dpi,
                &font_dirs,
            ) {
                Ok(set) => set,
                Err(error) => {
                    // When: the measurement errs, this fixture prints no row; the others still do.
                    failures
                        .push(format!("{platform} {name} at {scale}x did not measure: {error}"));
                    continue;
                }
            };
            // Reviewed exceptions apply here, where each failure still names its face; unresolved
            // characters and oversize required tiles always count.
            let raster = normalize_raster_failures(platform, &set.raster_failed, RASTER_EXCEPTIONS);
            let incomplete_glyphs = sonicterm_text::start_size_inputs::incomplete_glyphs(
                set.unresolved_chars.len(),
                set.oversize_required.len(),
                &raster,
            );
            println!(
                "{}",
                working_set_row(
                    platform,
                    scale,
                    name,
                    "helper",
                    &set.fit_outcome.label(),
                    set.max_tile_dims,
                    set.packed_pixels,
                    incomplete_glyphs
                )
            );
            if incomplete_glyphs > 0 || !raster.raw.is_empty() {
                // When: a required tile is tofu on this host, say which, apart from the row: the raw
                // raster failures, those a reviewed exception approved with its reason, and the rest.
                println!(
                    "glyph_atlas_working_set_tofu platform={platform} scale={scale} fixture={name} \
                     unresolved={:?} oversize_required={:?} raster_failed={} matched={:?} \
                     unapproved={:?}",
                    set.unresolved_chars,
                    set.oversize_required,
                    raster.raw.len(),
                    raster.matched,
                    raster.unapproved
                );
            }
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
                constant < ATLAS_DIM,
                &evidence,
            );
            let provenance =
                recorded.map_or_else(|| "none".to_owned(), |row| row.provenance.describe());
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
                DriftVerdict::Advisory(advisory) => {
                    println!("glyph_atlas_working_set advisory: {advisory}")
                }
                DriftVerdict::Fails(failure) => failures.push(failure),
            }
            // CI seals these rows only after this test returns successfully, including every drift check.
            if let Err(error) = crate::workload::glyph_oracle::export(name, dpi, &set) {
                failures.push(format!("{name}@{scale}: oracle export: {error}"));
            }
            local.push(RuleInput {
                label: format!("{platform} {scale}x {name} helper"),
                outcome: set.fit_outcome,
                max_tile: set.max_tile_dims,
                incomplete_glyphs,
            });
        }
        match start_rule(&local) {
            Ok(need) => {
                println!(
                    "glyph_atlas_working_set platform={platform} scale={scale} need={} \
                     constant={constant} verdict={}",
                    need.dim, need.verdict
                );
                if constant < need.dim {
                    // When: constant is below need.dim, this platform's working set overflows it.
                    failures.push(format!("{scale}x needs {} but starts at {constant}", need.dim));
                }
            }
            Err(error) => failures.push(format!("{scale}x has no measured need: {error}")),
        }
    }
    if !failures.is_empty() {
        // When: any check failed, the step fails once with every failure after every row.
        panic!("glyph_atlas_working_set: {} failures:\n{}", failures.len(), failures.join("\n"));
    }
}

/// One run of the working-set step always prints every row: no check panics or asserts while the
/// fixtures and scales are still being measured; every failure is collected and the step panics
/// once, after the last row, naming all of them.
#[test]
fn the_working_set_step_prints_every_row_before_its_one_panic() {
    let source = concat!(
        include_str!("workload_working_set.rs"),
        "\n",
        include_str!("workload_working_set_tests.rs"),
        "\n",
        include_str!("workload_renderer_fixture.rs")
    )
    .replace("\r\n", "\n");
    let start = source.find("fn glyph_atlas_working_set() {").expect("the step");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("its end")];
    for check in ["assert!(", "assert_eq!(", ".expect(", ".unwrap()"] {
        assert!(!body.contains(check), "{check} would stop the step before every row prints");
    }
    assert_eq!(body.matches("panic!(").count(), 1, "one panic");
    let panic = body.find("panic!(").unwrap();
    let last_row = body.rfind("println!(").expect("rows are printed");
    assert!(last_row < panic, "the panic follows every printed row");
}

/// The working-set step tells an incomplete live measurement apart from sizing drift. A complete
/// measurement equal to its row matches, and one whose figures changed is drift. A measurement that
/// drew a required glyph as tofu is refused before any comparison, because its fit and largest tile
/// describe a smaller set than the working set; it is never reported as drift and never passes.
#[test]
fn an_incomplete_helper_measurement_is_refused_before_the_drift_check() {
    use sonicterm_text::glyph_atlas::FitOutcome;
    let row = (FitOutcome::Fits(1024), [23, 20], 0);
    assert_eq!(check_helper(Some(row), row, true, "none"), HelperCheck::Matches);
    let drifted = (FitOutcome::Fits(2048), [23, 20], 0);
    let HelperCheck::Drifted(reason) = check_helper(Some(row), drifted, true, "none") else {
        panic!("a complete measurement with other figures is drift");
    };
    assert!(reason.contains("Fits(2048)"), "{reason}");
    // The figures also differ here, but the missing glyphs decide: refusal, not drift.
    let incomplete = (FitOutcome::Fits(1024), [23, 16], 60);
    for (recorded, below) in [(Some(row), true), (Some(row), false), (None, true)] {
        let HelperCheck::Refused(reason) = check_helper(recorded, incomplete, below, "U+1F600")
        else {
            panic!("an incomplete measurement was not refused ({recorded:?}, below {below})");
        };
        assert!(reason.contains("60 required glyphs") && reason.contains("U+1F600"), "{reason}");
    }
    // With no recorded row and the start already the maximum, the conservative rule decides.
    assert_eq!(check_helper(None, incomplete, false, "U+1F600"), HelperCheck::MaximumOnly);
}

/// Every representative codepoint is drawn by S9: each appears on the fixture's last line, so the
/// frozen list cannot drift from the fixture it stands for.
#[test]
fn the_s9_representative_codepoints_are_on_the_fixtures_last_line() {
    let bytes = fixture_bytes(crate::scenarios::Fixture::EmojiCjk);
    let text = String::from_utf8(bytes).expect("the S9 fixture is UTF-8");
    let last = text.lines().rev().find(|line| !line.trim().is_empty()).expect("a last line");
    let absent: Vec<char> = S9_REPRESENTATIVE_CODEPOINTS
        .iter()
        .copied()
        .filter(|codepoint| !last.contains(*codepoint))
        .collect();
    assert!(absent.is_empty(), "not on the last line {last:?}: {absent:?}");
}

/// The working-set step and the real-renderer coverage test print their measurements through one
/// formatter, so a single `key=value` parser reads both and the rows can become `START_SIZE_INPUTS`
/// entries: the same keys in the same order, told apart only by `source`. Both kinds carry
/// `incomplete_glyphs`, the count that makes a row ineligible to select a smaller start.
#[test]
fn working_set_rows_share_one_parseable_format() {
    use sonicterm_text::glyph_atlas::FitOutcome;
    let parse = |line: &str| -> Vec<(String, String)> {
        let fields = line.strip_prefix("glyph_atlas_working_set ").expect("the row prefix");
        fields
            .split(' ')
            .map(|field| {
                let (key, value) = field.split_once('=').expect("a key=value field");
                (key.to_owned(), value.to_owned())
            })
            .collect()
    };
    let helper = parse(&working_set_row(
        "windows",
        1,
        "S9",
        "helper",
        &FitOutcome::Fits(512).label(),
        [18, 24],
        9000,
        3,
    ));
    let real = parse(&working_set_row(
        "windows",
        2,
        "S12",
        "real_renderer",
        &FitOutcome::FitsWithoutHeadroom.label(),
        [40, 48],
        123_456,
        0,
    ));
    let keys =
        |row: &[(String, String)]| row.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
    assert_eq!(
        keys(&helper),
        [
            "platform",
            "scale",
            "fixture",
            "source",
            "fit",
            "max_tile",
            "packed_pixels",
            "incomplete_glyphs"
        ]
    );
    assert_eq!(keys(&helper), keys(&real));
    assert_eq!(helper[3].1, "helper");
    assert_eq!(real[3].1, "real_renderer");
    assert_eq!((real[4].1.as_str(), real[5].1.as_str()), ("no_headroom", "40x48"));
    assert_eq!((helper[7].1.as_str(), real[7].1.as_str()), ("3", "0"));
}

/// Fallback is complete only on a presented frame whose terminal rows and chrome both drew no
/// tofu: a footer still waiting on its fallback face keeps the check waiting even though the
/// terminal rows are complete, which a terminal-only check would have accepted.
#[test]
fn fallback_settles_only_when_rows_and_chrome_drew_no_tofu() {
    assert!(fallback_settled(true, &[], &[]));
    assert!(!fallback_settled(true, &[], &['↵']), "a footer awaiting its face is not settled");
    assert!(!fallback_settled(true, &['界'], &[]));
    assert!(!fallback_settled(false, &[], &[]), "a frame that did not present proves nothing");
}

/// The coverage test compares tiles only after the chrome it exercises is resident: every footer
/// key-hint symbol under `PaletteFooter` and the short tab title under `TabTitle`, each as a real
/// tile (missing sentinels are not resident). A symbol drawn only at another variant still counts
/// as missing.
#[test]
fn required_chrome_symbols_are_checked_per_raster_variant() {
    use sonicterm_types::{GlyphKey, GlyphRasterVariant};
    // Any real shaped key stands in for a resident tile; only the character and variant matter.
    let key = |symbol: char, variant| {
        GlyphKey::shaped(symbol, 1, 7, false, false).with_raster_variant(variant)
    };
    let mut resident: std::collections::HashSet<GlyphKey> = REQUIRED_FOOTER_SYMBOLS
        .chars()
        .map(|symbol| key(symbol, GlyphRasterVariant::PaletteFooter))
        .chain(REQUIRED_TITLE_TEXT.chars().map(|symbol| key(symbol, GlyphRasterVariant::TabTitle)))
        .collect();
    assert!(missing_required_chrome(&resident).is_empty());
    resident.remove(&key('↵', GlyphRasterVariant::PaletteFooter));
    resident.insert(key('↵', GlyphRasterVariant::Normal));
    assert_eq!(missing_required_chrome(&resident), vec![(GlyphRasterVariant::PaletteFooter, '↵')]);
}

/// The real-renderer coverage test seeds its main window under a synthetic key and then attaches a
/// native window with another id, so naming that key frontmost reads as a torn-out child and the
/// palette attaches there: the main frame never draws it, and no footer key becomes resident. The
/// shared setup must open the palette on the main frame (no attached window), on every host.
#[test]
fn the_coverage_setup_opens_the_palette_on_the_main_frame() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let fresh = || {
        let mut app =
            sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("zsh");
        app
    };
    // The mechanism: the seeded main key named frontmost routes the palette away from main.
    let mut misrouted = fresh();
    let main = misrouted.__test_main_window_id().expect("a seeded main window");
    misrouted.__test_set_frontmost_window(Some(main));
    assert!(misrouted.run_action(&sonicterm_cfg::keymap::Action::OpenCommandPalette));
    assert_eq!(misrouted.__test_palette_attached_window(), Some(main));
    // The setup the coverage test uses draws it on the main frame.
    let mut app = fresh();
    open_palette_on_main_frame(&mut app).expect("the palette opens on the main frame");
    assert!(app.__test_palette_open());
    assert_eq!(app.__test_palette_attached_window(), None);
}
