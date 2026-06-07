use sonicterm_app_core::{AppState, WindowKey};

#[test]
fn exports_builder_and_contract_types() {
    let state = AppState::builder().with_grid(80, 24).build();
    assert_eq!(state.cols, 80);
    assert_eq!(state.rows, 24);
    assert_ne!(WindowKey::new(1), WindowKey::new(2));
}
