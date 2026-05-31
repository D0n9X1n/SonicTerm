//! Regression: scrollbar rendering must be opt-out via config (PR-B of #386).
//!
//! Original purpose (pre-#386): assert that Sonic never drew a scrollbar
//! at all, matching WezTerm's `enable_scroll_bar = false` default by
//! source-scanning for forbidden tokens.
//!
//! Updated for #386: PR-A added `appearance.scrollbar = Auto` as the
//! default and PR-B wires the renderer. The historic source-scan no
//! longer applies — the rendering symbols now legitimately exist. What
//! still must hold: when the user picks `ScrollbarMode::Never`, the
//! emit path produces zero quads. That's the regression-guard PR-D
//! polish must not break either.
//!
//! Deeper coverage (Auto-shows-when-scrollable, geometry placement,
//! no-emit when total <= viewport) lives in `render_scrollbar_emit.rs`.

use sonic_cfg::config::ScrollbarMode;

#[test]
fn never_mode_emits_nothing() {
    let rect = sonic_ui::scrollbar::Rect::new(0.0, 0.0, 800.0, 600.0);
    let geom = sonic_ui::scrollbar::compute(24, 10_000, 0, rect, ScrollbarMode::Never, 8.0);
    assert!(geom.is_none(), "ScrollbarMode::Never must produce no geometry");
}

#[test]
fn default_mode_is_auto() {
    // PR-A chose Auto as the default. Documenting the choice here so a
    // future bump to e.g. `Never` is intentional and not accidental.
    assert_eq!(ScrollbarMode::default(), ScrollbarMode::Auto);
}
