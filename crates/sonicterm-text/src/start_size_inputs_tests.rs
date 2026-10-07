use std::sync::LazyLock;

use super::*;
use crate::face_content::{face_content_id, FaceNamespace};
use crate::glyph_atlas::{START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};

/// A complete rule input labelled `label` with `outcome` and a square largest tile of `tile` pixels.
fn input(label: &str, outcome: FitOutcome, tile: u32) -> RuleInput {
    RuleInput { label: label.to_owned(), outcome, max_tile: [tile, tile], incomplete_glyphs: 0 }
}

/// One recorded row for every input [`required_inputs`] names at `scale`, each fitting `fit_dim`
/// with a small tile and every required glyph drawn.
fn complete_rows(scale: u32, fit_dim: u32) -> Vec<StartSizeInput> {
    required_inputs(scale)
        .into_iter()
        .map(|required| StartSizeInput {
            platform: required.platform,
            scale,
            fixture: required.fixture,
            source: required.source,
            outcome: FitOutcome::Fits(fit_dim),
            max_tile: [24, 24],
            incomplete_glyphs: 0,
            packed_pixels: 0,
            provenance: Provenance {
                run_id: 1,
                attempt: 1,
                measured_sha: "0000000000000000000000000000000000000000",
                side: "push",
                set: "test",
                origin: "test",
            },
        })
        .collect()
}

/// The rule picks the largest fit, raised by the largest tile, and names its deciding input.
#[test]
fn start_rule_takes_the_largest_fit_raised_by_the_largest_tile() {
    let all_small =
        start_rule(&[input("a", FitOutcome::Fits(256), 20), input("b", FitOutcome::Fits(256), 30)]);
    assert_eq!(all_small.as_ref().map(|verdict| verdict.dim), Ok(256));
    assert_eq!(all_small.unwrap().verdict, "a: fits 256");
    let mixed = start_rule(&[
        input("s9", FitOutcome::Fits(512), 20),
        input("s12", FitOutcome::Fits(1024), 30),
    ])
    .unwrap();
    assert_eq!(mixed, StartVerdict { dim: 1024, verdict: "s12: fits 1024".to_owned() });
    // A 600-pixel tile cannot sit in a 512 atlas, so the start rises to the power of two holding it.
    let tall = start_rule(&[input("emoji", FitOutcome::Fits(512), 600)]).unwrap();
    assert_eq!(
        tall,
        StartVerdict {
            dim: 1024,
            verdict: "emoji fits 512; max tile 600 raises it to 1024".to_owned()
        }
    );
}

/// Any outcome without a fit selects the maximum and names the input; an empty list is an error.
#[test]
fn start_rule_selects_the_maximum_for_any_bad_outcome_and_refuses_no_input() {
    for bad in [FitOutcome::FitsWithoutHeadroom, FitOutcome::DoesNotFit, FitOutcome::Evicted] {
        let verdict =
            start_rule(&[input("ok", FitOutcome::Fits(256), 10), input("bad", bad, 10)]).unwrap();
        assert_eq!(verdict.dim, ATLAS_DIM, "{bad:?}");
        assert_eq!(verdict.verdict, format!("bad: {} selects {ATLAS_DIM}", bad.label()));
    }
    assert_eq!(start_rule(&[]), Err("no measured input".to_owned()));
}

/// A measurement that drew any required glyph as tofu (unresolved, or resolved but not
/// rasterized) is not a complete working set, so the rule selects the maximum and names it, even
/// when its fit outcome alone would have chosen the smallest start.
#[test]
fn start_rule_selects_the_maximum_for_an_incomplete_measurement() {
    let mut emoji = input("S9 helper", FitOutcome::Fits(256), 20);
    emoji.incomplete_glyphs = 3;
    let verdict = start_rule(&[input("S12 helper", FitOutcome::Fits(256), 20), emoji]).unwrap();
    assert_eq!(
        verdict,
        StartVerdict {
            dim: ATLAS_DIM,
            verdict: format!("S9 helper: 3 unresolved or unrasterized glyphs select {ATLAS_DIM}"),
        }
    );
}

