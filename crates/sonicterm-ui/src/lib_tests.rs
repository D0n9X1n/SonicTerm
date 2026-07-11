//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::search::SearchState;

#[test]
fn exports_search_state() {
    let search = SearchState::new();
    assert!(search.query.is_empty());
    assert!(search.matches.is_empty());
}
