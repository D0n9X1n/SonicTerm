//! Plain-text URL detection for terminal grid rows.
//!
//! Scans a row of terminal text and returns the byte ranges that look
//! like URLs we are willing to open via [`crate::url_open::open`]. The
//! scanner is deliberately narrow:
//!
//! - Only `http://`, `https://`, `mailto:` and `file://` schemes are
//!   recognised — matching the allow-list enforced by
//!   [`crate::url_open::validate`].
//! - URL characters are limited to RFC 3986 unreserved / sub-delims /
//!   reserved minus a handful of shell-meta and quote chars (`<`, `>`,
//!   `"`, `'`, backtick, whitespace, control). This intentionally
//!   under-matches at the edges (e.g. trailing punctuation like `.`
//!   or `)` is trimmed) but the result is always a string that will
//!   pass `validate()`.
//! - No regex / `once_cell` dependency: the scanner is a small hand
//!   loop so we can keep `sonic-cfg`'s dep surface minimal and avoid
//!   per-frame regex compilation cost.
//!
//! The contract is: every returned `(start, end)` slice satisfies
//! `validate(slice).is_ok()`. Tests below assert this.

use crate::url_open::validate;

/// One detected URL in a row of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlMatch {
    /// Byte offset (inclusive) of the URL in the input.
    pub start: usize,
    /// Byte offset (exclusive) of the URL in the input.
    pub end: usize,
    /// The matched URL string.
    pub url: String,
}

const SCHEMES: &[&str] = &["https://", "http://", "mailto:", "file://"];

/// Return every URL substring of `text` whose scheme is on our
/// allow-list and which passes [`validate`].
pub fn find_urls(text: &str) -> Vec<UrlMatch> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find the next plausible scheme start. We anchor on ASCII
        // letters because every supported scheme begins with one.
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let mut matched_scheme = None;
        for s in SCHEMES {
            let sb = s.as_bytes();
            // Use `text.get(..)` rather than `&text[..]` so a byte
            // range that lands inside a multi-byte UTF-8 char (e.g.
            // `❯` from an oh-my-zsh prompt) returns `None` instead of
            // panicking. Schemes are pure ASCII so a non-boundary end
            // index can never be a real match anyway.
            if let Some(slice) = text.get(i..i + sb.len()) {
                if slice.eq_ignore_ascii_case(s) {
                    matched_scheme = Some(sb.len());
                    break;
                }
            }
        }
        let Some(scheme_len) = matched_scheme else {
            i += 1;
            continue;
        };
        // A scheme match in the middle of a longer identifier
        // (e.g. `xhttp://`) should not count — the previous char,
        // if any, must not itself be a URL body char.
        if i > 0 && is_url_body_char(bytes[i - 1] as char) {
            i += 1;
            continue;
        }
        let mut end = i + scheme_len;
        while end < bytes.len() && is_url_body_char(bytes[end] as char) {
            end += 1;
        }
        // Trim trailing punctuation that's commonly adjacent to a
        // URL in prose (`)`, `.`, `,`, `;`, `:`, `!`, `?`).
        while end > i + scheme_len {
            let last = bytes[end - 1] as char;
            if matches!(last, ')' | ']' | '.' | ',' | ';' | ':' | '!' | '?') {
                end -= 1;
            } else {
                break;
            }
        }
        // Require at least one body byte after the scheme.
        if end <= i + scheme_len {
            i += scheme_len;
            continue;
        }
        let url = &text[i..end];
        if validate(url).is_ok() {
            out.push(UrlMatch { start: i, end, url: url.to_string() });
        }
        i = end.max(i + 1);
    }
    out
}

/// Return the URL covering byte offset `byte_col`, if any.
pub fn url_at_byte(text: &str, byte_col: usize) -> Option<UrlMatch> {
    find_urls(text).into_iter().find(|m| byte_col >= m.start && byte_col < m.end)
}

/// Return the URL covering character column `col` (0-based, counting
/// `char`s not bytes — matches the terminal grid model).
pub fn url_at_char_col(text: &str, col: usize) -> Option<UrlMatch> {
    let mut byte = None;
    for (i, (b, _)) in text.char_indices().enumerate() {
        if i == col {
            byte = Some(b);
            break;
        }
    }
    let byte = byte?;
    url_at_byte(text, byte)
}

