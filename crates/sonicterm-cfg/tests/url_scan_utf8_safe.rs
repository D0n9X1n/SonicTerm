//! Regression tests for the Cmd+Click crash where `url_scan::find_urls`
//! panicked on a `&str` containing multi-byte UTF-8 chars (e.g. the `❯`
//! glyph from oh-my-zsh prompts).
//!
//! Pre-fix the scanner sliced `text[i..i + scheme_len]` with a raw byte
//! index; when `i + scheme_len` landed inside a multi-byte char, Rust
//! panicked with `byte index N is not a char boundary`.
//!
//! These tests assert that the public API never panics on arbitrary
//! UTF-8 input and still returns the correct URL spans when one is
//! present alongside multi-byte content.

use sonicterm_cfg::url_scan::{find_urls, url_at_byte, url_at_char_col};

#[test]
fn user_reported_prompt_with_box_arrow_does_not_panic() {
    // Exact shape of the line that crashed sonicterm-mac when the user
    // Cmd+clicked it. The `❯` is 3 bytes, so the original byte-index
    // arithmetic could land mid-codepoint.
    let text = "sonic-work ❯ cd ..  ";
    let result = find_urls(text);
    assert!(result.is_empty(), "no URLs in this string, got {result:?}");
}

#[test]
fn cjk_emoji_box_drawing_chars_do_not_panic() {
    // One char from every multi-byte class we promise to support.
    let inputs = [
        "中文 in a prompt",
        "🎉 party 🎊 emoji",
        "┌─ box ─ drawing ─┐",
        "Привет from cyrillic",
        "한글 hangul row",
        "❯ ➜ ▶ powerline-ish chars",
        "mix: 中 🎉 ❯ ─ end",
    ];
    for s in inputs {
        // Must not panic.
        let _ = find_urls(s);
    }
}

#[test]
fn url_among_multibyte_chars_is_still_found() {
    let s = "❯ visit https://example.com 中文";
    let m = find_urls(s);
    assert_eq!(m.len(), 1, "expected exactly one URL, got {m:?}");
    assert_eq!(m[0].url, "https://example.com");
    // Span must round-trip cleanly through the original string.
    assert_eq!(&s[m[0].start..m[0].end], "https://example.com");
}

#[test]
fn url_at_byte_does_not_panic_on_multibyte() {
    let s = "❯ cd ..";
    // Probe every byte offset including ones inside `❯`.
    for i in 0..=s.len() {
        let _ = url_at_byte(s, i);
    }
}

#[test]
fn url_at_char_col_does_not_panic_on_multibyte() {
    let s = "❯ 中文 https://example.com 🎉";
    for col in 0..=s.chars().count() + 2 {
        let _ = url_at_char_col(s, col);
    }
}

#[test]
fn scheme_prefix_at_string_end_with_multibyte_does_not_panic() {
    // Force a scheme-prefix check where `i + scheme_len` runs past
    // the end *and* the tail bytes are multi-byte continuation bytes.
    // Pre-fix `text[i..i+sb.len()]` could panic; post-fix `get()`
    // returns None and we move on.
    let cases = [
        "htt❯",     // partial scheme followed by multi-byte
        "https:/❯", // almost a scheme, ends mid-multi-byte zone
        "mailto❯",  // scheme-like prefix abutting multi-byte
        "file:/❯/x",
        "❯https://x.test", // multi-byte immediately before a real URL
    ];
    for s in cases {
        let _ = find_urls(s);
    }
    // The last one should still find the URL even though it's preceded
    // by a multi-byte glyph (the previous byte is a UTF-8 continuation
    // byte, not a URL body char, so the word-boundary check passes).
    let m = find_urls("❯https://x.test");
    assert_eq!(m.len(), 1, "got {m:?}");
    assert_eq!(m[0].url, "https://x.test");
}