/// The required inputs are the spec's list: at scale 1 the perf end-of-scenario atlas, the helper
/// on both platforms and the Windows real renderer, for S9 and S12; at scale 2 the same without
/// the perf rows, since the perf scenarios run at scale 1 only.
#[test]
fn required_inputs_name_every_platform_fixture_and_source() {
    let count = |scale, source| {
        required_inputs(scale).iter().filter(|required| required.source == source).count()
    };
    assert_eq!(required_inputs(1).len(), 10);
    assert_eq!((count(1, InputSource::PerfS9End), count(1, InputSource::PerfS12End)), (2, 2));
    assert_eq!((count(1, InputSource::Helper), count(1, InputSource::RealRenderer)), (4, 2));
    assert_eq!(required_inputs(2).len(), 6);
    assert_eq!((count(2, InputSource::Helper), count(2, InputSource::RealRenderer)), (4, 2));
    assert!(required_inputs(2).iter().all(|required| !matches!(
        required.source,
        InputSource::PerfS9End | InputSource::PerfS12End
    )));
    assert!(required_inputs(1)
        .iter()
        .filter(|required| required.source == InputSource::RealRenderer)
        .all(|required| required.platform == "windows"));
}

/// The maximum start claims no savings, so it is valid with no recorded row at either scale and
/// whatever the oracle guard says; the verdict says that nothing is saved.
#[test]
fn the_maximum_start_is_valid_with_no_measured_rows() {
    for oracle_complete in [false, true] {
        for scale in [1, 2] {
            let verdict = validate_start_dim(scale, ATLAS_DIM, &[], oracle_complete)
                .unwrap_or_else(|error| panic!("{scale}x maximum rejected: {error}"));
            assert_eq!(verdict.dim, ATLAS_DIM);
            assert!(verdict.verdict.contains("no savings"), "{}", verdict.verdict);
        }
    }
}

/// A start below the maximum with no recorded row fails validation at both scales, whatever the
/// oracle guard says: an empty table justifies only the maximum.
#[test]
fn a_start_below_the_maximum_fails_without_measured_rows() {
    for oracle_complete in [false, true] {
        for scale in [1, 2] {
            for start in [256, 512, 1024] {
                assert!(
                    validate_start_dim(scale, start, &[], oracle_complete).is_err(),
                    "{scale}x start {start} accepted with no rows (oracle {oracle_complete})"
                );
            }
        }
    }
}

/// Recording every required row does not lower a start while the oracle guard is false: the
/// guard alone decides here, since the same rows validate once it is true.
#[test]
fn complete_rows_do_not_lower_the_start_while_the_oracle_is_incomplete() {
    for scale in [1, 2] {
        let rows = complete_rows(scale, 256);
        let refused = validate_start_dim(scale, 256, &rows, false)
            .expect_err("complete rows lowered the start with the oracle incomplete");
        assert!(refused.contains("oracle"), "{refused}");
        assert_eq!(validate_start_dim(scale, 256, &rows, true).map(|verdict| verdict.dim), Ok(256));
    }
}

/// A recorded measurement whose helper drew glyphs as tofu, such as emoji that failed to
/// rasterize, cannot justify a smaller start even with the oracle complete: validation rejects
/// the smaller start, and the maximum it accepts names the incomplete row.
#[test]
fn a_measurement_with_unrasterized_glyphs_cannot_select_a_smaller_start() {
    for scale in [1, 2] {
        let mut rows = complete_rows(scale, 256);
        let s9_helper = rows
            .iter_mut()
            .find(|row| row.fixture == "S9" && row.source == InputSource::Helper)
            .expect("a required S9 helper row");
        s9_helper.incomplete_glyphs = 2;
        let refused = validate_start_dim(scale, 256, &rows, true)
            .expect_err("an incomplete measurement selected a smaller start");
        assert!(refused.contains("unresolved or unrasterized"), "{refused}");
        let maximum = validate_start_dim(scale, ATLAS_DIM, &rows, true).expect("the maximum");
        assert!(maximum.verdict.contains("2 unresolved or unrasterized"), "{}", maximum.verdict);
    }
}

/// A smaller start needs every required input at its scale: dropping one required row, or
/// recording it only at the other scale, fails and names the missing input.
#[test]
fn a_start_below_the_maximum_needs_every_required_input() {
    let mut rows = complete_rows(1, 256);
    let real = rows.iter().position(|row| row.source == InputSource::RealRenderer).unwrap();
    let mut moved = rows.remove(real);
    let refused = validate_start_dim(1, 256, &rows, true).expect_err("a missing input accepted");
    assert!(refused.contains("windows S9 RealRenderer"), "{refused}");
    moved.scale = 2;
    rows.push(moved);
    assert!(validate_start_dim(1, 256, &rows, true).is_err(), "another scale's row counted");
}

