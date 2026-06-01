//! Smoke test for `sonicterm_ui::i18n::I18n`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/i18n.rs`.
//! Named `src_i18n.rs` to distinguish from the more comprehensive
//! `sonicterm-shared/tests/i18n.rs` in a sibling crate.

#[test]
fn module_loads() {
    let _ = sonicterm_ui::i18n::I18n::new(Some("en"));
}
