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
//!   loop so we can keep `sonicterm-cfg`'s dep surface minimal and avoid
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
            // When: `bytes[i]` is not ASCII alphabetic, it cannot start an allow-listed scheme.
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
                // When: `text.get(...)` yields `slice`, the candidate ended on UTF-8 boundaries and can be compared safely.
                if slice.eq_ignore_ascii_case(s) {
                    // When: `slice` equals `s` ignoring case, retain this scheme length and stop probing alternatives.
                    matched_scheme = Some(sb.len());
                    break;
                }
            }
        }
        let Some(scheme_len) = matched_scheme else {
            // When: `matched_scheme` is absent, advance one byte and continue searching for a scheme start.
            i += 1;
            continue;
        };
        // A scheme match in the middle of a longer identifier
        // (e.g. `xhttp://`) should not count — the previous char,
        // if any, must not itself be a URL body char.
        if i > 0 && is_url_body_char(bytes[i - 1] as char) {
            // When: `i` follows a URL-body character, this scheme text is embedded in a larger token.
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
                // When: `matches!(last, ...)` is false, preserve `last` as part of the candidate URL.
                break;
            }
        }
        // Require at least one body byte after the scheme.
        if end <= i + scheme_len {
            // When: `end` contains no body beyond `scheme_len`, skip the empty URL candidate.
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
            // When: character index `i` reaches `col`, retain its UTF-8 byte offset for URL lookup.
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
#[path = "url_scan_tests.rs"]
mod url_scan_tests;