/// A smaller start must equal what the rule selects over its scale's rows: neither more nor less.
#[test]
fn a_start_below_the_maximum_must_equal_the_rule() {
    let rows = complete_rows(1, 512);
    assert_eq!(validate_start_dim(1, 512, &rows, true).map(|verdict| verdict.dim), Ok(512));
    for start in [256, 1024] {
        let refused = validate_start_dim(1, start, &rows, true).expect_err("a start off the rule");
        assert!(refused.contains("fits 512"), "{refused}");
    }
}

/// The shipped start constants pass the same validation as every case above, over the recorded
/// table and the shipped oracle guard.
#[test]
fn start_constants_pass_the_table_validation() {
    for (scale, constant) in [(1, START_ATLAS_DIM_1X), (2, START_ATLAS_DIM_2X)] {
        let verdict = validate_table_start(scale, constant)
            .unwrap_or_else(|error| panic!("{scale}x start {constant} rejected: {error}"));
        assert_eq!(verdict.dim, constant);
    }
}

/// The real check on the shipped constants: once the oracle is complete, each normal start constant
/// **equals** what the rule selects over the recorded rows at its scale. Validation alone accepts
/// the maximum unconditionally, so a constant left at 2048 where the rows rule smaller, or lowered
/// below what they rule, would pass it; this test fails both.
#[test]
fn each_start_constant_equals_the_rule_over_the_recorded_rows() {
    for (scale, constant) in [(1, START_ATLAS_DIM_1X), (2, START_ATLAS_DIM_2X)] {
        let ruled = ruled_start(scale, START_SIZE_INPUTS, SIZING_ORACLE_COMPLETE)
            .unwrap_or_else(|error| panic!("{scale}x has no ruled start: {error}"));
        assert_eq!(constant, ruled.dim, "{scale}x constant against the rule: {}", ruled.verdict);
    }
}

