use super::*;
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
            run_url: "https://ci.invalid/run",
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
/// table and the shipped oracle guard; while the guard is false that admits only the maximum.
#[test]
fn start_constants_pass_the_table_validation() {
    for (scale, constant) in [(1, START_ATLAS_DIM_1X), (2, START_ATLAS_DIM_2X)] {
        let verdict = validate_table_start(scale, constant)
            .unwrap_or_else(|error| panic!("{scale}x start {constant} rejected: {error}"));
        assert_eq!(verdict.dim, constant);
    }
}
