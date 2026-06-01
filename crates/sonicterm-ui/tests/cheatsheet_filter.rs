use sonicterm_ui::cheatsheet::filter_indices;

#[test]
fn filter_empty_query_returns_all() {
    let bindings = vec![("Ctrl+A".to_string(), "select_all".to_string())];
    let idxs = filter_indices(&bindings, "");
    assert_eq!(idxs, vec![0]);
}

#[test]
fn filter_matches_action_name_case_insensitive() {
    let bindings = vec![
        ("Ctrl+P".to_string(), "command_palette".to_string()),
        ("Ctrl+A".to_string(), "select_all".to_string()),
    ];
    assert_eq!(filter_indices(&bindings, "PALETTE"), vec![0]);
    assert_eq!(filter_indices(&bindings, "select"), vec![1]);
}

#[test]
fn filter_matches_key_string() {
    let bindings = vec![("Ctrl+P".to_string(), "command_palette".to_string())];
    assert_eq!(filter_indices(&bindings, "ctrl"), vec![0]);
}
