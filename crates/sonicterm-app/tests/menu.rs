//! Tests for the menu blueprint (sonicterm-app::menu).
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/menu.rs`. Named
//! `menu.rs` (not `menu_blueprint.rs`) to avoid clashing with the existing
//! cross-platform parity test at `tests/menu_blueprint.rs`.

use sonicterm_app::menu::{blueprint, Sender};

#[test]
fn blueprint_top_level_order_is_canonical() {
    let bp = blueprint();
    let titles: Vec<&str> = bp.iter().map(|s| s.title).collect();
    assert_eq!(titles, vec!["SonicTerm", "Shell", "Edit", "View", "Help"]);
}

#[test]
fn sender_is_clone_send_sync() {
    fn assert_traits<T: Clone + Send + Sync>() {}
    assert_traits::<Sender>();
}
