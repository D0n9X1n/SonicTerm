//! Behavior tests for plain-text URL scanning.
//!
//! The load-bearing security property is the closing assertion in
//! `every_match_passes_open_policy`: whatever the scanner hands back is
//! always something `url_open::validate` will accept, so a detected
//! click target can never smuggle an unsafe URI past the open policy.

use super::*;
use crate::url_open::validate;

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
    let m = find_urls("see http://a.com");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a.com");
    assert_eq!(m[0].start, 4);
    assert_eq!(m[0].end, 16);
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
        let m = find_urls(text);
        assert_eq!(m.len(), 1, "one match in {text:?}");
        assert_eq!(m[0].url, want);
        assert_eq!(m[0].end, end);
    }
}

#[test]
fn trims_run_of_trailing_punctuation() {
    // `).,;` are all trimmed back to the bare URL.
    let m = find_urls("http://a.com).,;");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a.com");
    assert_eq!(m[0].start, 0);
    assert_eq!(m[0].end, 12);
}

#[test]
fn under_matches_url_wrapped_in_parens() {
    // Documents the intentional narrowness: a leading `(` is a body
    // char, so `(http://a.com)` is treated as mid-token and skipped
    // rather than mis-detected. Safe under-match, never an over-match.
    assert!(find_urls("(http://a.com)").is_empty());
}

// ---- multiple URLs -----------------------------------------------------

#[test]
fn finds_multiple_urls_with_correct_byte_offsets() {
    let text = "see http://a.com and https://b.org here";
    let m = find_urls(text);
    assert_eq!(m.len(), 2);

    assert_eq!(m[0].url, "http://a.com");
    assert_eq!(m[0].start, 4);
    assert_eq!(m[0].end, 16);
    assert_eq!(&text[m[0].start..m[0].end], m[0].url);

    assert_eq!(m[1].url, "https://b.org");
    assert_eq!(m[1].start, 21);
    assert_eq!(m[1].end, 34);
    assert_eq!(&text[m[1].start..m[1].end], m[1].url);
}

// ---- UTF-8 safety ------------------------------------------------------

#[test]
fn multibyte_prefix_does_not_panic_and_offsets_are_byte_accurate() {
    // `❯` (U+276F) is 3 bytes; a scan must not panic slicing near it and
    // the reported offsets are byte offsets into the original string.
    let text = "❯ http://a.com";
    let m = find_urls(text);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a.com");
    assert_eq!(m[0].start, 4, "❯(3) + space(1) => url at byte 4");
    assert_eq!(&text[m[0].start..m[0].end], m[0].url);
}

#[test]
fn multibyte_char_terminates_url_body_without_panic() {
    // A non-ASCII char right after the scheme body ends the match at a
    // valid char boundary (body scan only accepts ASCII body chars).
    let text = "http://a❯b";
    let m = find_urls(text);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a");
    assert_eq!(&text[m[0].start..m[0].end], m[0].url);
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
    // (a) exactly the reported byte span and (b) accepted by the same
    // validator that gates spawning. This is the scanner's contract.
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
        for m in find_urls(text) {
            assert!(m.start < m.end, "non-empty span for {text:?}");
            assert!(m.end <= text.len(), "span within bounds for {text:?}");
            assert_eq!(&text[m.start..m.end], m.url, "slice matches url for {text:?}");
            assert!(
                validate(&m.url).is_ok(),
                "scanner produced {:?} which fails validate() (from {text:?})",
                m.url
            );
        }
    }
}

#[test]
fn overlong_url_is_dropped_because_it_fails_validate() {
    // A body longer than the 4096-byte cap fails `validate`, so the
    // scanner must not return it — preserving the invariant above.
    let text = format!("http://{}", "a".repeat(5000));
    assert!(text.len() > 4096);
    assert!(find_urls(&text).is_empty(), "overlong candidate must be dropped");
}

#[test]
fn shell_meta_in_body_ends_the_match_before_the_meta_char() {
    // `&` is not a body char, so the match stops before it and the
    // returned slice is metacharacter-free (and validate-clean).
    let text = "http://a.com/p?x=1&y=2";
    let m = find_urls(text);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a.com/p?x=1");
    assert!(validate(&m[0].url).is_ok());
    assert!(!m[0].url.contains('&'));
}