#[inline]
fn is_url_body_char(c: char) -> bool {
    // RFC 3986 unreserved + sub-delims + a couple of reserved we
    // commonly see embedded in URLs in the wild, MINUS shell-meta
    // and quote chars that `validate()` rejects.
    matches!(c,
        'a'..='z' | 'A'..='Z' | '0'..='9' |
        '-' | '_' | '.' | '~' |
        '!' | '$' | '*' | '+' | ',' | ';' | '=' |
        ':' | '/' | '?' | '#' | '[' | ']' | '@' |
        '%' | '(' | ')'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_plain_https() {
        let m = find_urls("see https://example.com for details");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "https://example.com");
        assert_eq!(
            &"see https://example.com for details"[m[0].start..m[0].end],
            "https://example.com"
        );
    }

    #[test]
    fn detects_http_mailto_file() {
        let s = "open http://x.test or mailto:a@b.test or file:///etc/hosts";
        let m = find_urls(s);
        assert_eq!(m.len(), 3, "got: {m:?}");
        assert_eq!(m[0].url, "http://x.test");
        assert_eq!(m[1].url, "mailto:a@b.test");
        assert_eq!(m[2].url, "file:///etc/hosts");
    }

    #[test]
    fn trims_trailing_punctuation() {
        let m = find_urls("visit https://example.com.");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "https://example.com");
        let m = find_urls("(see https://example.com).");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "https://example.com");
        let m = find_urls("https://example.com/path?q=1!");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "https://example.com/path?q=1");
    }

    #[test]
    fn rejects_shell_meta() {
        // These three carry shell-meta in the URL body itself; the
        // scanner must not return them.
        for bad in [
            "https://evil.test&calc.exe",
            "https://evil.test|whoami",
            "https://evil.test`id`",
            "https://evil.test\"x",
            "https://evil.test'x",
            "https://evil.test<x",
            "https://evil.test>x",
        ] {
            for m in find_urls(bad) {
                // Any match returned must individually validate. The
                // shell-meta byte must NOT be inside the matched span.
                assert!(validate(&m.url).is_ok(), "leaked unsafe url: {bad:?} -> {m:?}");
                assert!(
                    !m.url.chars().any(|c| matches!(c, '&' | '|' | '`' | '"' | '\'' | '<' | '>')),
                    "url body contains shell meta: {m:?}"
                );
            }
        }
    }

    #[test]
    fn rejects_unknown_schemes() {
        let m = find_urls("ssh://host gopher://x ftp://y javascript:alert(1)");
        assert!(m.is_empty(), "should reject non-allow-listed schemes, got {m:?}");
    }

    #[test]
    fn anchors_to_word_boundary() {
        // `xhttp://...` is not a URL — the `x` is part of a word.
        let m = find_urls("xhttp://example.com");
        assert!(m.is_empty(), "should not match mid-word, got {m:?}");
    }

    #[test]
    fn finds_multiple_in_one_line() {
        let s = "a https://one.test b https://two.test c";
        let m = find_urls(s);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].url, "https://one.test");
        assert_eq!(m[1].url, "https://two.test");
    }

    #[test]
    fn url_at_char_col_basic() {
        let s = "hello https://example.com world";
        // 'x' in "example" — should be inside the URL
        let col = s.char_indices().position(|(_, c)| c == 'x').unwrap();
        assert!(url_at_char_col(s, col).is_some());
        // Far past the URL
        assert!(url_at_char_col(s, s.chars().count() - 1).is_none());
    }

    #[test]
    fn empty_and_no_url() {
        assert!(find_urls("").is_empty());
        assert!(find_urls("nothing to see here").is_empty());
        assert!(find_urls("https://").is_empty()); // no body after scheme
    }

    #[test]
    fn case_insensitive_scheme() {
        let m = find_urls("HTTPS://Example.COM/Path");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "HTTPS://Example.COM/Path");
    }
}
