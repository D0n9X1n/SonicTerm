//! Regression guard for P0 text rasterizer fixes.
//!
//! PR #267 enabled `Format::Subpixel` (LCD) rendering to address Windows
//! ClearType parity (#261), and inserted `JetBrainsMono Nerd Font` at the
//! FRONT of every fallback chain to satisfy Nerd Font PUA coverage. The
//! fallback ordering regressed CJK glyph resolution on macOS, and the Windows
//! LCD integration later regressed terminal cell readability into horizontal
//! ink-stroke artifacts (#316).
//!
//! These tests pin the safe grayscale rasterizer format and fallback-chain
//! invariants so a future "let's unify the rasterizer config" or "let's
//! reorder the chain for convenience" PR cannot silently re-regress.

#![allow(missing_docs)]

use sonic_text::swash_rasterizer::{
    monochrome_render_config_for_test, platform_fallback_chain_for_test,
};
use swash::zeno::Format;

#[test]
fn monochrome_uses_alpha_format_on_all_platforms() {
    let (_sources, format, _hint) = monochrome_render_config_for_test();
    assert_eq!(
        format,
        Format::Alpha,
        "all platforms must use grayscale alpha masks until the Windows LCD integration is fixed (#316)"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn nerd_font_not_first_in_macos_fallback_chain() {
    let first = platform_fallback_chain_for_test().first().copied().unwrap_or("");
    assert_ne!(
        first, "JetBrainsMono Nerd Font",
        "JetBrainsMono Nerd Font at the FRONT of the macOS chain steals \
         CJK glyph resolution from PingFang SC (Nerd Font has no CJK \
         coverage). Keep it at the TAIL — it still resolves PUA \
         codepoints (#261) because no earlier face covers those."
    );
    assert_eq!(
        first, "PingFang SC",
        "macOS fallback chain must begin with PingFang SC for correct CJK"
    );
    assert!(
        !platform_fallback_chain_for_test().contains(&"JetBrainsMono Nerd Font"),
        "the bundled JetBrainsMono TTFs were dropped in #419; the chain must not \
         reference them anymore (Nerd Font PUA glyphs now rely on a system install)"
    );
}

#[test]
#[cfg(target_os = "windows")]
fn nerd_font_not_first_in_windows_fallback_chain() {
    let first = platform_fallback_chain_for_test().first().copied().unwrap_or("");
    assert_ne!(first, "JetBrainsMono Nerd Font");
    assert_eq!(first, "Microsoft YaHei");
}

#[test]
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn nerd_font_not_first_in_linux_fallback_chain() {
    let first = platform_fallback_chain_for_test().first().copied().unwrap_or("");
    assert_ne!(first, "JetBrainsMono Nerd Font");
}
