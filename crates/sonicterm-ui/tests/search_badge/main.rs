use sonicterm_ui::overlays::{search_bar_label, search_query_caret_prefix};
use sonicterm_ui::search::SearchState;

#[test]
fn search_badge_label_uses_dot_separator_not_horizontal_rule_glyph() {
    let mut search = SearchState::new();
    search.input_char('b', &sonicterm_grid::grid::Grid::new(8, 2));
    search.input_char('r', &sonicterm_grid::grid::Grid::new(8, 2));

    let label = search_bar_label(&search, "");

    assert!(label.contains(" · "));
    assert!(!label.contains('—'));
}

#[test]
fn search_badge_caret_prefix_stops_before_status_suffix() {
    let mut search = SearchState::new();
    search.input_char('b', &sonicterm_grid::grid::Grid::new(8, 2));
    search.input_char('r', &sonicterm_grid::grid::Grid::new(8, 2));

    let label = search_bar_label(&search, "中");
    let prefix = search_query_caret_prefix(&search, "中");
    let (head, suffix) = label.split_once('▏').expect("search label includes caret marker");

    assert_eq!(prefix, head);
    assert!(suffix.starts_with(" · "));
}
