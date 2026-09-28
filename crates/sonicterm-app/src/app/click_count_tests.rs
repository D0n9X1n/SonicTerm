use super::next_click_count;

#[test]
fn single_double_triple_then_wraps() {
    // Same cell, within interval: 1 → 2 → 3 → back to 1.
    let first_press_count = next_click_count(0, true, true); // fresh streak
    assert_eq!(first_press_count, 1);
    let second_press_count = next_click_count(first_press_count, true, true);
    assert_eq!(second_press_count, 2);
    let third_press_count = next_click_count(second_press_count, true, true);
    assert_eq!(third_press_count, 3);
    let fourth_press_count = next_click_count(third_press_count, true, true);
    assert_eq!(fourth_press_count, 1); // wraps after triple
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
