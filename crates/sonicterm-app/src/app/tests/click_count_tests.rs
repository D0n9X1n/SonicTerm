use super::next_click_count;

#[test]
fn single_double_triple_then_wraps() {
    // Same cell, within interval: 1 → 2 → 3 → back to 1.
    let c1 = next_click_count(0, true, true); // fresh streak
    assert_eq!(c1, 1);
    let c2 = next_click_count(c1, true, true);
    assert_eq!(c2, 2);
    let c3 = next_click_count(c2, true, true);
    assert_eq!(c3, 3);
    let c4 = next_click_count(c3, true, true);
    assert_eq!(c4, 1); // wraps after triple
}

#[test]
fn different_cell_resets_to_one() {
    // A double-click is in progress (prev = 2) but the new press is
    // on a different cell → streak restarts at 1.
    assert_eq!(next_click_count(2, false, true), 1);
    assert_eq!(next_click_count(1, false, true), 1);
}

#[test]
fn timeout_resets_to_one() {
    // Same cell but past the multi-click interval → restart at 1.
    assert_eq!(next_click_count(2, true, false), 1);
    assert_eq!(next_click_count(1, true, false), 1);
}

