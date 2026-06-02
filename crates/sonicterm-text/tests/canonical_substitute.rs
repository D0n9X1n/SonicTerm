//! #461 PR-B2c: canonical-substitute regression. Locks the
//! `SwashRasterizer::resolve_slot` + `rasterize` substitution mapping for
//! codepoints that visually equal a bundled-font codepoint but aren't
//! themselves in the bundled cmap. Without this fix, Claude Code's
//! bypass-mode arrows (U+23F5) render as `[]` tofu instead of `▶`.

use sonicterm_text::swash_rasterizer::canonical_substitute;

#[test]
fn u23f4_left_substitutes_to_u25c0() {
    assert_eq!(canonical_substitute('\u{23F4}'), '\u{25C0}', "⏴ → ◀");
}

#[test]
fn u23f5_right_substitutes_to_u25b6() {
    assert_eq!(canonical_substitute('\u{23F5}'), '\u{25B6}', "⏵ → ▶");
}

#[test]
fn u23f6_up_substitutes_to_u25b2() {
    assert_eq!(canonical_substitute('\u{23F6}'), '\u{25B2}', "⏶ → ▲");
}

#[test]
fn u23f7_down_substitutes_to_u25bc() {
    assert_eq!(canonical_substitute('\u{23F7}'), '\u{25BC}', "⏷ → ▼");
}

#[test]
fn non_substituted_codepoints_pass_through_unchanged() {
    // Bundled font HAS these — must NOT substitute.
    assert_eq!(canonical_substitute('\u{25B6}'), '\u{25B6}'); // already ▶
    assert_eq!(canonical_substitute('\u{E0B0}'), '\u{E0B0}'); // Powerline
    assert_eq!(canonical_substitute('A'), 'A');
    assert_eq!(canonical_substitute('中'), '中');
    assert_eq!(canonical_substitute('\u{F0001}'), '\u{F0001}'); // NF MDI
}

#[test]
fn substitute_range_is_exactly_4_codepoints() {
    // Boundary check: U+23F3 (just outside) and U+23F8 (just outside) must
    // NOT be substituted. The range is exactly U+23F4..=U+23F7.
    assert_eq!(canonical_substitute('\u{23F3}'), '\u{23F3}'); // ⏳ hourglass
    assert_eq!(canonical_substitute('\u{23F8}'), '\u{23F8}'); // ⏸ pause
}
