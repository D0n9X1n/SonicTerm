//! Word boundaries preserve connector tokens and clamp empty or boundary cells.

use super::*;

#[test]
fn word_bounds_inside_connector_word() {
    // "foo bar.baz qux"
    //  0123456789012345
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // col 8 is the 'b' of "baz" inside "bar.baz"; the '.' connector
    // keeps "bar.baz" a single word spanning cols 4..=10.
    assert_eq!(word_bounds(&chars, 8), (4, 10));
    // The connector '.' itself (col 7) is a word char → same span.
    assert_eq!(word_bounds(&chars, 7), (4, 10));
}

#[test]
fn word_bounds_on_space_is_single_cell() {
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // col 3 is the space between "foo" and "bar.baz".
    assert_eq!(word_bounds(&chars, 3), (3, 3));
    // col 11 is the space before "qux".
    assert_eq!(word_bounds(&chars, 11), (11, 11));
}

#[test]
fn word_bounds_stops_at_word_edges() {
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // Start of "foo".
    assert_eq!(word_bounds(&chars, 0), (0, 2));
    // End of "foo".
    assert_eq!(word_bounds(&chars, 2), (0, 2));
    // Start of "qux".
    assert_eq!(word_bounds(&chars, 12), (12, 14));
    // End of "qux".
    assert_eq!(word_bounds(&chars, 14), (12, 14));
}

#[test]
fn word_bounds_empty_slice() {
    assert_eq!(word_bounds(&[], 0), (0, 0));
    assert_eq!(word_bounds(&[], 5), (0, 0));
}

// ---- word_at / line_at (Grid constructors) ----
