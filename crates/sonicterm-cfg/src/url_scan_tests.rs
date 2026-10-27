//! Behavior tests for plain-text URL scanning.
//!
//! The load-bearing security property is the closing assertion in
//! `every_match_passes_open_policy`: whatever the scanner hands back passes
//! the opener's lexical URI check, and every match that is not a `file:` URI
//! is something `url_open::validate` accepts. The opener refuses `file:`
//! matches, so a detected click target can never smuggle an unsafe URI past
//! the open policy.

use super::*;
use crate::url_open::validate;

/// Every query cell resolves to the same complete URI, including separators and later parameters.
#[test]
fn query_separators_remain_inside_click_target() {
    let uri = "https://dev.azure.com/example/project/_git/repo?path=%2Fsrc%2Ffile.cs&line=1&lineEnd=10&_a=contents";
    for col in 0..uri.len() {
        let found = url_at_byte(uri, col).unwrap();
        assert_eq!(found.url, uri);
        assert_eq!(found.end, uri.len());
        assert!(validate(&found.url).is_ok());
    }
}

/// Prose wrappers do not turn a URL scheme into an embedded identifier.
#[test]
fn parenthesized_urls_have_the_same_target_as_plain_urls() {
    let uri = "https://leetcode.com/";
    for (text, start) in [(uri.to_string(), 0), (format!("LeetCode ({uri})"), 10)] {
        let found = url_at_byte(&text, start + 8).expect("URL inside prose wrapper");
        assert_eq!(found.url, uri);
        assert_eq!((found.start, found.end), (start, start + uri.len()));
    }
    assert!(find_urls("xhttps://leetcode.com/").is_empty());
}

// ---- scheme recognition ------------------------------------------------

#[test]
fn finds_each_supported_scheme() {
    for (text, want) in [
        ("http://a.com", "http://a.com"),
        ("https://a.com/p", "https://a.com/p"),
        ("mailto:user@a.com", "mailto:user@a.com"),
        ("file:///etc/hosts", "file:///etc/hosts"),
    ] {
        let found = find_urls(text);
        assert_eq!(found.len(), 1, "exactly one match in {text:?}");
        assert_eq!(found[0].url, want);
        assert_eq!(found[0].start, 0);
        assert_eq!(found[0].end, text.len());
    }
}

#[test]
fn ignores_text_without_a_supported_scheme() {
    for text in ["no url here", "ftp://x.com", "just words", "a@b.com", ""] {
        assert!(find_urls(text).is_empty(), "no match expected in {text:?}");
    }
}

// ---- scheme / identifier boundary --------------------------------------

#[test]
fn scheme_embedded_in_a_longer_identifier_is_not_a_match() {
    // The char before the scheme is a URL body char, so this is the
    // middle of a token (e.g. `xhttp://`) and must not be detected.
    assert!(find_urls("xhttp://a.com").is_empty());
    assert!(find_urls("foohttps://a.com").is_empty());
}

#[test]
fn scheme_after_a_non_body_char_is_a_match() {
    // A space (non-body) before the scheme opens a fresh match.
    let urls = find_urls("see http://a.com");
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "http://a.com");
    assert_eq!(urls[0].start, 4);
    assert_eq!(urls[0].end, 16);
}

#[test]
fn scheme_with_no_body_byte_is_not_a_match() {
    // Requires at least one body byte after the scheme.
    assert!(find_urls("http:// ").is_empty());
    assert!(find_urls("mailto: ").is_empty());
}

// ---- trailing punctuation ----------------------------------------------

#[test]
fn trims_single_trailing_punctuation() {
    for (text, want, end) in [
        ("http://a.com.", "http://a.com", 12),
        ("visit http://a.com!", "http://a.com", 18),
        ("http://a.com?", "http://a.com", 12),
    ] {
        let urls = find_urls(text);
        assert_eq!(urls.len(), 1, "one match in {text:?}");
        assert_eq!(urls[0].url, want);
        assert_eq!(urls[0].end, end);
    }
}

