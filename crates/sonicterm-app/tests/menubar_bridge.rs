//! Tests for `menubar_bridge::push_action` + drain.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/menubar_bridge.rs`.
//! Uses `__test_drain` (the doc-hidden test bridge) so this file does not
//! need access to the crate-private `drain`.

use sonicterm_app::menubar_bridge::{__test_drain, push_action};
use sonicterm_core::keymap::Action;

#[test]
fn push_then_drain_preserves_order() {
    let _ = __test_drain();
    push_action(Action::NewTab);
    push_action(Action::CloseTab);
    push_action(Action::EditConfigFile);
    let drained = __test_drain();
    assert_eq!(drained.len(), 3);
    assert!(matches!(drained[0], Action::NewTab));
    assert!(matches!(drained[1], Action::CloseTab));
    assert!(matches!(drained[2], Action::EditConfigFile));
    assert!(__test_drain().is_empty());
}
