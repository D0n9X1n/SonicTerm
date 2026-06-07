use sonicterm_ui::search::SearchState;

#[test]
fn exports_search_state() {
    let search = SearchState::new();
    assert!(search.query.is_empty());
    assert!(search.matches.is_empty());
}