#[test]
fn trims_run_of_trailing_punctuation() {
    // `).,;` are all trimmed back to the bare URL.
    let urls = find_urls("http://a.com).,;");
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "http://a.com");
    assert_eq!(urls[0].start, 0);
    assert_eq!(urls[0].end, 12);
}

#[test]
fn matches_url_wrapped_in_prose() {
    // Opening wrappers delimit URLs without becoming part of their destination.
    for text in ["(http://a.com)", "[http://a.com]"] {
        let found = find_urls(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "http://a.com");
        assert_eq!((found[0].start, found[0].end), (1, 13));
    }
}

// ---- multiple URLs -----------------------------------------------------

#[test]
fn finds_multiple_urls_with_correct_byte_offsets() {
    let text = "see http://a.com and https://b.org here";
    let urls = find_urls(text);
    assert_eq!(urls.len(), 2);

    assert_eq!(urls[0].url, "http://a.com");
    assert_eq!(urls[0].start, 4);
    assert_eq!(urls[0].end, 16);
    assert_eq!(&text[urls[0].start..urls[0].end], urls[0].url);

    assert_eq!(urls[1].url, "https://b.org");
    assert_eq!(urls[1].start, 21);
    assert_eq!(urls[1].end, 34);
    assert_eq!(&text[urls[1].start..urls[1].end], urls[1].url);
}

// ---- UTF-8 safety ------------------------------------------------------

#[test]
fn multibyte_prefix_does_not_panic_and_offsets_are_byte_accurate() {
    // `❯` (U+276F) is 3 bytes; a scan must not panic slicing near it and
    // the reported offsets are byte offsets into the original string.
    let text = "❯ http://a.com";
    let urls = find_urls(text);
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "http://a.com");
    assert_eq!(urls[0].start, 4, "❯(3) + space(1) => url at byte 4");
    assert_eq!(&text[urls[0].start..urls[0].end], urls[0].url);
}

#[test]
fn multibyte_char_terminates_url_body_without_panic() {
    // A non-ASCII char right after the scheme body ends the match at a
    // valid char boundary (body scan only accepts ASCII body chars).
    let text = "http://a❯b";
    let urls = find_urls(text);
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "http://a");
    assert_eq!(&text[urls[0].start..urls[0].end], urls[0].url);
}

// ---- byte vs character column mapping ----------------------------------

#[test]
fn char_col_and_byte_col_lookups_agree_across_a_multibyte_prefix() {
    // `😀` (U+1F600) is 4 bytes but one char/one grid column.
    let text = "😀http://x.com";
    // Char column 1 is the 'h' at byte 4 — inside the URL.
    let by_char = url_at_char_col(text, 1).expect("char col 1 is inside the url");
    let by_byte = url_at_byte(text, 4).expect("byte 4 is inside the url");
    assert_eq!(by_char, by_byte);
    assert_eq!(by_char.url, "http://x.com");
    assert_eq!(by_char.start, 4);

    // Char column 0 is the emoji — before the URL — so no hit, and the
    // byte at 0 is likewise outside the URL span.
    assert!(url_at_char_col(text, 0).is_none(), "emoji column is not the url");
    assert!(url_at_byte(text, 0).is_none(), "byte 0 is not in the url span");
}

#[test]
fn char_col_beyond_text_returns_none() {
    let text = "😀http://x.com";
    assert!(url_at_char_col(text, 9999).is_none());
    assert!(url_at_byte(text, 9999).is_none());
}

// ---- the load-bearing invariant ----------------------------------------

