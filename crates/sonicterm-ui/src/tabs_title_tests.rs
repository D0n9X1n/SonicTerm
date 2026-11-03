//! Pins the tab-title text helpers on their own: the `#N` prefix split, body replacement, and
//! fitting a title into a width with an ellipsis.

use super::*;

/// `strip_index_prefix` returns the text after `#N` and refuses a title with no digits after
/// `#`; `title_body` drops the prefix and its separator.
#[test]
fn the_index_prefix_is_split_only_when_digits_follow_the_hash() {
    assert_eq!(strip_index_prefix("#12 ~/work"), Some(" ~/work"));
    assert_eq!(strip_index_prefix("#x ~/work"), None);
    assert_eq!(strip_index_prefix("Welcome"), None);
    assert_eq!(title_body("#3 ~/work"), "~/work");
    assert_eq!(title_body("Welcome"), "Welcome");
}

/// A body replacement keeps the `#N` index, and a title without one becomes the body.
#[test]
fn a_replaced_body_keeps_the_index() {
    assert_eq!(title_with_replaced_body("#2 ~/work", "renamed"), "#2 renamed");
    assert_eq!(title_with_replaced_body("Welcome", "renamed"), "renamed");
}

/// A title within the width plus the rounding tolerance is kept whole; a longer one is cut at a
/// grapheme with the ellipsis width reserved, and nothing fits when even the ellipsis does not.
#[test]
fn a_title_is_kept_whole_or_cut_with_an_ellipsis() {
    let advances = |text: &str| -> Vec<(usize, f32)> {
        text.char_indices().map(|(offset, _)| (offset, 10.0)).collect()
    };
    let whole = fit_title_to_width("abc", &advances("abc"), 5.0, 30.0 - TITLE_FIT_TOLERANCE_PX);
    assert_eq!((whole.text.as_str(), whole.cut), ("abc", false));
    let cut = fit_title_to_width("abcdef", &advances("abcdef"), 5.0, 30.0);
    assert!(cut.cut && cut.text.ends_with('…') && cut.width_px <= 30.0 + TITLE_FIT_TOLERANCE_PX);
    let none = fit_title_to_width("abcdef", &advances("abcdef"), 50.0, 30.0);
    assert_eq!((none.text.as_str(), none.cut), ("", true));
}