/// Native path scanning distinguishes path provenance without changing URL-only APIs.
#[test]
fn finds_native_absolute_and_explicit_relative_paths() {
    let posix =
        find_targets_for_style("open /usr/local/etc then ./file and ../../file", PathStyle::Posix);
    assert_eq!(
        posix.iter().map(|m| &m.target).collect::<Vec<_>>(),
        vec![
            &DetectedTarget::PathCandidate("/usr/local/etc".into()),
            &DetectedTarget::PathCandidate("./file".into()),
            &DetectedTarget::PathCandidate("../../file".into()),
        ]
    );

    let windows = find_targets_for_style(
        r"open C:/Users/dotan then C:\Users\dotan and ..\..\file",
        PathStyle::Windows,
    );
    assert_eq!(
        windows.iter().map(|m| &m.target).collect::<Vec<_>>(),
        vec![
            &DetectedTarget::PathCandidate("C:/Users/dotan".into()),
            &DetectedTarget::PathCandidate(r"C:\Users\dotan".into()),
            &DetectedTarget::PathCandidate(r"..\..\file".into()),
        ]
    );
}

/// Roots, implicit relatives, drive-relative paths, and UNC paths never become candidates.
#[test]
fn rejects_ambiguous_or_unsupported_path_forms() {
    for text in ["/", "./", "../", "file", "~/file", "//server/share"] {
        assert!(
            find_targets_for_style(text, PathStyle::Posix).is_empty(),
            "unexpected POSIX target in {text:?}"
        );
    }
    for text in [r"C:\", "C:foo", "file", r"~\file", r"\\server\share"] {
        assert!(
            find_targets_for_style(text, PathStyle::Windows).is_empty(),
            "unexpected Windows target in {text:?}"
        );
    }
}

/// URI matches outrank path-looking slashes and remain absent from path-only quick select.
#[test]
fn typed_scanning_preserves_uri_precedence_and_url_compatibility() {
    let text = "https://example.com/a file:///tmp/a /tmp/b";
    let targets = find_targets_for_style(text, PathStyle::Posix);
    assert_eq!(
        targets.iter().map(|m| &m.target).collect::<Vec<_>>(),
        vec![
            &DetectedTarget::Uri("https://example.com/a".into()),
            &DetectedTarget::Uri("file:///tmp/a".into()),
            &DetectedTarget::PathCandidate("/tmp/b".into()),
        ]
    );
    assert_eq!(find_urls(text).len(), 2, "URL-only API must ignore raw paths");
}

/// Wrappers are excluded while ordinary filename punctuation and trailing separators survive.
#[test]
fn path_spans_obey_wrappers_and_preserve_filename_punctuation() {
    let text = "(/tmp/a.txt) [/tmp/b,] /tmp/c!/";
    let targets = find_targets_for_style(text, PathStyle::Posix);
    assert_eq!(
        targets.iter().map(|m| (&text[m.start..m.end], &m.target)).collect::<Vec<_>>(),
        vec![
            ("/tmp/a.txt", &DetectedTarget::PathCandidate("/tmp/a.txt".into())),
            ("/tmp/b,", &DetectedTarget::PathCandidate("/tmp/b,".into())),
            ("/tmp/c!/", &DetectedTarget::PathCandidate("/tmp/c!/".into())),
        ]
    );
}

/// Character-column lookup stays byte-accurate across a single-cell Unicode prefix and path.
#[test]
fn typed_path_lookup_maps_utf8_byte_and_character_columns() {
    let text = "é /tmp/café";
    let found =
        target_at_char_col_for_style(text, 4, PathStyle::Posix).expect("column inside the path");
    assert_eq!(found.target, DetectedTarget::PathCandidate("/tmp/café".into()));
    assert_eq!(&text[found.start..found.end], "/tmp/café");
}

/// Editor line/column suffixes are not interpreted as filesystem names.
#[test]
fn rejects_editor_location_suffixes() {
    for candidate in ["/tmp/file.rs:12", "/tmp/file.rs:12:4", "./file:9"] {
        assert!(find_targets_for_style(candidate, PathStyle::Posix).is_empty());
    }
    for candidate in [r"C:\work\file.rs:12", r".\file.rs:12:4"] {
        assert!(find_targets_for_style(candidate, PathStyle::Windows).is_empty());
    }
}
