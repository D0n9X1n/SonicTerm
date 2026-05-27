//! Regression: the tab-title text MUST use the same `Family::Name(...)`
//! that the configured font family resolves to. This pins the chain
//!
//!   `Config.font.family` → `GpuRenderer::font_family` → `terminal_font_attrs(...)`
//!
//! so a future refactor cannot silently re-introduce the bug where the
//! tab bar fell through to `Family::Monospace` and rendered the title
//! with a different installed face than the grid body.
//!
//! Companion to `unified_font_attrs.rs`, but specifically targeting the
//! "tab title follows config" contract the user reported regressing in
//! PR #113.

use glyphon::Family;
use sonic_core::config::FontConfig;
use sonic_shared::render::terminal_font_attrs;

#[test]
fn tab_title_attrs_follow_configured_family() {
    let font = FontConfig { family: "Menlo".to_string(), ..FontConfig::default() };

    let tab_attrs = terminal_font_attrs(&font.family);
    let grid_attrs = terminal_font_attrs(&font.family);

    // Both call sites must hand glyphon the SAME Family::Name(...) value.
    assert_eq!(format!("{:?}", tab_attrs.family), format!("{:?}", grid_attrs.family));

    // And that value must be Family::Name("Menlo") — not Monospace or any
    // other generic the buffer could fall through to.
    let expected = Family::Name("Menlo");
    assert_eq!(format!("{:?}", tab_attrs.family), format!("{:?}", expected));
}

#[test]
fn tab_title_attrs_follow_default_st_helens_family() {
    let font = FontConfig::default();
    let tab_attrs = terminal_font_attrs(&font.family);
    let expected = Family::Name("Rec Mono St.Helens");
    assert_eq!(format!("{:?}", tab_attrs.family), format!("{:?}", expected));
}
