use super::*;

#[test]
fn plain_word_is_single_quoted() {
    assert_eq!(shell_quote_posix("foo"), "'foo'");
}

#[test]
fn empty_becomes_empty_quotes() {
    assert_eq!(shell_quote_posix(""), "''");
}

#[test]
fn embedded_single_quote_is_escaped() {
    // don't -> 'don'\''t'
    assert_eq!(shell_quote_posix("don't"), "'don'\\''t'");
}

#[test]
fn spaces_and_specials_are_contained_by_quotes() {
    assert_eq!(shell_quote_posix("a b$c;d"), "'a b$c;d'");
    assert_eq!(shell_quote_posix("/path/with space/x"), "'/path/with space/x'");
}

#[test]
fn only_single_quote() {
    assert_eq!(shell_quote_posix("'"), "''\\'''");
}