/// The rule's result depends on the rows that set each scale's largest fit: dropping any one of
/// them either loses a required input or lowers the ruled start, so the equality above stops
/// holding. The test finds those rows itself, so it covers whichever rows set the maximum.
#[test]
fn dropping_a_row_that_sets_the_largest_fit_breaks_the_equality() {
    for (scale, constant) in [(1, START_ATLAS_DIM_1X), (2, START_ATLAS_DIM_2X)] {
        let largest = START_SIZE_INPUTS
            .iter()
            .filter(|row| row.scale == scale)
            .filter_map(|row| match row.outcome {
                FitOutcome::Fits(dim) => Some(dim),
                _ => None,
            })
            .max()
            .expect("rows at the scale");
        let deciding: Vec<usize> = START_SIZE_INPUTS
            .iter()
            .enumerate()
            .filter(|(_, row)| row.scale == scale && row.outcome == FitOutcome::Fits(largest))
            .map(|(index, _)| index)
            .collect();
        assert!(!deciding.is_empty(), "{scale}x has a row setting its largest fit");
        for dropped in deciding {
            let mut rows = START_SIZE_INPUTS.to_vec();
            let removed = rows.remove(dropped);
            let ruled = ruled_start(scale, &rows, true);
            assert!(
                ruled.as_ref().map_or(true, |verdict| verdict.dim != constant),
                "{scale}x still rules {constant} without {:?}: {ruled:?}",
                removed.provenance
            );
        }
    }
}

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
/// helper and real-renderer rows come from the push run, the helper from the working-set step and
/// the real renderer from one of the six named test runs, each of which appears. All measure one
/// commit, and no two rows share an identity.
#[test]
fn every_recorded_row_keeps_its_provenance() {
    let mut identities = std::collections::HashSet::new();
    for row in START_SIZE_INPUTS {
        let provenance = &row.provenance;
        assert_eq!((provenance.attempt, provenance.measured_sha), (1, MEASURED_SHA), "{row:?}");
        match row.source {
            InputSource::PerfS9End | InputSource::PerfS12End => {
                assert_eq!((provenance.run_id, provenance.side), (PERF_END_RUN_ID, "base"));
                assert!(matches!(provenance.set, "timed" | "laps" | "counters"), "{row:?}");
                assert!(provenance.origin.ends_with(" end"), "{row:?}");
                assert_eq!(row.scale, 1, "{row:?}");
            }
            InputSource::Helper => {
                assert_eq!((provenance.run_id, provenance.side), (PUSH_RUN_ID, "push"));
                assert_eq!(provenance.set, "working-set-step");
            }
            InputSource::RealRenderer => {
                assert_eq!((provenance.run_id, provenance.side), (PUSH_RUN_ID, "push"));
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

/// A stand-in content identity for a face file whose bytes are `bytes`.
fn content_of(bytes: &str) -> String {
    face_content_id(FaceNamespace::File, bytes.as_bytes())
}

/// The reviewed face's content identity, the one [`exception`] names: a static, as entries are.
static REVIEWED_CONTENT: LazyLock<String> = LazyLock::new(|| content_of("seguiemj.ttf"));

/// A failure of `glyph_id` in a face file named `file` whose bytes hash to `content`, drawn by the
/// body at 14 px in the regular face.
fn failure_in(file: &str, content: String, glyph_id: u32) -> RasterFailure {
    RasterFailure {
        codepoint: '😀',
        role: GlyphRasterVariant::Normal,
        bold: false,
        italic: false,
        face: Some(FailedFace {
            file: file.to_owned(),
            content,
            face_index: 0,
            glyph_id,
            strike_px_milli: 14_000,
        }),
    }
}

/// A failure of `glyph_id` in `file`, whose stand-in bytes are its name, so different names have
/// different contents.
fn failure(file: &str, glyph_id: u32) -> RasterFailure {
    failure_in(file, content_of(file), glyph_id)
}

/// The exception naming exactly `failure("seguiemj.ttf", 42)` on Windows.
fn exception() -> RasterException {
    RasterException {
        platform: "windows",
        role: GlyphRasterVariant::Normal,
        content: REVIEWED_CONTENT.as_str(),
        face_index: 0,
        glyph_id: 42,
        bold: false,
        italic: false,
        strike_px_milli: 14_000,
        codepoint: "😀",
        reason: "reviewed: the face has no outline for this glyph",
    }
}

/// An exception approves only the failure it names on every field: platform, raster role, face
/// content and index, glyph id, style and strike. Changing any one leaves the failure unapproved.
#[test]
fn an_exception_approves_only_its_exact_face_glyph_style_and_strike() {
    let listed = [exception()];
    let matched = normalize_raster_failures("windows", &[failure("seguiemj.ttf", 42)], &listed);
    assert_eq!(matched.matched, vec![(failure("seguiemj.ttf", 42), exception().reason)]);
    assert!(matched.unapproved.is_empty());
    assert_eq!(matched.raw, vec![failure("seguiemj.ttf", 42)]);
    let other_face = |change: fn(&mut FailedFace)| {
        let mut changed = failure("seguiemj.ttf", 42);
        change(changed.face.as_mut().expect("a resolved face"));
        changed
    };
    let mut bold = failure("seguiemj.ttf", 42);
    bold.bold = true;
    let mut italic = failure("seguiemj.ttf", 42);
    italic.italic = true;
    let mut title = failure("seguiemj.ttf", 42);
    title.role = GlyphRasterVariant::TabTitle;
    let unmatched = [
        ("another platform", "macos", failure("seguiemj.ttf", 42)),
        ("another face file", "windows", failure("seguisym.ttf", 42)),
        ("another glyph", "windows", failure("seguiemj.ttf", 43)),
        ("another face", "windows", other_face(|face| face.face_index = 1)),
        ("another strike", "windows", other_face(|face| face.strike_px_milli = 28_000)),
        ("bold", "windows", bold),
        ("italic", "windows", italic),
        ("another role", "windows", title),
    ];
    for (name, platform, candidate) in unmatched {
        let normalized =
            normalize_raster_failures(platform, std::slice::from_ref(&candidate), &listed);
        assert!(normalized.matched.is_empty(), "{name}: not approved");
        assert_eq!(normalized.unapproved, vec![candidate], "{name}: stays unapproved");
    }
}

/// A failure whose key resolved to no face cannot be named by any exception, so it stays
/// unapproved whatever the list holds.
#[test]
fn an_unresolved_failure_stays_unapproved() {
    let unresolved = RasterFailure { face: None, ..failure("seguiemj.ttf", 42) };
    let normalized =
        normalize_raster_failures("windows", std::slice::from_ref(&unresolved), &[exception()]);
    assert_eq!(normalized.unapproved, vec![unresolved]);
}

/// Only approved raster failures leave the incomplete count; unresolved characters and oversize
/// required tiles always count, and any remaining count still selects the maximum.
#[test]
fn only_approved_raster_failures_leave_the_incomplete_count() {
    let normalized = normalize_raster_failures(
        "windows",
        &[failure("seguiemj.ttf", 42), failure("seguiemj.ttf", 7)],
        &[exception()],
    );
    assert_eq!(incomplete_glyphs(2, 1, &normalized), 4, "2 unresolved + 1 oversize + 1 unapproved");
    assert_eq!(incomplete_glyphs(0, 0, &normalized), 1);
    let approved =
        normalize_raster_failures("windows", &[failure("seguiemj.ttf", 42)], &[exception()]);
    assert_eq!(incomplete_glyphs(0, 0, &approved), 0, "an approved failure does not count");
    let mut counted = input("windows 1x S9 helper", FitOutcome::Fits(256), 32);
    counted.incomplete_glyphs = incomplete_glyphs(0, 0, &normalized);
    assert_eq!(start_rule(&[counted]).map(|verdict| verdict.dim), Ok(ATLAS_DIM));
}

/// The reviewed list starts empty and well formed; an entry naming no glyph (glyph id 0, or no face
/// content identity: empty, a file name, a path or a bare digest), with no reason, for an unknown
/// platform or listed twice is refused.
#[test]
fn the_reviewed_exception_list_is_well_formed_and_starts_empty() {
    assert!(RASTER_EXCEPTIONS.is_empty(), "every entry needs a reason reviewed in its PR");
    assert!(exception_problems(RASTER_EXCEPTIONS).is_empty());
    assert!(exception_problems(&[exception()]).is_empty(), "a complete entry is accepted");
    let broken = [
        ("glyph id 0", RasterException { glyph_id: 0, ..exception() }),
        ("no content", RasterException { content: "", ..exception() }),
        ("a file name", RasterException { content: "seguiemj.ttf", ..exception() }),
        (
            "a path",
            RasterException { content: "/System/Library/Fonts/ReviewedFace.ttf", ..exception() },
        ),
        (
            "a bare digest",
            RasterException {
                content: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                ..exception()
            },
        ),
        ("no reason", RasterException { reason: " ", ..exception() }),
        ("unknown platform", RasterException { platform: "linux", ..exception() }),
    ];
    for (name, entry) in broken {
        assert_eq!(exception_problems(&[entry]).len(), 1, "{name}");
    }
    assert_eq!(exception_problems(&[exception(), exception()]).len(), 1, "a duplicate entry");
}

/// An exception names a face by its content, never by its file name: a failure from an unrelated
/// file that shares the reviewed file's name (`/tmp/unrelated-fonts/ReviewedFace.ttf` against the
/// reviewed `/System/Library/Fonts/ReviewedFace.ttf`) stays unapproved, while the reviewed bytes are
/// approved under any name or path, since the name is kept for reading only.
#[test]
fn a_face_file_sharing_the_reviewed_name_is_not_approved() {
    let reviewed = content_of("bytes of /System/Library/Fonts/ReviewedFace.ttf");
    let entry =
        RasterException { content: Box::leak(reviewed.clone().into_boxed_str()), ..exception() };
    let impostor = failure_in(
        "ReviewedFace.ttf",
        content_of("bytes of /tmp/unrelated-fonts/ReviewedFace.ttf"),
        42,
    );
    let normalized =
        normalize_raster_failures("windows", std::slice::from_ref(&impostor), &[entry]);
    assert!(normalized.matched.is_empty(), "a same-named file is not the reviewed face");
    assert_eq!(normalized.unapproved, vec![impostor]);
    let renamed = failure_in("Renamed.ttf", reviewed, 42);
    let normalized = normalize_raster_failures("windows", std::slice::from_ref(&renamed), &[entry]);
    assert_eq!(normalized.matched, vec![(renamed, entry.reason)], "the reviewed bytes match");
}
