use super::*;
use crate::glyph_atlas::{START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};

/// A rule input labelled `label` with `outcome` and a square largest tile of `tile` pixels.
fn input(label: &str, outcome: FitOutcome, tile: u32) -> RuleInput {
    RuleInput { label: label.to_owned(), outcome, max_tile: [tile, tile] }
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

/// The start constants are exactly what the rule selects over the measured table at each scale;
/// an empty table selects the maximum, which saves nothing until CI rows are recorded.
#[test]
fn start_constants_equal_the_rule_over_the_measured_table() {
    assert_eq!(table_start_dim(1).dim, START_ATLAS_DIM_1X, "{}", table_start_dim(1).verdict);
    assert_eq!(table_start_dim(2).dim, START_ATLAS_DIM_2X, "{}", table_start_dim(2).verdict);
    if START_SIZE_INPUTS.is_empty() {
        // When: START_SIZE_INPUTS is empty the verdict must say so rather than claim a measurement.
        assert!(table_start_dim(1).verdict.contains("no savings"));
    }
}