#[test]
fn every_match_passes_open_policy() {
    // For a broad corpus of tricky rows, assert every returned slice is
    // (a) exactly the reported byte span, (b) accepted by the shared URI
    // check, and (c) accepted by the opener unless it is a `file:` URI,
    // which the opener refuses. This is the scanner's contract.
    let corpus = [
        "plain http://example.com/path?q=1#frag done",
        "email me at mailto:user.name+tag@example.com now",
        "local file:///Users/me/notes.txt opened",
        "wrapped (https://en.wikipedia.org/wiki/Rust) text",
        "trailing http://a.com. and http://b.com!",
        "two https://a.com https://b.com adjacent-ish",
        "unicode ❯ https://例え.example/パス maybe",
        "percent https://a.com/%20%26%3C encoded",
        "no-scheme just some ordinary prose without links",
        "mid-token xhttps://not-a-match.example here",
    ];
    for text in corpus {
        for url_match in find_urls(text) {
            assert!(url_match.start < url_match.end, "non-empty span for {text:?}");
            assert!(url_match.end <= text.len(), "span within bounds for {text:?}");
            assert_eq!(
                &text[url_match.start..url_match.end],
                url_match.url,
                "slice matches url for {text:?}"
            );
            assert!(
                check_uri(&url_match.url, SCHEMES).is_ok(),
                "scanner produced {:?} which fails the URI check (from {text:?})",
                url_match.url
            );
            let is_file = url_match.url.to_ascii_lowercase().starts_with("file:");
            assert_eq!(
                validate(&url_match.url).is_ok(),
                !is_file,
                "the opener must accept {:?} exactly when it is not a file URI",
                url_match.url
            );
        }
    }
}

/// Detection still finds `file://` spans so the app can classify them as local
/// targets, while the opener refuses them: detection and dispatch use separate
/// validators, and only the dispatch one gates the platform handler.
#[test]
fn file_uris_are_detected_but_refused_by_the_opener() {
    let found = find_urls("see file:///Users/me/notes.txt and file://host/share/x");
    let urls: Vec<&str> = found.iter().map(|url_match| url_match.url.as_str()).collect();
    assert_eq!(urls, ["file:///Users/me/notes.txt", "file://host/share/x"]);
    for url in urls {
        assert!(validate(url).is_err(), "the opener must refuse detected {url}");
    }
}

#[test]
fn overlong_url_is_dropped_because_it_fails_validate() {
    // A body longer than the 4096-byte cap fails the shared URI check (and so
    // `validate`), so the scanner must not return it — preserving the
    // invariant above.
    let text = format!("http://{}", "a".repeat(5000));
    assert!(text.len() > 4096);
    assert!(find_urls(&text).is_empty(), "overlong candidate must be dropped");
}

#[test]
fn shell_meta_in_body_ends_the_match_before_the_meta_char() {
    // A pipe remains outside the URL body and cannot enter the handler target.
    let text = "http://a.com/p?x=1|other";
    let urls = find_urls(text);
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "http://a.com/p?x=1");
    assert!(validate(&urls[0].url).is_ok());
    assert!(!urls[0].url.contains('|'));
}

/// `find_url_at` returns exactly the full scan's URL covering each byte, over every existing URL
/// fixture shape and a deterministic corpus that mixes schemes, wrappers, punctuation, spaces,
/// quotes and multi-byte characters, so scanning only the run around the pointer changes nothing.
#[test]
fn find_url_at_matches_the_full_scan_at_every_byte() {
    const PIECES: [&str; 18] = [
        "https://", "http://", "mailto:", "file://", "a.b", "/x", "(", ")", "[", "]", " ", ".",
        ",", "\"", "'", "\u{e9}", "\u{276f}", "\t",
    ];
    let mut seed = 0x0bad_5eed_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    let mut lines = vec![
        "see https://example.com/a?b=c#d.".to_owned(),
        "(https://ex.co/path) and [http://x.y/z], mailto:a@b.c".to_owned(),
        "xhttps://embedded.example file:///tmp/a".to_owned(),
    ];
    lines.extend(
        (0..2_000).map(|_| (0..1 + next() % 10).map(|_| PIECES[next() % PIECES.len()]).collect()),
    );
    for line in &lines {
        let full = find_urls(line);
        for byte in 0..=line.len() {
            let expected = full.iter().find(|url| byte >= url.start && byte < url.end);
            assert_eq!(find_url_at(line, byte).as_ref(), expected, "{line:?} at byte {byte}");
        }
    }
}
