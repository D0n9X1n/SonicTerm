//! Regression guard for the P0 macOS-glyph-blur fix.
//!
//! PR #267 enabled `Format::Subpixel` (LCD) rendering for ALL platforms
//! to address Windows ClearType parity (#261), and inserted
//! `JetBrainsMono Nerd Font` at the FRONT of every fallback chain to
//! satisfy Nerd Font PUA coverage. Both changes regressed macOS:
//!
//! 1. LCD subpixel masks produce visible color fringing on macOS where
//!    grayscale `Format::Alpha` was correct (Mojave+ removed system
//!    subpixel AA).
//! 2. Nerd Font at the front of the chain stole CJK glyph resolution
//!    from PingFang SC / Microsoft YaHei / Noto CJK, since cosmic-text
//!    walks fallbacks in order and Nerd Font has no CJK coverage —
//!    so CJK cells rendered as mangled boxes.
//!
//! These tests pin the platform-gated invariants so a future "let's
//! unify the rasterizer config" or "let's reorder the chain for
//! convenience" PR cannot silently re-regress.

#![allow(missing_docs)]

use sonic_text::swash_rasterizer::{
    monochrome_render_config_for_test, platform_fallback_chain_for_test,
};
use swash::zeno::Format;

#[test]
#[cfg(not(target_os = "windows"))]
fn non_windows_uses_alpha_format() {
    let (_sources, format, _hint) = monochrome_render_config_for_test();
    assert_eq!(
        format,
        Format::Alpha,
        "macOS and Linux must use grayscale alpha masks. LCD subpixel \
         (Format::Subpixel) produces color fringing on macOS where the OS \
         no longer performs system-level subpixel AA. See P0 fix for PR #267."
    );
}

#[test]
#[cfg(target_os = "windows")]
fn windows_uses_subpixel_format() {
    let (_sources, format, _hint) = monochrome_render_config_for_test();
    assert_eq!(
        format,
        Format::Subpixel,
        "Windows must use LCD subpixel masks for ClearType parity (#261)."
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
        platform_fallback_chain_for_test().contains(&"JetBrainsMono Nerd Font"),
        "JetBrainsMono Nerd Font must still appear in the chain (tail) for PUA codepoints"
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
