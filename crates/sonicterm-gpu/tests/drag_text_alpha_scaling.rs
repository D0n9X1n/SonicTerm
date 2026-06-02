//! Phase D (Epic #289) — Haiku follow-up on PR #298.
//!
//! Regression for the rendering bug where the source-tab title text
//! and ghost-chip title text painted at full opacity on top of dimmed
//! body quads. The fix introduced `scale_glyphon_alpha`, which the
//! renderer uses to multiply the text color alpha by the matching
//! `chip.ghost_alpha` (0.5) and `chip.source_alpha` (0.3).
//!
//! These tests pin the helper's arithmetic at the spec multipliers so
//! a future refactor can't silently regress the alpha values without
//! tripping the test floor.

use glyphon::Color as GColor;
use sonicterm_gpu::core::scale_glyphon_alpha;

#[test]
fn scale_glyphon_alpha_half_matches_ghost_spec() {
    // Phase D D1 spec: ghost text alpha = 50 % of the base color.
    // Base is fully opaque (a=255), so the result must be ~127.
    let base = GColor::rgba(0xAA, 0xBB, 0xCC, 0xFF);
    let dimmed = scale_glyphon_alpha(base, 0.5);
    assert_eq!(dimmed.r(), 0xAA, "rgb must be preserved");
    assert_eq!(dimmed.g(), 0xBB);
    assert_eq!(dimmed.b(), 0xCC);
    assert_eq!(dimmed.a(), 128, "0.5 * 255 rounds to 128 (the 50 %-alpha ghost title spec)");
}

#[test]
fn scale_glyphon_alpha_three_tenths_matches_source_spec() {
    // Phase D D3 spec: source-tab text alpha = 30 % of the base color.
    let base = GColor::rgba(0x10, 0x20, 0x30, 0xFF);
    let dimmed = scale_glyphon_alpha(base, 0.3);
    assert_eq!(dimmed.r(), 0x10);
    assert_eq!(dimmed.g(), 0x20);
    assert_eq!(dimmed.b(), 0x30);
    // 0.3 * 255 = 76.5 -> rounds to 77.
    assert_eq!(dimmed.a(), 77, "0.3 * 255 rounds to 77 (the 30 %-alpha source-tab title spec)");
}

#[test]
fn scale_glyphon_alpha_clamps_factor() {
    let base = GColor::rgba(1, 2, 3, 200);
    // factor < 0 clamps to 0 -> alpha zero.
    assert_eq!(scale_glyphon_alpha(base, -0.5).a(), 0);
    // factor > 1 clamps to 1 -> alpha unchanged.
    assert_eq!(scale_glyphon_alpha(base, 5.0).a(), 200);
    // factor == 1 is identity.
    let same = scale_glyphon_alpha(base, 1.0);
    assert_eq!(same.as_rgba_tuple(), (1, 2, 3, 200));
}

#[test]
fn scale_glyphon_alpha_respects_existing_alpha() {
    // If the base color is already semi-transparent, the scale
    // composes — e.g. a base alpha of 200 * 0.5 ≈ 100, NOT 128.
    // This pins that the helper is a multiplier, not an override.
    let base = GColor::rgba(0xFF, 0xFF, 0xFF, 200);
    let dimmed = scale_glyphon_alpha(base, 0.5);
    assert_eq!(dimmed.a(), 100);
}
