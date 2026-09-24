//! Behavior tests for plain-text URL scanning.
//!
//! The load-bearing security property is the closing assertion in
//! `every_match_passes_open_policy`: whatever the scanner hands back is
//! always something `url_open::validate` will accept, so a detected
//! click target can never smuggle an unsafe URI past the open policy.

use super::*;
use crate::url_open::validate;

/// Tool headings expose the complete inner path without assigning it to wrapper cells.
#[test]
fn tool_heading_candidates_expose_inner_path() {
    for style in [PathStyle::Posix, PathStyle::Windows] {
        for prefix in ["Update", "Read", "Write", "Inspect"] {
            let path = "crates/sonicterm-app/src/app/mod_tests.rs";
            let text = format!("  ⏺ {prefix}({path})");
            let start = text.find(path).unwrap();
            let end = start + path.len();
            for (col, (byte, _)) in text.char_indices().enumerate() {
                let candidates = target_candidates_at_char_col_for_style(&text, col, style, true);
                let inner = candidates.iter().any(|candidate| {
                    candidate.start == start
                        && candidate.end == end
                        && candidate.target == DetectedTarget::PathCandidate(path.into())
                });
                assert_eq!(
                    inner,
                    (start..end).contains(&byte),
                    "{text}: col={col}, {candidates:?}"
                );
            }
        }
    }
}

/// Prose paths remain separately enumerable while real multiword names keep their literal candidate.
#[test]
fn prose_paths_and_literal_and_names_are_distinct_candidates() {
    let text = ".github/scripts/validate_release.py and focused tests/test_release_validation.py. Require stable";
    for style in [PathStyle::Posix, PathStyle::Windows] {
        for path in [".github/scripts/validate_release.py", "tests/test_release_validation.py"] {
            let start = text.find(path).unwrap();
            for col in start..start + path.len() {
                let candidates = target_candidates_at_char_col_for_style(text, col, style, true);
                assert!(
                    candidates.iter().any(|candidate| {
                        candidate.start == start
                            && candidate.end == start + path.len()
                            && candidate.target == DetectedTarget::PathCandidate(path.into())
                    }),
                    "{path}: col={col}, {candidates:?}"
                );
            }
        }
        for path in
            ["fixtures/foo and bar.py", "fixtures/and", "fixtures/and/x", "fixtures/candy.txt"]
        {
            for col in 0..path.len() {
                let candidates = target_candidates_at_char_col_for_style(path, col, style, true);
                assert!(
                    candidates.iter().any(|candidate| {
                        candidate.start == 0
                            && candidate.end == path.len()
                            && candidate.target == DetectedTarget::PathCandidate(path.into())
                    }),
                    "{path}: col={col}"
                );
            }
        }
    }
}

/// Tool extraction preserves literal parentheses and spaces while refusing malformed call boundaries.
#[test]
fn tool_heading_boundaries_preserve_literal_safety() {
    for style in [PathStyle::Posix, PathStyle::Windows] {
        for path in [
            "./fixtures/a(b).txt",
            "./fixtures/foo and bar.py",
            "./fixtures/and",
            "./fixtures/file.rs:12–14",
        ] {
            let text = format!("Read({path})");
            for col in 5..5 + path.chars().count() {
                let candidates = target_candidates_at_char_col_for_style(&text, col, style, true);
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| &text[candidate.start..candidate.end] == path),
                    "{text} col={col}"
                );
                assert!(candidates.len() <= MAX_PATH_CANDIDATES_PER_CELL);
            }
        }
        for text in [
            "Read(./fixtures/a.txt",
            "Read(./fixtures/a.txt))",
            "Read(./fixtures/a.txt)tail",
            "Read(./fixtures/a.txt)(extra)",
        ] {
            let candidates = target_candidates_at_char_col_for_style(text, 10, style, true);
            assert!(
                !candidates
                    .iter()
                    .any(|candidate| &text[candidate.start..candidate.end] == "./fixtures/a.txt"),
                "{text}"
            );
        }
        for path in ["./fixtures/a(b).txt", "./fixtures/foo and bar.py"] {
            let candidates = target_candidates_at_char_col_for_style(path, 4, style, true);
            assert!(candidates
                .iter()
                .any(|candidate| candidate.start == 0 && candidate.end == path.len()));
        }
    }
}

/// Unverified absolute paths stop at prose while an indivisible rooted spaced path remains intact.
#[test]
fn rooted_path_feedback_does_not_absorb_following_prose() {
    for (text, pointed, expected) in [
        ("/work/a.rs and focused tests/b.rs. Require stable", "a.rs", "/work/a.rs"),
        ("/work/a.rs and focused tests/b.rs. Require stable", "b.rs", "tests/b.rs"),
        ("/work/Test Folder/missing.rs", "missing", "/work/Test Folder/missing.rs"),
        ("/work/foo and bar.py", "bar.py", "/work/foo and bar.py"),
        ("/work/Test Folder/a.rs and tests/b.rs", "a.rs", "/work/Test Folder/a.rs"),
        ("/work/Test Folder/a.rs and tests/b.rs", "b.rs", "tests/b.rs"),
    ] {
        let candidates = target_candidates_at_char_col_for_style(
            text,
            text.find(pointed).unwrap(),
            PathStyle::Posix,
            true,
        );
        assert_eq!(
            explicit_path_feedback(
                candidates.iter().map(|candidate| &candidate.target),
                PathStyle::Posix
            ),
            Some(expected),
            "{text}"
        );
    }
}

/// Missing extensionless multiword guesses do not claim a prose boundary without validation.
#[test]
fn extensionless_spaced_feedback_requires_validation() {
    let text = "/tmp/My File and more";
    let candidates = target_candidates_at_char_col_for_style(text, 10, PathStyle::Posix, true);
    assert_eq!(
        explicit_path_feedback(
            candidates.iter().map(|candidate| &candidate.target),
            PathStyle::Posix
        ),
        None
    );
    assert!(candidates.iter().any(|candidate| &text[candidate.start..candidate.end] == text));
}

/// Spaced scans preserve the literal filename candidate for filesystem disambiguation.
#[test]
fn spaced_candidates_retain_literal_filename() {
    for name in ["test-local-link-actions.ps1", "my local script.ps1"] {
        for prefix in
            ["-a---  9/11/2026  3:42 PM           3984 ", " 9/11/2026  3:42 PM           3984 "]
        {
            let row = format!("{prefix}{name}");
            let col = row.find("script").unwrap_or_else(|| row.find("actions").unwrap());
            let candidates =
                target_candidates_at_char_col_for_style(&row, col, PathStyle::Windows, true);
            assert!(!candidates.is_empty());
            assert!(
                candidates.iter().any(|candidate| &row[candidate.start..candidate.end] == name),
                "{candidates:?}"
            );
        }
    }
}

/// Local line fragments identify source locations without becoming part of the filename.
#[test]
fn local_line_fragments_preserve_file_identity() {
    for uri in [
        "C:/work/CLAUDE.md#L3",
        "C:/work/CLAUDE.md#3",
        "file:c://work/CLAUDE.md#L3",
        "file:///C:/work/CLAUDE.md#3",
    ] {
        let target = local_link_target(uri, PathStyle::Windows).unwrap().unwrap();
        assert!(
            matches!(target, DetectedTarget::SourceReference(reference)
            if reference.path.ends_with("/CLAUDE.md") && reference.line == 3),
            "{uri}"
        );
    }
    for uri in
        ["file:c://work/main.rs#L0", "file:///C:/work/main.rs#Lbad", "file:c://work/main.rs#L9-L3"]
    {
        assert!(local_link_target(uri, PathStyle::Windows).is_err(), "{uri}");
    }
    assert_eq!(
        local_link_target("file:///C:/work/a%23L3.txt", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/a#L3.txt".into())))
    );
    assert_eq!(local_link_target("https://example.com/a#L3", PathStyle::Windows), Ok(None));
    assert_eq!(
        local_link_target("C:/work/a#Lemon.txt", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/a#Lemon.txt".into())))
    );
    assert_eq!(
        local_link_target("file:///C:/work/notes%233", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/notes#3".into())))
    );
    assert!(find_targets_for_style("C:/work/notes#3", PathStyle::Windows)
        .iter()
        .any(|target| target.target == DetectedTarget::PathCandidate("C:/work/notes#3".into())));
}

/// Drive-rooted file: links use file-URI decoding, never browser dispatch or drive-relative resolution.
#[test]
fn file_colon_drive_links_are_local_on_windows() {
    for (uri, path) in [
        ("file:c://work/a%20b.txt", "c://work/a b.txt"),
        ("FILE:C:/work/a%2520b.txt", "C:/work/a%20b.txt"),
        ("file:c://", "c://"),
    ] {
        assert_eq!(
            local_link_target(uri, PathStyle::Windows),
            Ok(Some(DetectedTarget::PathCandidate(path.into())))
        );
        assert!(local_link_target(uri, PathStyle::Posix).is_err());
    }
    for uri in [
        "file:c:relative.txt",
        "file:notes.txt",
        "file://server/share/file.txt",
        "file:c://work/%2e%2e/other.txt",
        "file:c://work/a%2fb.txt",
        "file:c://work/a%00b.txt",
        "file:c://work/a%3astream",
        "file:c://work/a%2",
    ] {
        assert!(local_link_target(uri, PathStyle::Windows).is_err(), "{uri}");
    }
}

/// Encoded path structure cannot redirect a displayed file URI into another directory.
#[test]
fn file_uri_rejects_encoded_separators_and_traversal() {
    for uri in [
        "file:///tmp/%2E%2E%2Fetc/passwd",
        "file:///tmp/%2e%2e/file",
        "file:///tmp/a%5cb",
        "file:///tmp/../file",
    ] {
        assert!(local_link_target(uri, PathStyle::Posix).is_err(), "{uri}");
    }
    assert!(local_link_target("file:///C:/Users/%2E%2E/Windows", PathStyle::Windows).is_err());
    assert_eq!(
        local_link_target("file:///", PathStyle::Posix),
        Ok(Some(DetectedTarget::PathCandidate("/".into())))
    );
    assert_eq!(
        local_link_target("file:///C:/", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/".into())))
    );
}

/// Explicit link destinations are classified whole, never through prose trimming or label fallback.
#[test]
fn local_link_destinations_preserve_native_paths_and_decode_file_uris_once() {
    for (input, expected) in [
        ("C://work/main.rs", "C://work/main.rs"),
        (r"C:\work\a%20b.txt", r"C:\work\a%20b.txt"),
        ("file:///C:/work/a%20b.txt", "C:/work/a b.txt"),
        ("file://localhost/C:/work/a%2520b.txt", "C:/work/a%20b.txt"),
    ] {
        assert_eq!(
            local_link_target(input, PathStyle::Windows),
            Ok(Some(DetectedTarget::PathCandidate(expected.into())))
        );
    }
    assert!(matches!(local_link_target("C://work/main.rs:7", PathStyle::Windows),
        Ok(Some(DetectedTarget::SourceReference(reference))) if reference.path == "C://work/main.rs" && reference.line == 7));
    assert_eq!(
        local_link_target("file:///tmp/a%20b.txt", PathStyle::Posix),
        Ok(Some(DetectedTarget::PathCandidate("/tmp/a b.txt".into())))
    );
    for target in [
        "file://server/share/a",
        "file:///C:/bad%00.txt",
        "file:///C:/bad%2",
        "file:///C:/x%3Astream",
        "C://work/a.exe:bad:2",
    ] {
        assert!(local_link_target(target, PathStyle::Windows).is_err(), "{target}");
    }
    for target in ["https://example.com/?a=1&b=2", "vscode://file/C:/a.rs", "C:notes.txt"] {
        assert_eq!(local_link_target(target, PathStyle::Windows), Ok(None));
    }
    assert_eq!(local_link_target("C://work/main.rs", PathStyle::Posix), Ok(None));
}

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
    // A pipe remains outside the URL body and cannot enter the handler target.
    let text = "http://a.com/p?x=1|other";
    let m = find_urls(text);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].url, "http://a.com/p?x=1");
    assert!(validate(&m[0].url).is_ok());
    assert!(!m[0].url.contains('|'));
}

/// Native compatibility wrappers preserve the explicit native-grammar behavior.
#[allow(deprecated)]
#[test]
fn native_style_wrappers_match_explicit_native_style() {
    let style = PathStyle::native();
    let text = if style == PathStyle::Windows {
        r"open C:\Users\name\file and https://example.com"
    } else {
        "open /Users/name/file and https://example.com"
    };

    assert_eq!(find_targets(text), find_targets_for_style(text, style));
    for byte_col in 0..=text.len() {
        assert_eq!(target_at_byte(text, byte_col), target_at_byte_for_style(text, byte_col, style));
    }
    for char_col in 0..=text.chars().count() {
        assert_eq!(
            target_at_char_col(text, char_col),
            target_at_char_col_for_style(text, char_col, style),
        );
    }
}

/// Native path scanning covers absolute, dot-relative, and current-home-relative syntax.
#[test]
fn finds_supported_native_path_forms() {
    let posix = find_targets_for_style(
        "open /usr/local/etc then ./file ../../file ~/notes ~/.config/file src/main.rs",
        PathStyle::Posix,
    );
    assert_eq!(
        posix.iter().map(|m| &m.target).collect::<Vec<_>>(),
        vec![
            &DetectedTarget::PathCandidate("/usr/local/etc".into()),
            &DetectedTarget::PathCandidate("./file".into()),
            &DetectedTarget::PathCandidate("../../file".into()),
            &DetectedTarget::PathCandidate("~/notes".into()),
            &DetectedTarget::PathCandidate("~/.config/file".into()),
            &DetectedTarget::PathCandidate("src/main.rs".into()),
        ]
    );

    let windows = find_targets_for_style(
        r"open C:/Users/dotan C:\Users\dotan ..\..\file ~\notes ~/AppData/file src\main.rs lib/main.rs",
        PathStyle::Windows,
    );
    assert_eq!(
        windows.iter().map(|m| &m.target).collect::<Vec<_>>(),
        vec![
            &DetectedTarget::PathCandidate("C:/Users/dotan".into()),
            &DetectedTarget::PathCandidate(r"C:\Users\dotan".into()),
            &DetectedTarget::PathCandidate(r"..\..\file".into()),
            &DetectedTarget::PathCandidate(r"~\notes".into()),
            &DetectedTarget::PathCandidate("~/AppData/file".into()),
            &DetectedTarget::PathCandidate(r"src\main.rs".into()),
            &DetectedTarget::PathCandidate("lib/main.rs".into()),
        ]
    );
}

/// The supported target matrix keeps every URI and native path family typed distinctly.
#[test]
fn supported_plain_text_target_matrix_is_detected() {
    for (style, text, expected) in [
        (PathStyle::Posix, "http://example.com", DetectedTarget::Uri("http://example.com".into())),
        (
            PathStyle::Posix,
            "https://example.com/path",
            DetectedTarget::Uri("https://example.com/path".into()),
        ),
        (
            PathStyle::Posix,
            "mailto:user@example.com",
            DetectedTarget::Uri("mailto:user@example.com".into()),
        ),
        (PathStyle::Posix, "file:///tmp/file", DetectedTarget::Uri("file:///tmp/file".into())),
        (PathStyle::Posix, "/tmp/file", DetectedTarget::PathCandidate("/tmp/file".into())),
        (PathStyle::Posix, "./file", DetectedTarget::PathCandidate("./file".into())),
        (PathStyle::Posix, "../file", DetectedTarget::PathCandidate("../file".into())),
        (PathStyle::Posix, "~/file", DetectedTarget::PathCandidate("~/file".into())),
        (PathStyle::Posix, "src/main.rs", DetectedTarget::PathCandidate("src/main.rs".into())),
        (
            PathStyle::Windows,
            r"C:\Users\name\file",
            DetectedTarget::PathCandidate(r"C:\Users\name\file".into()),
        ),
        (
            PathStyle::Windows,
            "C:/Users/name/file",
            DetectedTarget::PathCandidate("C:/Users/name/file".into()),
        ),
        (PathStyle::Windows, r".\file", DetectedTarget::PathCandidate(r".\file".into())),
        (PathStyle::Windows, r"..\file", DetectedTarget::PathCandidate(r"..\file".into())),
        (PathStyle::Windows, r"~\file", DetectedTarget::PathCandidate(r"~\file".into())),
        (PathStyle::Windows, "~/file", DetectedTarget::PathCandidate("~/file".into())),
        (PathStyle::Windows, r"src\main.rs", DetectedTarget::PathCandidate(r"src\main.rs".into())),
        (PathStyle::Windows, "src/main.rs", DetectedTarget::PathCandidate("src/main.rs".into())),
    ] {
        let found = find_targets_for_style(text, style);
        assert_eq!(found.len(), 1, "one target expected in {text:?}");
        assert_eq!(found[0].target, expected, "wrong target provenance for {text:?}");
        assert_eq!(&text[found[0].start..found[0].end], text, "wrong span for {text:?}");
    }

    for style in [PathStyle::Posix, PathStyle::Windows] {
        let bare = bare_name_at_char_col_for_style("sonicterm", 2, style)
            .expect("a whole contextual component remains supported");
        assert_eq!(bare.target, DetectedTarget::BareName("sonicterm".into()));
    }
}

/// Roots, implicit relatives, named-home expansion, variables, and network paths stay inert.
#[test]
fn rejects_ambiguous_or_unsupported_path_forms() {
    for text in [
        "/",
        "./",
        "../",
        "~/",
        "file",
        "~other/file",
        "$HOME/file",
        "${HOME}/file",
        "//server/share",
    ] {
        assert!(
            find_targets_for_style(text, PathStyle::Posix).is_empty(),
            "unexpected POSIX target in {text:?}"
        );
    }
    for text in
        [r"C:\", "C:foo", "file", "~\\", r"~other\file", r"%USERPROFILE%\file", r"\\server\share"]
    {
        assert!(
            find_targets_for_style(text, PathStyle::Windows).is_empty(),
            "unexpected Windows target in {text:?}"
        );
    }
}

/// Quoted paths and unsupported URI lookalikes remain inert rather than becoming relative paths.
#[test]
fn rejects_quoted_paths_and_unsupported_uri_lookalikes() {
    for text in [
        "\"~/file\"",
        "'src/main.rs'",
        "`src/main.rs`",
        "src/\"main.rs\"",
        "src/'main.rs'",
        "src/`main.rs`",
        "src/(main.rs)",
        "src/",
        "src/\nmain.rs",
        "src/\rmain.rs",
        "src/\tmain.rs",
        "ftp://example.com/file",
    ] {
        assert!(
            find_targets_for_style(text, PathStyle::Posix).is_empty(),
            "ambiguous or unsupported target detected in {text:?}"
        );
    }
    for text in [r#"src\"main.rs\""#, r"src\'main.rs'", r"src\`main.rs`", "src\\", "src\\\nmain.rs"]
    {
        assert!(
            find_targets_for_style(text, PathStyle::Windows).is_empty(),
            "ambiguous Windows target detected in {text:?}"
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

/// Focused path lookup preserves legal filename punctuation and offers a shorter prose alternate.
#[test]
fn focused_candidates_include_literal_and_prose_trimmed_paths() {
    for (path, punctuation) in [
        ("lua/config/plugins/lsp.lua", ','),
        ("lua/config/autocmds.lua", ';'),
        ("scripts/smoke.sh", ';'),
        ("lua/config/theme.lua", '.'),
    ] {
        let text = format!("{path}{punctuation}");
        for col in 0..path.chars().count() {
            let candidates =
                target_candidates_at_char_col_for_style(&text, col, PathStyle::Posix, true);
            let literal = candidates
                .iter()
                .position(|matched| text[matched.start..matched.end] == text)
                .expect("legal punctuation-ending filename remains a literal candidate");
            let trimmed = candidates
                .iter()
                .position(|matched| &text[matched.start..matched.end] == path)
                .expect("prose punctuation produces a shorter alternate");
            assert!(literal < trimmed, "literal identity must be probed before its alternate");
        }

        let punctuation_col = path.chars().count();
        let candidates =
            target_candidates_at_char_col_for_style(&text, punctuation_col, PathStyle::Posix, true);
        assert!(candidates.iter().all(|matched| matched.end == text.len()));
    }
}

/// Candidate pressure keeps a punctuation-bearing literal and its fully trimmed fallback together.
#[test]
fn candidate_cap_keeps_literal_and_final_fallback_atomic() {
    let text = "a0 a1 a2 a3 a4 a5 a6 src/main.rs,. z0 z1 z2 z3 z4 z5 z6";
    let candidates = target_candidates_at_char_col_for_style(
        text,
        text.find("main").unwrap(),
        PathStyle::Posix,
        true,
    );
    let literal = "src/main.rs,.";
    let trimmed = "src/main.rs";
    let start = text.find(literal).unwrap();

    assert!(candidates.len() <= MAX_PATH_CANDIDATES_PER_CELL);
    assert!(candidates.iter().any(|matched| {
        matched.start == start
            && &text[matched.start..matched.end] == literal
            && matched.target == DetectedTarget::PathCandidate(literal.into())
    }));
    assert!(candidates.iter().any(|matched| {
        matched.start == start
            && &text[matched.start..matched.end] == trimmed
            && matched.target == DetectedTarget::PathCandidate(trimmed.into())
    }));
}

/// A maximum-length punctuation run emits only the literal, one-trim, and full-trim tiers.
#[test]
fn long_punctuation_run_keeps_candidate_construction_bounded() {
    let path = "src/main.rs";
    let text = format!("{path}{}", ",".repeat(MAX_TARGET_BYTES - path.len()));
    let candidates = target_candidates_at_char_col_for_style(&text, 2, PathStyle::Posix, true);

    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0].end, text.len());
    assert_eq!(candidates[1].end, text.len() - 1);
    assert_eq!(&text[candidates[2].start..candidates[2].end], path);
}

/// Windows sentence periods can fall back without permitting trailing-dot normalization aliases.
#[test]
fn windows_focused_paths_offer_period_trimmed_fallbacks() {
    let text = r"src\main.rs.";
    let candidates = target_candidates_at_char_col_for_style(text, 2, PathStyle::Windows, true);

    assert!(candidates.iter().any(|matched| {
        &text[matched.start..matched.end] == r"src\main.rs"
            && matched.target == DetectedTarget::PathCandidate(r"src\main.rs".into())
    }));
    assert!(candidates.iter().all(|matched| &text[matched.start..matched.end] != text));
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

/// Contextual bare-name scanning keeps provenance separate from explicit paths and URIs.
#[test]
fn contextual_bare_names_cover_ls_tokens_without_widening_target_scan() {
    let text = "drwxr-xr-x user 18 Aug 12:30 sonicterm";
    let found = bare_name_at_char_col_for_style(text, 34, PathStyle::Posix)
        .expect("column inside the ls name");
    assert_eq!(found.target, DetectedTarget::BareName("sonicterm".into()));
    assert_eq!(&text[found.start..found.end], "sonicterm");
    assert!(
        find_targets_for_style(text, PathStyle::Posix).is_empty(),
        "ordinary words must remain absent from explicit target APIs"
    );

    let dotfile = bare_name_at_char_col_for_style(".DS_Store", 2, PathStyle::Posix)
        .expect("dotfile is one contextual component");
    assert_eq!(dotfile.target, DetectedTarget::BareName(".DS_Store".into()));
    assert!(
        bare_name_at_char_col_for_style(text, 10, PathStyle::Posix).is_none(),
        "whitespace must not inherit the token before it"
    );
}

/// Contextual bare-name grammar rejects path syntax, editor suffixes, and ambiguous pseudo-components.
#[test]
fn contextual_bare_names_reject_non_components() {
    for candidate in [".", "..", "./file", "../file", "/tmp/file", "file:12", "a/b", "a\\b"] {
        assert!(
            bare_name_at_char_col_for_style(candidate, 0, PathStyle::Posix).is_none(),
            "unexpected contextual POSIX name {candidate:?}"
        );
    }
    for candidate in [".", "..", r".\file", r"C:\file", "file:12", "a/b", r"a\b"] {
        assert!(
            bare_name_at_char_col_for_style(candidate, 0, PathStyle::Windows).is_none(),
            "unexpected contextual Windows name {candidate:?}"
        );
    }
}

/// Quoted or decorated `ls` output stays inert because it does not identify one exact component.
#[test]
fn contextual_bare_names_reject_quoted_and_classified_output() {
    for (text, col) in [
        ("'sonicterm'", 2),
        ("\"sonicterm\"", 2),
        ("`sonicterm`", 2),
        ("sonicterm*", 2),
        ("sonicterm@", 2),
        ("sonicterm=", 2),
        ("sonicterm|", 2),
        (r"sonicterm\ name", 2),
    ] {
        assert!(
            bare_name_at_char_col_for_style(text, col, PathStyle::Posix).is_none(),
            "ambiguous output became a contextual target: {text:?}"
        );
    }
}

/// Shell-quoted spaced names unwrap only one complete contextual component.
#[test]
fn shell_quoted_spaced_names_produce_one_unwrapped_candidate() {
    let text = "drwxr-xr-x@ - user 23 Aug 21:54 'ff ff'";
    let start = text.find("ff ff").unwrap();
    let start_col = text[..start].chars().count();
    for col in start_col..start_col + "ff ff".chars().count() {
        assert_eq!(
            target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true),
            vec![TargetMatch {
                start,
                end: start + "ff ff".len(),
                source_start: start - 1,
                source_end: start + "ff ff".len() + 1,
                missing_before: Vec::new(),
                target: DetectedTarget::BareName("ff ff".into()),
            }]
        );
    }
    assert!(target_candidates_at_char_col_for_style(text, start_col, PathStyle::Posix, false)
        .is_empty());

    for ambiguous in [
        "key='ff ff'",
        "prefix'ff ff'",
        "'ff ff'suffix",
        "'ff ff",
        "ff ff'",
        "\"ff ff\"",
        "'one two three four five six seven eight nine'",
    ] {
        let col = ambiguous.find(' ').unwrap();
        assert!(
            target_candidates_at_char_col_for_style(ambiguous, col, PathStyle::Posix, true)
                .is_empty(),
            "ambiguous shell text became clickable: {ambiguous:?}"
        );
    }

    let fragments = "'ab ' ' cd'";
    let inter_quote_space = fragments.find("' '").unwrap() + 1;
    assert!(target_candidates_at_char_col_for_style(
        fragments,
        inter_quote_space,
        PathStyle::Posix,
        true,
    )
    .is_empty());
}

/// Balanced presentation quotes expose the whole native path, never quotes or surrounding command text.
#[test]
fn balanced_quoted_paths_cover_exact_inner_spans() {
    for (style, path) in [
        (PathStyle::Windows, r"C:\Users\dotan\OneDrive - Microsoft\Desktop\Promotion Analysis"),
        (PathStyle::Windows, r"C:\work\file.txt"),
        (PathStyle::Windows, r".\My Folder\file.txt"),
        (PathStyle::Windows, r"~\My Folder\file.txt"),
        (PathStyle::Windows, r"src\My Folder\café.rs"),
        (PathStyle::Posix, "/tmp/My Folder/file.txt"),
        (PathStyle::Posix, "./My Folder/file.txt"),
        (PathStyle::Posix, "~/My Folder/file.txt"),
        (PathStyle::Posix, "src/café.rs"),
        (PathStyle::Posix, "/tmp/name(with)[braces]{and},punct!.txt"),
    ] {
        for quote in ['\'', '"', '`'] {
            for (prefix, suffix) in [
                ("", ""),
                ("python -m http.server 8766 --bind 127.0.0.1 --directory ", "  stopped"),
                ("é (", ")."),
                ("see [", "],"),
                ("see {", "};"),
            ] {
                let text = format!("{prefix}{quote}{path}{quote}{suffix}");
                let start = prefix.len() + quote.len_utf8();
                let end = start + path.len();
                for (col, (byte, _)) in text.char_indices().enumerate() {
                    let matches = target_candidates_at_char_col_for_style(&text, col, style, true);
                    let exact = matches.iter().any(|m| {
                        m.start == start
                            && m.end == end
                            && m.target == DetectedTarget::PathCandidate(path.into())
                    });
                    assert_eq!(exact, (start..end).contains(&byte), "{text:?} at {col}");
                }
            }
        }
    }
}

/// Quoted source suffixes retain location metadata without treating literal internal punctuation as prose.
#[test]
fn balanced_quoted_source_references_preserve_metadata() {
    for style in [PathStyle::Posix, PathStyle::Windows] {
        for quote in ['\'', '"', '`'] {
            let text = format!("see {quote}src/My Folder/café.rs:12:4{quote}.");
            let col = text.find("café").unwrap();
            let found = target_candidates_at_char_col_for_style(&text, col, style, true);
            assert!(found.iter().any(|m| matches!(&m.target,
                DetectedTarget::SourceReference(r) if r.path == "src/My Folder/café.rs"
                    && r.line == 12 && r.column == Some(4))));
            let text = format!("{quote}/tmp/file.txt,{quote}");
            if style == PathStyle::Posix {
                assert_eq!(
                    target_candidates_at_char_col_for_style(&text, 3, style, true)[0].target,
                    DetectedTarget::PathCandidate("/tmp/file.txt,".into())
                );
            }
        }
    }
}

/// A grouped citation inherits only its explicit first path and validates every location before returning any member.
#[test]
fn grouped_source_references_select_pointed_location() {
    for (style, path) in
        [(PathStyle::Posix, "src/café.rs"), (PathStyle::Windows, r"C:\work\main.rs")]
    {
        for (left, right) in [("", ""), ("(", ")."), ("[", "],"), ("{", "};")] {
            for separator in [", ", ","] {
                let body = format!("{path}:924{separator}:934:2{separator}:375–380");
                let prefix = if left.is_empty() { "" } else { "é " };
                let text = format!("{prefix}{left}{body}{right}");
                let start = text.find(path).unwrap();
                for (needle, line, column, end_line) in [
                    (path, 924, None, None),
                    (":934", 934, Some(2), None),
                    (":375", 375, None, Some(380)),
                ] {
                    let byte = text.find(needle).unwrap();
                    let col = text[..byte].chars().count();
                    let found = target_candidates_at_char_col_for_style(&text, col, style, true);
                    assert!(
                        found.iter().any(|m| m.start == start
                            && m.end == start + body.len()
                            && matches!(&m.target, DetectedTarget::SourceReference(r)
                            if r.path == path && r.display == body && r.line == line
                                && r.column == column && r.end_line == end_line)),
                        "{text:?} at {needle}"
                    );
                }
                for (col, (_, ch)) in text.char_indices().enumerate() {
                    if ch == ',' || ch == ' ' {
                        assert!(!target_candidates_at_char_col_for_style(&text, col, style, true)
                            .iter().any(|m| matches!(&m.target, DetectedTarget::SourceReference(r) if r.path == path)));
                    }
                }
            }
        }
    }
}

/// Groups stay local to their own citation and do not hide later independent paths or citations.
#[test]
fn grouped_source_references_do_not_capture_neighbors() {
    for text in [
        "(src/a.rs:12, :14) /tmp/other.rs",
        "(src/a.rs:12, :0) /tmp/other.rs",
        "src/a.rs:12, :14 /tmp/other.rs",
        "src/a.rs:12, :0 /tmp/other.rs",
    ] {
        let start = text.find("/tmp").unwrap();
        assert!(
            target_candidates_at_char_col_for_style(text, start, PathStyle::Posix, true)
                .iter()
                .any(|m| m.target == DetectedTarget::PathCandidate("/tmp/other.rs".into())),
            "{text}"
        );
    }
    let text = "(src/a.rs:12, :14) [src/b.rs:20, :24].";
    let col = text.find(":24").unwrap();
    assert!(target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
        .iter().any(|m| matches!(&m.target, DetectedTarget::SourceReference(r) if r.path == "src/b.rs" && r.line == 24)));
    let text = "(src/a.rs:12,   :14)";
    assert!(target_candidates_at_char_col_for_style(text, 2, PathStyle::Posix, true)
        .iter()
        .any(|m| matches!(&m.target, DetectedTarget::SourceReference(r) if r.path == "src/a.rs")));
}

/// Ambiguous spaced anchors never expose a shorter filename, but independent following groups retain their own ownership.
#[test]
fn grouped_source_anchors_never_truncate_spaced_paths() {
    for text in [
        "My Folder/a.rs:1, :2",
        "see src/a.rs:1, :2",
        "(My Folder/a.rs:1, :2)",
        "  My Folder/a.rs:1, :2",
    ] {
        for col in 0..text.chars().count() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
                    .is_empty(),
                "{text:?} at {col}"
            );
        }
    }
    for text in [
        "src/a.rs:1, :2 src/b.rs:3, :4",
        "(My Folder/a.rs:1, :2) [src/b.rs:3, :4]",
        "  src/b.rs:3, :4",
        "see (src/b.rs:3, :4)",
    ] {
        let col = text.find(":4").unwrap();
        let found = target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true);
        assert_eq!(found.len(), 1, "{text}");
        assert!(
            matches!(&found[0].target, DetectedTarget::SourceReference(r) if r.path == "src/b.rs" && r.line == 4),
            "{text}"
        );
    }
}

/// Wrapper-looking characters inside spaced paths cannot replace a rooted anchor with a relative suffix.
#[test]
fn grouped_source_wrappers_do_not_discard_path_prefixes() {
    for (style, text) in [
        (PathStyle::Posix, "/tmp/My (Folder/a.rs:1, :2)"),
        (PathStyle::Posix, "/tmp/My long folder [Folder/a.rs:1, :2]"),
        (PathStyle::Windows, r"C:\My (Folder\a.rs:1, :2)"),
        (PathStyle::Windows, r"C:\My long folder {Folder\a.rs:1, :2}"),
    ] {
        for (col, _) in text.char_indices().enumerate() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, style, true).is_empty(),
                "{text} at {col}"
            );
        }
    }
}

/// Structured targets retain byte budgets, the eight-location limit, and explicit-path configuration independence.
#[test]
fn structured_path_limits_and_uri_precedence_hold() {
    for style in [PathStyle::Posix, PathStyle::Windows] {
        let path = format!("./{}", "x".repeat(MAX_TARGET_BYTES - 2));
        for extra in ["", "x"] {
            let text = format!("'{path}{extra}'");
            let found = target_candidates_at_char_col_for_style(&text, 3, style, false);
            assert_eq!(found.len(), usize::from(extra.is_empty()));
        }
        let text = "(src/a.rs:1, :2, :3, :4, :5, :6, :7, :8)";
        let found =
            target_candidates_at_char_col_for_style(text, text.find(":8").unwrap(), style, false);
        assert_eq!(found.len(), 1);
        assert!(matches!(&found[0].target, DetectedTarget::SourceReference(r) if r.line == 8));
        let text = "'https://example.com/a' src/a.rs:1, :2";
        let found = target_candidates_at_char_col_for_style(text, 4, style, true);
        assert_eq!(found.len(), 1);
        assert!(
            matches!(&found[0].target, DetectedTarget::Uri(uri) if uri == "https://example.com/a")
        );
    }
}

/// Incomplete quotes, concatenation, invalid groups, expansion syntax and over-limit groups never expose partial targets.
#[test]
fn structured_paths_reject_ambiguous_or_malformed_input() {
    for text in [
        "'/tmp/My Folder",
        "/tmp/My Folder'",
        "'/tmp/My Folder\"",
        "prefix'/tmp/My Folder'",
        "'/tmp/My Folder'suffix",
        "' /tmp/My Folder '",
        "\"/tmp/$HOME/file\"",
        "`/tmp/$(command)/file`",
        "\"/tmp/a\\ b\"",
        "(src/file.rs:12, :0)",
        "(src/file.rs:12, :13:0)",
        "(src/file.rs:12, :14-13)",
        "(src/file.rs:12, :4294967296)",
        "(src/file.rs:12, :abc)",
        "(src/file.rs:12, :13].",
        "(src/file.rs:12, :13",
        "(src/file.rs:12, :13)tail",
        "(src/file.rs:12, :13,)",
        "(src/file.rs:1, :2, :3, :4, :5, :6, :7, :8, :9)",
    ] {
        let col = text.find("tmp").or_else(|| text.find("src")).unwrap();
        assert!(
            target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true).is_empty(),
            "{text:?}"
        );
    }
}

/// Spaced explicit and contextual paths produce bounded full-span candidates on every cell.
#[test]
fn spaced_path_candidates_cover_each_pointed_cell() {
    for (style, text, expected) in [
        (PathStyle::Windows, r"C:\Program Files\SonicTerm", r"C:\Program Files\SonicTerm"),
        (PathStyle::Windows, r"~\My Folder\file.txt", r"~\My Folder\file.txt"),
        (PathStyle::Windows, r"src\My Folder\file.txt", r"src\My Folder\file.txt"),
        (PathStyle::Windows, r"My Folder\file.txt", r"My Folder\file.txt"),
        (PathStyle::Posix, "/tmp/My Folder", "/tmp/My Folder"),
        (PathStyle::Posix, "~/My Folder/file.txt", "~/My Folder/file.txt"),
        (PathStyle::Posix, "./My Folder/file.txt", "./My Folder/file.txt"),
        (PathStyle::Posix, "src/My Folder/file.txt", "src/My Folder/file.txt"),
        (PathStyle::Posix, "src/My Folder (copy)/file.txt", "src/My Folder (copy)/file.txt"),
        (PathStyle::Posix, "src/My Folder [copy]/file.txt", "src/My Folder [copy]/file.txt"),
        (PathStyle::Posix, "src/My Folder {copy}/file.txt", "src/My Folder {copy}/file.txt"),
        (PathStyle::Posix, "My Folder/file.txt", "My Folder/file.txt"),
        (PathStyle::Posix, "My Folder", "My Folder"),
    ] {
        let character_count = text.chars().count();
        for col in 0..character_count {
            let matches = target_candidates_at_char_col_for_style(text, col, style, true);
            assert!(
                matches.iter().any(|matched| {
                    &text[matched.start..matched.end] == expected
                        && matches!(
                            matched.target,
                            DetectedTarget::PathCandidate(_) | DetectedTarget::BareName(_)
                        )
                }),
                "missing full candidate at column {col} in {text:?}: {matches:?}"
            );
            assert!(matches.len() <= MAX_PATH_CANDIDATES_PER_CELL);
        }
    }
}

/// Spaced separator-relative paths retain explicit provenance without bare-name fallback.
#[test]
fn spaced_contextual_paths_keep_path_candidate_provenance() {
    for (style, text) in
        [(PathStyle::Windows, r"My Folder\file.txt"), (PathStyle::Posix, "My Folder/file.txt")]
    {
        for col in 0..text.chars().count() {
            let matches = target_candidates_at_char_col_for_style(text, col, style, false);
            assert!(
                matches.iter().any(|matched| {
                    matched.start == 0
                        && matched.end == text.len()
                        && matched.target == DetectedTarget::PathCandidate(text.into())
                }),
                "missing explicit full candidate at column {col} in {text:?}: {matches:?}"
            );
        }
    }
}

/// Whole-row scanning keeps ordinary spaces as path-token boundaries.
#[test]
fn whole_row_scanning_does_not_join_spaced_path_tokens() {
    for (style, text, expected_start) in [
        (PathStyle::Windows, r"My Folder\file.txt", None),
        (PathStyle::Posix, "My Folder/file.txt", None),
        (PathStyle::Windows, r"  src\main.rs", Some(2)),
        (PathStyle::Posix, "  src/main.rs", Some(2)),
    ] {
        let matches = find_targets_for_style(text, style);
        assert!(matches.iter().all(|matched| match &matched.target {
            DetectedTarget::PathCandidate(candidate) => {
                !candidate.starts_with(' ') && matched.start != 1
            }
            DetectedTarget::Uri(_)
            | DetectedTarget::BareName(_)
            | DetectedTarget::SourceReference(_) => true,
        }));
        if let Some(expected_start) = expected_start {
            assert!(matches.iter().any(|matched| {
                matched.start == expected_start
                    && matched.end == text.len()
                    && matched.target
                        == DetectedTarget::PathCandidate(text[expected_start..].into())
            }));
        } else {
            assert!(matches.iter().all(|matched| matched.start > 0 || matched.end < text.len()));
        }
    }
}

/// Windows spaced candidates reject trailing-dot and trailing-space normalization aliases.
#[test]
fn windows_spaced_candidates_reject_normalization_aliases() {
    for text in [r"C:\tmp\bad.\name", r"C:\tmp\bad \name", "My Folder "] {
        assert!(
            target_candidates_at_char_col_for_style(
                text,
                text.chars().count().saturating_sub(1),
                PathStyle::Windows,
                true,
            )
            .iter()
            .all(|matched| &text[matched.start..matched.end] != text),
            "normalization-sensitive candidate detected in {text:?}"
        );
    }
}

/// Wrappers and prose endings compose without changing inner spans, location metadata, or pointer ownership.
#[test]
fn wrapped_paths_with_prose_endings_preserve_exact_targets() {
    let mut failures = Vec::new();
    for (style, path) in [
        (PathStyle::Posix, "src/main.rs"),
        (PathStyle::Posix, "./src/main.rs"),
        (PathStyle::Posix, "/tmp/main.rs"),
        (PathStyle::Posix, "~/src/main.rs"),
        (PathStyle::Posix, "/tmp/My Folder/main.rs"),
        (PathStyle::Posix, "src/café.rs"),
        (PathStyle::Windows, r"C:\work\main.rs"),
        (PathStyle::Windows, r"src\main.rs"),
        (PathStyle::Windows, r".\src\main.rs"),
    ] {
        for (suffix, column, end_line) in [
            ("", None, None),
            (":97", None, None),
            (":97:4", Some(4), None),
            (":97-100", None, Some(100)),
            (":97–100", None, Some(100)),
        ] {
            let display = format!("{path}{suffix}");
            let expected = if suffix.is_empty() {
                DetectedTarget::PathCandidate(path.into())
            } else {
                DetectedTarget::SourceReference(SourceReference {
                    path: path.into(),
                    display: display.clone(),
                    line: 97,
                    column,
                    end_line,
                    explicit_path: true,
                })
            };
            for (left, right) in [("", ""), ("(", ")"), ("[", "]"), ("{", "}"), ("open(", ")")] {
                for ending in ["", ".", ",", ";", ":", "!", "?", ",.!?"] {
                    let prefix = format!("é {left}");
                    let text = format!("{prefix}{display}{right}{ending}");
                    let start = prefix.len();
                    let end = start + display.len();
                    let mut missing = 0;
                    for (col, (byte, _)) in text.char_indices().enumerate() {
                        let candidates =
                            target_candidates_at_char_col_for_style(&text, col, style, true);
                        let exact = candidates
                            .iter()
                            .any(|m| m.start == start && m.end == end && m.target == expected);
                        if (start..end).contains(&byte) {
                            missing += usize::from(!exact);
                        } else {
                            assert!(!exact, "exterior column {col} owns inner target: {text:?}");
                        }
                        assert!(candidates.len() <= MAX_PATH_CANDIDATES_PER_CELL);
                    }
                    if missing > 0 {
                        failures.push(format!("{style:?} {text:?}: {missing} missing columns"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failing combinations; first: {:?}",
        failures.len(),
        failures.first()
    );
}

/// Outer sentence endings must not erase an inner punctuation-bearing literal or filename parentheses.
#[test]
fn wrapped_paths_with_prose_endings_preserve_literal_precedence() {
    for text in ["(/tmp/a(b).rs,.).", "open(/tmp/a(b).rs,.),.!?"] {
        let col = text.find("a(b)").unwrap();
        let candidates = target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true);
        let paths = candidates
            .iter()
            .filter_map(|m| match &m.target {
                DetectedTarget::PathCandidate(path) => Some(path.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, ["/tmp/a(b).rs,.", "/tmp/a(b).rs,", "/tmp/a(b).rs"]);
    }
}

/// Candidate pressure retains the complete three-tier inner group without increasing the per-cell budget.
#[test]
fn wrapped_paths_with_prose_endings_keep_bounded_atomic_groups() {
    let text = "a0 a1 a2 a3 a4 a5 a6 (/tmp/My Long Spaced Path With Seven Parts.rs,.). z0 z1 z2 z3 z4 z5 z6";
    let path = "/tmp/My Long Spaced Path With Seven Parts.rs";
    let col = text.find("Parts").unwrap();
    let candidates = target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true);
    assert!(candidates.len() <= MAX_PATH_CANDIDATES_PER_CELL);
    for suffix in [",.", ",", ""] {
        let expected = format!("{path}{suffix}");
        assert!(
            candidates.iter().any(|m| m.target == DetectedTarget::PathCandidate(expected.clone())),
            "missing {expected:?}"
        );
    }
    let start = text.find("(/tmp").unwrap();
    let source_end = text.find(").").unwrap() + 2;
    let group =
        focused_candidate_group(text, col, PathStyle::Posix, true, 8, start, source_end).unwrap();
    assert_eq!(group.candidates.len(), 3);
}

/// Composing boundary alternatives never repairs malformed locations, quotes, or ambiguous outer syntax.
#[test]
fn wrapped_paths_with_prose_endings_reject_unsafe_boundaries() {
    for text in [
        "(src/main.rs:0).",
        "(src/main.rs:97:0).",
        "(src/main.rs:100-97).",
        "(src/main.rs:99999999999).",
        "(src/main.rs:97:abc).",
        "(src/main.rs:97].",
        "((src/main.rs:97)).",
        "(src/main.rs:97).tail",
        "open(src/main.rs:97)).",
        "\"(src/main.rs:97).\"",
        "(main.rs:97).",
    ] {
        let col = text.find("main").unwrap();
        assert!(
            target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true).is_empty(),
            "{text:?}"
        );
    }
    let text = "(https://example.com/main.rs:97).";
    let candidates = target_candidates_at_char_col_for_style(text, 10, PathStyle::Posix, true);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].target, DetectedTarget::Uri("https://example.com/main.rs:97".into()));
}

/// Existing wrapper trimming remains available through the focused candidate API.
#[test]
fn focused_candidates_preserve_matching_wrapper_support() {
    let text = "(/tmp/My Folder)";
    let matches = target_candidates_at_char_col_for_style(text, 4, PathStyle::Posix, true);
    assert!(matches.iter().any(|matched| {
        &text[matched.start..matched.end] == "/tmp/My Folder"
            && matched.target == DetectedTarget::PathCandidate("/tmp/My Folder".into())
    }));
}

/// URI spans and hard delimiters prevent spaced filesystem reconstruction across provenance.
#[test]
fn spaced_candidates_preserve_uri_and_hard_boundaries() {
    let uri = "https://example.com/a path";
    let found = target_candidates_at_char_col_for_style(uri, 8, PathStyle::Posix, true);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].target, DetectedTarget::Uri("https://example.com/a".into()));

    let tabbed = "/tmp/My\tFolder";
    for col in 0..tabbed.chars().count() {
        assert!(
            target_candidates_at_char_col_for_style(tabbed, col, PathStyle::Posix, true)
                .iter()
                .all(|matched| matched.start < 7 && matched.end <= 7 || matched.start > 7),
            "candidate crossed the tab at {col}: {tabbed:?}"
        );
    }
    for text in ["\"/tmp/My Folder", "/tmp/My Folder\"", r"/tmp/My\ Folder"] {
        for col in 0..text.chars().count() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
                    .is_empty(),
                "quoted or escaped segment exposed a partial target at {col} in {text:?}"
            );
        }
    }
    for text in [
        "\" /tmp/My Folder \"",
        "' /tmp/My Folder '",
        "` /tmp/My Folder `",
        "\" /tmp/My Folder",
        "/tmp/My Folder \"",
        "' /tmp/My Folder",
        "/tmp/My Folder '",
        "` /tmp/My Folder",
        "/tmp/My Folder `",
        "key=\" /tmp/My Folder",
        "key=' /tmp/My Folder",
        "key=` /tmp/My Folder",
    ] {
        for col in 0..text.chars().count() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
                    .is_empty(),
                "padded quoted segment exposed a partial target at {col} in {text:?}"
            );
        }
    }
    for (mixed, path) in [
        (r#""/tmp/My Folder" /tmp/Other Folder"#, "/tmp/Other Folder"),
        ("owners' /tmp/Other Folder", "/tmp/Other Folder"),
        (r#"/tmp/Other Folder "quoted value""#, "/tmp/Other Folder"),
    ] {
        let unquoted = mixed.find(path).unwrap();
        let col = mixed[..unquoted].chars().count() + 5;
        assert!(
            target_candidates_at_char_col_for_style(mixed, col, PathStyle::Posix, true)
                .iter()
                .any(|matched| &mixed[matched.start..matched.end] == path),
            "lexical quote context suppressed an unquoted path in {mixed:?}"
        );
    }
}

/// Candidate enumeration and each reconstructed path stay within their explicit work bounds.
#[test]
fn spaced_candidate_enumeration_is_bounded() {
    let supported = (0..MAX_SPACED_PATH_TOKENS)
        .map(|index| format!("part{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let supported_matches = target_candidates_at_char_col_for_style(
        &supported,
        supported.chars().count() / 2,
        PathStyle::Posix,
        true,
    );
    assert!(supported_matches
        .iter()
        .any(|matched| supported[matched.start..matched.end] == supported));

    let text = (0..128).map(|index| format!("part{index}")).collect::<Vec<_>>().join(" ");
    let middle = text.chars().count() / 2;
    let matches = target_candidates_at_char_col_for_style(&text, middle, PathStyle::Posix, true);
    assert!(matches.len() <= MAX_PATH_CANDIDATES_PER_CELL);
    assert!(matches.iter().all(|matched| matched.end - matched.start <= MAX_TARGET_BYTES));
    assert!(matches.iter().all(|matched| {
        text[matched.start..matched.end].split(' ').count() <= MAX_SPACED_PATH_TOKENS
    }));
}

/// URI-looking text never acquires contextual filesystem provenance.
#[test]
fn contextual_bare_lookup_preserves_uri_precedence() {
    for text in ["https://example.com", "mailto:user@example.com", "file:///tmp/a"] {
        assert!(bare_name_at_char_col_for_style(text, 0, PathStyle::Posix).is_none());
    }
}

/// Return the full-span source reference offered for `col`, if the scanner produced one.
fn source_reference_at(
    text: &str,
    col: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<SourceReference> {
    target_candidates_at_char_col_for_style(text, col, style, include_bare_names)
        .into_iter()
        .find_map(|matched| match matched.target {
            DetectedTarget::SourceReference(reference)
                if matched.start == 0 && matched.end == text.len() =>
            {
                Some(reference)
            }
            _ => None,
        })
}

/// Every pointed cell of a source reference resolves to the same typed location and full span.
#[test]
fn source_reference_suffix_is_typed_at_every_column() {
    // `end_line` carries ranges, `column` carries `:line:column`; the two never both apply.
    for (style, text, path, line, column, end_line) in [
        (PathStyle::Posix, "src/main.rs:12", "src/main.rs", 12, None, None),
        (PathStyle::Posix, "src/main.rs:12:4", "src/main.rs", 12, Some(4), None),
        (PathStyle::Posix, "src/main.rs:12-20", "src/main.rs", 12, None, Some(20)),
        // An en dash is the range separator terminals produce when rendering prose output.
        (PathStyle::Posix, "src/main.rs:12\u{2013}20", "src/main.rs", 12, None, Some(20)),
        // A degenerate range still ascends and stays a valid single-line span.
        (PathStyle::Posix, "/tmp/a.rs:7-7", "/tmp/a.rs", 7, None, Some(7)),
        // Leading zeros are ordinary decimal syntax rather than a malformed suffix.
        (PathStyle::Posix, "./a.rs:012", "./a.rs", 12, None, None),
        (PathStyle::Windows, r"C:\work\main.rs:12", r"C:\work\main.rs", 12, None, None),
        (PathStyle::Windows, r"C:\work\main.rs:12:4", r"C:\work\main.rs", 12, Some(4), None),
    ] {
        for col in 0..text.chars().count() {
            let reference = source_reference_at(text, col, style, true)
                .unwrap_or_else(|| panic!("missing source reference at column {col} in {text:?}"));
            assert_eq!(reference.path, path);
            assert_eq!(reference.display, text, "display keeps the whole pointed span");
            assert_eq!(reference.line, line);
            assert_eq!(reference.column, column);
            assert_eq!(reference.end_line, end_line);
            assert!(reference.explicit_path);
        }
    }
}

/// A numeric-looking but invalid suffix stays inert instead of decaying to a literal filename.
#[test]
fn malformed_source_suffix_never_falls_back_to_literal_path() {
    for text in [
        // Zero, descending ranges, `u32` overflow, and an unparsable extra segment.
        "src/main.rs:0",
        "src/main.rs:12:0",
        "src/main.rs:0:12",
        "src/main.rs:20-12",
        "src/main.rs:20\u{2013}12",
        "src/main.rs:99999999999999",
        "src/main.rs:12:99999999999999",
        "src/main.rs:12-",
        "src/main.rs:12-20-30",
        "src/main.rs:1x",
        "src/main.rs:12:abc",
    ] {
        for col in 0..text.chars().count() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
                    .is_empty(),
                "malformed suffix produced a candidate at column {col} in {text:?}"
            );
        }
    }
}

/// Contextual source references obey the same bare-name switch as contextual paths.
#[test]
fn bare_source_reference_requires_bare_names_enabled() {
    let text = "main.rs:12";
    let reference = source_reference_at(text, 0, PathStyle::Posix, true).expect("bare reference");
    assert_eq!(reference.path, "main.rs");
    assert_eq!(reference.line, 12);
    assert!(!reference.explicit_path, "a bare component is not explicit path syntax");
    // With bare names disabled the same text has no filesystem provenance at all.
    assert!(target_candidates_at_char_col_for_style(text, 0, PathStyle::Posix, false).is_empty());
}

/// Drive letters and alternate-data-stream syntax are not read as location metadata.
#[test]
fn windows_drive_and_stream_colons_are_not_source_metadata() {
    // A lone drive letter keeps its existing reading rather than becoming file `C` line 12.
    for style in [PathStyle::Posix, PathStyle::Windows] {
        assert!(source_reference_at("C:12", 0, style, true).is_none());
    }
    // A drive-rooted path without a suffix stays an ordinary explicit path candidate.
    let matches =
        target_candidates_at_char_col_for_style(r"C:\work\main.rs", 0, PathStyle::Windows, true);
    assert!(matches
        .iter()
        .any(|matched| matched.target == DetectedTarget::PathCandidate(r"C:\work\main.rs".into())));
    // `file.txt:12` is read as a location; Windows stream syntax shares the grammar.
    let reference = source_reference_at("file.txt:12", 0, PathStyle::Windows, true).unwrap();
    assert_eq!((reference.path.as_str(), reference.line), ("file.txt", 12));
}

/// URI provenance still wins over source-reference parsing on the same cell.
#[test]
fn uri_precedence_outranks_source_reference_parsing() {
    let text = "https://example.com/a.rs:12";
    for col in 0..text.chars().count() {
        let matches = target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true);
        assert!(
            matches.iter().all(|matched| matches!(matched.target, DetectedTarget::Uri(_))),
            "non-URI provenance at column {col} in {text:?}: {matches:?}"
        );
    }
}

/// A rooted log-field value owns its path without consuming the field key or neighboring assignments.
#[test]
fn log_field_rooted_values_keep_exact_pointer_ownership() {
    let path = r"C:\Users\dotan\.copilot-cli\1.0.85\copilot.exe";
    for text in [
        format!(
            r#"Copilot CLI version probe timed out path={path} probe="--version" timeout_seconds=5"#
        ),
        format!(r#"path="{path}" mode=ready"#),
        format!("path={path}"),
    ] {
        let start = text.find(path).unwrap();
        for offset in 0..path.len() {
            let matches = target_candidates_at_char_col_for_style(
                &text,
                start + offset,
                PathStyle::Windows,
                true,
            );
            assert!(
                matches.iter().any(|m| m.start == start
                    && m.end == start + path.len()
                    && m.target == DetectedTarget::PathCandidate(path.to_string())),
                "{text}: {matches:?}"
            );
        }
    }
}

/// Quoted fields permit spaces, but incomplete or concatenated values cannot authorize an inner path.
#[test]
fn log_field_quotes_reject_partial_values() {
    let path = r"C:\My Folder\name=value.txt";
    let text = format!("file=\"{path}\" next=done");
    let start = text.find(path).unwrap();
    let found = target_candidates_at_char_col_for_style(&text, start + 4, PathStyle::Windows, true);
    assert!(found.iter().any(|m| m.start == start
        && m.end == start + path.len()
        && m.target == DetectedTarget::PathCandidate(path.into())));
    for malformed in [format!("file=\"{path}"), format!("file=\"{path}\"tail")] {
        let found = target_candidates_at_char_col_for_style(
            &malformed,
            start + 4,
            PathStyle::Windows,
            true,
        );
        assert!(!found.iter().any(|m| m.target == DetectedTarget::PathCandidate(path.into())));
    }
    let text = "key='/tmp/My Folder'";
    let found = target_candidates_at_char_col_for_style(text, 8, PathStyle::Posix, true);
    assert!(found.iter().any(|m| m.start == 5
        && m.end == text.len() - 1
        && m.target == DetectedTarget::PathCandidate("/tmp/My Folder".into())));
    for text in ["key='src/file.rs'", "key='/tmp/file'tail", "prefix'/tmp/file'"] {
        let col = text.find("file").unwrap();
        assert!(
            target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true).is_empty()
        );
    }
    let literal = r"C:\work\name=value.txt";
    assert!(target_candidates_at_char_col_for_style(literal, 15, PathStyle::Windows, true)
        .iter()
        .any(|m| m.target == DetectedTarget::PathCandidate(literal.into())));
}

/// Hyphens in filenames are not list separators, and a short second name keeps its own bare-name provenance.
#[test]
fn punctuation_list_does_not_invent_parent_directories() {
    let text = "src/first.rs、second.rs";
    let second = text.find("second").unwrap();
    let col = text[..second].chars().count();
    let found = target_candidates_at_char_col_for_style(text, col, PathStyle::Windows, true);
    assert!(found
        .iter()
        .any(|m| m.start == second && m.target == DetectedTarget::BareName("second.rs".into())));
    assert!(!found
        .iter()
        .any(|m| m.target == DetectedTarget::PathCandidate("src/second.rs".into())));
    let literal = "src/first.rs-second.rs";
    let found = target_candidates_at_char_col_for_style(literal, 6, PathStyle::Windows, true);
    assert!(found.iter().any(|m| m.target == DetectedTarget::PathCandidate(literal.into())));
    assert!(!found
        .iter()
        .any(|m| m.target == DetectedTarget::PathCandidate("src/first.rs".into())));
}

/// Independently enumerated spaced members carry the same literal guard even under candidate-budget pressure.
#[test]
fn list_guards_survive_spaced_candidates_and_caps() {
    for prefix in ["", "one two three four five six seven "] {
        let text = format!("{prefix}src/a.rs、 b.rs c d e f g h");
        let start = text.find("b.rs").unwrap();
        let col = text[..start].chars().count();
        let found = target_candidates_at_char_col_for_style(&text, col, PathStyle::Windows, true);
        let members = found
            .iter()
            .filter(|m| m.target == DetectedTarget::BareName("b.rs".into()))
            .collect::<Vec<_>>();
        assert!(!members.is_empty());
        assert!(members.iter().all(|m| m.missing_before.iter().any(|literal|
            matches!(literal, DetectedTarget::PathCandidate(value) if value.contains("src/a.rs、 b.rs")))));
    }
}

/// List-member recognition must retain the complete literal candidate before any shorter filesystem alternative.
#[test]
fn punctuation_list_retains_literal_and_focused_member() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for separator in ["、", ",", "，", ";"] {
            let path = "crates/sonicterm-cfg/src/url_scan.rs";
            let text = format!("{path}{separator}url_scan_tests.rs");
            let found = target_candidates_at_char_col_for_style(&text, 3, style, true);
            assert!(found.iter().any(|m| m.target == DetectedTarget::PathCandidate(text.clone())));
            assert!(
                found.iter().any(|m| m.start == 0
                    && m.end == path.len()
                    && m.target == DetectedTarget::PathCandidate(path.into())),
                "{found:?}"
            );
        }
    }
}

/// Complete structures keep exact destinations under independent changes to outer prose and punctuation.
#[test]
fn structural_boundaries_preserve_destinations_and_pointer_ownership() {
    let mut failures = Vec::new();
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for path in ["reports/flight.html", "reports/flight.md", "./notes", "./My Folder/a(b).rs"] {
            for suffix in ["", ":12", ":12:4", ":12–20"] {
                let display = format!("{path}{suffix}");
                for (left, right) in [
                    ("(", ")"),
                    ("[", "]"),
                    ("{", "}"),
                    ("Read(", ")"),
                    ("'", "'"),
                    ("\"", "\""),
                    ("`", "`"),
                    ("（", "）"),
                    ("【", "】"),
                    ("《", "》"),
                    ("「", "」"),
                    ("『", "』"),
                    ("“", "”"),
                    ("‘", "’"),
                    ("«", "»"),
                ] {
                    for tail in [
                        "",
                        ", next",
                        ",next",
                        "，内容与",
                        "。",
                        "；后文",
                        "：说明",
                        "！",
                        "？",
                        "、",
                        "…",
                        "——",
                        "،text",
                        "，「引用」",
                    ] {
                        let prefix = format!("已检查 {left}");
                        let text = format!("{prefix}{display}{right}{tail}");
                        let start = prefix.len();
                        let end = start + display.len();
                        for (col, (byte, _)) in text.char_indices().enumerate() {
                            let found =
                                target_candidates_at_char_col_for_style(&text, col, style, true);
                            let exact = found.iter().any(|m| {
                                m.start == start
                                    && m.end == end
                                    && match &m.target {
                                        DetectedTarget::PathCandidate(p) => {
                                            suffix.is_empty() && p == path
                                        }
                                        DetectedTarget::SourceReference(r) => {
                                            !suffix.is_empty()
                                                && r.path == path
                                                && r.display == display
                                                && r.line == 12
                                                && r.column == (suffix == ":12:4").then_some(4)
                                                && r.end_line == (suffix == ":12–20").then_some(20)
                                        }
                                        _ => false,
                                    }
                            });
                            assert!(found.len() <= MAX_PATH_CANDIDATES_PER_CELL);
                            if exact != (start..end).contains(&byte) {
                                if failures.len() < 8 {
                                    failures
                                        .push(format!("{style:?} {text:?} at {col}: {found:?}"));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Adjacent Chinese prose keeps both home paths literal-first and never owns their shorter target spans.
#[test]
fn prose_boundary_exact_sentence_keeps_home_paths_guarded() {
    let text = "同步时会将客户端设置写入 ~/.claude/settings.json，将 MCP 配置块写入顶层的 ~/.claude.json，并将权限";
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for include_bare_names in [false, true] {
            for (path, literal) in [
                ("~/.claude/settings.json", "~/.claude/settings.json，将"),
                ("~/.claude.json", "~/.claude.json，并将权限"),
            ] {
                let start = text.find(literal).unwrap();
                let end = start + path.len();
                let source_end = start + literal.len();
                for (col, (byte, _)) in text.char_indices().enumerate() {
                    let found = target_candidates_at_char_col_for_style(
                        text,
                        col,
                        style,
                        include_bare_names,
                    );
                    let short = found
                        .iter()
                        .filter(|m| m.target == DetectedTarget::PathCandidate(path.into()))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        !short.is_empty(),
                        (start..end).contains(&byte),
                        "{style:?} bare={include_bare_names} {path} at {col}: {found:?}"
                    );
                    if (start..source_end).contains(&byte) {
                        assert!(found.iter().any(|m| {
                            m.start == start
                                && m.end == source_end
                                && m.target == DetectedTarget::PathCandidate(literal.into())
                        }));
                    }
                    for matched in short {
                        assert_eq!(
                            (matched.start, matched.end, matched.source_start, matched.source_end),
                            (start, end, start, source_end)
                        );
                        assert_eq!(
                            matched.missing_before,
                            vec![DetectedTarget::PathCandidate(literal.into())]
                        );
                    }
                    assert!(found.len() <= MAX_PATH_CANDIDATES_PER_CELL);
                }
            }
        }
    }
}

/// Explicit Unicode paths and extensionless dotfiles use the same guarded boundary without an extension heuristic.
#[test]
fn prose_boundary_supports_explicit_unicode_paths_and_dotfiles() {
    for (style, paths) in [
        (
            PathStyle::Posix,
            vec!["~/.claude", "~/目录/文件", "/目录/文件", "./目录/文件", "../目录/文件"],
        ),
        (
            PathStyle::Windows,
            vec![
                r"~\.claude",
                "~/目录/文件",
                r"~\目录\文件",
                r"C:\目录\文件",
                r".\目录\文件",
                r"..\目录\文件",
            ],
        ),
    ] {
        for path in paths {
            for tail in ["，后文", "。后文", "；正文", "、说明"] {
                let text = format!("{path}{tail}");
                for (col, (byte, _)) in text.char_indices().enumerate() {
                    let found = target_candidates_at_char_col_for_style(&text, col, style, false);
                    let short = found
                        .iter()
                        .filter(|m| m.target == DetectedTarget::PathCandidate(path.into()))
                        .collect::<Vec<_>>();
                    assert_eq!(!short.is_empty(), byte < path.len(), "{text} at {col}: {found:?}");
                    assert!(found.iter().any(|m| {
                        m.start == 0
                            && m.end == text.len()
                            && m.target == DetectedTarget::PathCandidate(text.clone())
                    }));
                    for matched in short {
                        assert_eq!(
                            (matched.start, matched.end, matched.source_start, matched.source_end),
                            (0, path.len(), 0, text.len())
                        );
                        assert_eq!(
                            matched.missing_before,
                            vec![DetectedTarget::PathCandidate(text.clone())]
                        );
                    }
                }
            }
        }
    }
}

/// A Unicode prose separator permits a guarded prefix; adjacent letters or opening/closing punctuation do not.
#[test]
fn prose_boundary_requires_other_punctuation_after_unicode_path() {
    let path = "~/文档/notes.md";
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for (tail, boundary) in [("后文", false), ("（说明）", false), ("，见", true)] {
            let text = format!("{path}{tail}");
            for (col, (byte, _)) in text.char_indices().enumerate() {
                let found = target_candidates_at_char_col_for_style(&text, col, style, false);
                let short = found
                    .iter()
                    .filter(|m| m.target == DetectedTarget::PathCandidate(path.into()))
                    .collect::<Vec<_>>();
                assert_eq!(
                    !short.is_empty(),
                    boundary && byte < path.len(),
                    "{style:?} {text} at {col}: {found:?}"
                );
                assert!(found.iter().any(|m| {
                    m.start == 0
                        && m.end == text.len()
                        && m.target == DetectedTarget::PathCandidate(text.clone())
                }));
                for matched in short {
                    assert_eq!(
                        (matched.start, matched.end, matched.source_start, matched.source_end),
                        (0, path.len(), 0, text.len())
                    );
                    assert_eq!(
                        matched.missing_before,
                        vec![DetectedTarget::PathCandidate(text.clone())]
                    );
                }
            }
        }
    }
}

/// Candidate pressure cannot drop the home literal or attach CWD-dependent neighboring prose to its guard.
#[test]
fn prose_boundary_keeps_literal_and_guard_with_surrounding_words() {
    let path = "~/.claude";
    let literal = "~/.claude，正文";
    let text = format!("one two three four five six seven eight {literal} nine ten eleven twelve thirteen fourteen fifteen sixteen");
    let start = text.find(literal).unwrap();
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for offset in 0..path.len() {
            let found = target_candidates_at_char_col_for_style(&text, start + offset, style, true);
            assert!(found.len() <= MAX_PATH_CANDIDATES_PER_CELL);
            assert!(found.iter().any(|m| {
                m.start == start
                    && m.end == start + literal.len()
                    && m.target == DetectedTarget::PathCandidate(literal.into())
            }));
            let short = found
                .iter()
                .filter(|m| m.target == DetectedTarget::PathCandidate(path.into()))
                .collect::<Vec<_>>();
            assert!(!short.is_empty(), "{style:?} at {offset}: {found:?}");
            for matched in short {
                assert_eq!(
                    (matched.start, matched.end, matched.source_start, matched.source_end),
                    (start, start + path.len(), start, start + literal.len())
                );
                assert_eq!(
                    matched.missing_before,
                    vec![DetectedTarget::PathCandidate(literal.into())]
                );
            }
        }
    }
}

/// Prose fallback does not reinterpret contextual names or extend its single-token scope into spaced paths.
#[test]
fn prose_boundary_preserves_contextual_and_spaced_literals() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for path in
            ["src/name", "src/name.rs", "name", "~/My Folder/settings.json", "./My Folder/.config"]
        {
            let text = format!("{path}，正文");
            for (col, _) in text.char_indices().enumerate() {
                let found = target_candidates_at_char_col_for_style(&text, col, style, true);
                assert!(found.iter().any(|m| {
                    m.start == 0
                        && m.end == text.len()
                        && matches!(&m.target, DetectedTarget::PathCandidate(value) | DetectedTarget::BareName(value) if value == &text)
                }), "{style:?} {text} at {col}: {found:?}");
                assert!(!found.iter().any(|m| {
                    matches!(&m.target, DetectedTarget::PathCandidate(value) | DetectedTarget::BareName(value) if value == path)
                }), "{style:?} {text} at {col}: {found:?}");
            }
        }
    }
}

/// Grouped references use their matched closer rather than consuming neighboring prose or references.
#[test]
fn structural_boundaries_keep_grouped_locations_and_neighbors() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for (left, right) in [("(", ")"), ("【", "】"), ("“", "”")] {
            let group = "src/main.rs:12, :15:4, :20–25";
            let text = format!("说明 {left}{group}{right}，内容与 (src/other.rs:7)。");
            let start = text.find(group).unwrap();
            for (needle, line, column, end_line) in [
                ("src/main", 12, None, None),
                (":15", 15, Some(4), None),
                (":20", 20, None, Some(25)),
            ] {
                let col = text[..text.find(needle).unwrap()].chars().count();
                let found = target_candidates_at_char_col_for_style(&text, col, style, true);
                assert!(found.iter().any(|m| m.start == start && m.end == start + group.len()
                    && matches!(&m.target, DetectedTarget::SourceReference(r) if r.path == "src/main.rs"
                        && r.line == line && r.column == column && r.end_line == end_line)), "{text} {needle}: {found:?}");
            }
            let other = text.find("src/other").unwrap();
            let found = target_candidates_at_char_col_for_style(
                &text,
                text[..other].chars().count(),
                style,
                true,
            );
            assert!(found.iter().any(|m| matches!(&m.target, DetectedTarget::SourceReference(r) if r.path == "src/other.rs" && r.line == 7)));
            for (col, (byte, ch)) in text.char_indices().enumerate() {
                if (start..start + group.len()).contains(&byte) && matches!(ch, ',' | ' ') {
                    assert!(
                        target_candidates_at_char_col_for_style(&text, col, style, true).is_empty(),
                        "separator: {text} {col}"
                    );
                }
            }
        }
    }
}

/// Invalid structures and filename continuations never recover an actionable inner prefix.
#[test]
fn structural_boundaries_reject_repairs_and_continuations() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for text in [
            "(src/main.rs).bak",
            "(src/main.rs)/child",
            "(src/main.rs)\\child",
            "(src/main.rs)tail",
            "Read(src/main.rs)(extra)",
            "((src/main.rs))",
            "（src/main.rs】",
            "【src/main.rs)",
            "“src/main.rs’",
            "(src/main.rs",
            "prefix'src/main.rs'",
            "key='src/main.rs'",
            "'src/main.rs'suffix",
            "(src/main.rs:0)，",
            "(src/main.rs:12:abc)，",
            "(src/main.rs:12, :0)，",
            "/tmp/My (src/main.rs:12, :15)，",
            "C:\\My [src/main.rs:12, :15]，",
        ] {
            let col = text[..text.find("main.rs").unwrap()].chars().count();
            let found = target_candidates_at_char_col_for_style(text, col, style, true);
            assert!(
                !found.iter().any(|m| match &m.target {
                    DetectedTarget::PathCandidate(p) => p == "src/main.rs",
                    DetectedTarget::SourceReference(r) => r.path == "src/main.rs",
                    _ => false,
                }),
                "unsafe prefix: {text}: {found:?}"
            );
        }
    }
}

/// Punctuation inside an explicit structure is literal filename data, not the outer separator.
#[test]
fn structural_boundaries_preserve_literal_punctuation() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for path in
            ["src/a，b.html", "src/a。", "src/a…b.html", "./a(b).html", "./My Folder/a.txt,"]
        {
            let text = format!("({path})，后文");
            let found = target_candidates_at_char_col_for_style(&text, 3, style, true);
            assert!(
                found.iter().any(|m| m.target == DetectedTarget::PathCandidate(path.into())),
                "{text}: {found:?}"
            );
        }
    }
}

/// Source ranges include complete wide boundaries, not following prose; generated delimiter mutations cannot repair a path.
#[test]
fn structural_boundaries_carry_source_ranges_and_reject_mutations() {
    for (left, right) in [("(", ")"), ("【", "】"), ("“", "”"), ("«", "»")] {
        for path in ["src/main.rs", "./My Folder/file.md"] {
            let text = format!("前文 {left}{path}{right}，正文");
            let start = text.find(path).unwrap();
            let source_start = "前文 ".len();
            let source_end = text.find("正文").unwrap();
            let found = target_candidates_at_char_col_for_style(
                &text,
                text[..start].chars().count(),
                PathStyle::Windows,
                true,
            );
            let exact = found
                .iter()
                .find(|m| m.target == DetectedTarget::PathCandidate(path.into()))
                .unwrap();
            assert_eq!(
                (exact.start, exact.end, exact.source_start, exact.source_end),
                (start, start + path.len(), source_start, source_end)
            );
            for bad in ["", "]", "'", "\u{1b}"] {
                if bad == right {
                    continue;
                }
                let text = format!("{left}{path}{bad}，正文");
                let col = left.chars().count() + 2;
                assert!(
                    !target_candidates_at_char_col_for_style(&text, col, PathStyle::Windows, true)
                        .iter()
                        .any(|m| m.target == DetectedTarget::PathCandidate(path.into())),
                    "{text:?}"
                );
            }
        }
    }
}

/// Boundaries retain byte, nesting, token, and candidate budgets without accepting partial over-limit text.
#[test]
fn structural_boundaries_keep_limits_and_literal_priority() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for extra in [0, 1] {
            let path = format!("./{}", "x".repeat(MAX_TARGET_BYTES - 2 + extra));
            let text = format!("【{path}】，后文");
            let found = target_candidates_at_char_col_for_style(&text, 3, style, true);
            assert_eq!(
                found.iter().any(|m| m.target == DetectedTarget::PathCandidate(path.clone())),
                extra == 0
            );
        }
        let path = "./file.txt,";
        let text = format!("({path})，后文");
        let found = target_candidates_at_char_col_for_style(&text, 3, style, true);
        assert_eq!(found[0].target, DetectedTarget::PathCandidate(path.into()));
        assert!(found
            .iter()
            .any(|m| m.target == DetectedTarget::PathCandidate("./file.txt".into())));
        for depth in [9, 100] {
            let text = format!("{}src/file.rs{}", "(".repeat(depth), ")".repeat(depth));
            let found = target_candidates_at_char_col_for_style(&text, depth + 2, style, true);
            assert!(found.len() <= MAX_PATH_CANDIDATES_PER_CELL);
            assert!(!found
                .iter()
                .any(|m| m.target == DetectedTarget::PathCandidate("src/file.rs".into())));
        }
    }
}

/// An earlier relative filename in prose does not acquire ownership of a later independently wrapped target.
#[test]
fn structural_boundaries_do_not_capture_prior_relative_prose_paths() {
    for style in [PathStyle::Windows, PathStyle::Posix] {
        for text in [
            "see src/lib.rs and (src/main.rs)",
            "src/lib.rs and 【src/main.rs】，后文",
            "compare src/first.rs with 'src/main.rs'",
            "9:30 (src/main.rs)",
            "1: item (src/main.rs)",
            "A: item (src/main.rs)",
        ] {
            let start = text.find("src/main.rs").unwrap();
            for offset in 0.."src/main.rs".len() {
                let col = text[..start + offset].chars().count();
                let found = target_candidates_at_char_col_for_style(text, col, style, true);
                assert!(
                    found.iter().any(|m| m.start == start
                        && m.end == start + "src/main.rs".len()
                        && m.target == DetectedTarget::PathCandidate("src/main.rs".into())),
                    "{text}: {found:?}"
                );
            }
        }
    }
}

/// URI precedence preserves the surrounding source range used by grid-level boundary identity checks.
#[test]
fn structural_uri_source_includes_wrappers_and_trimmed_punctuation() {
    for text in ["(https://example.com/a).", "[file:///C:/work/a.txt],"] {
        let url = find_urls(text).remove(0);
        let found =
            target_candidates_at_char_col_for_style(text, url.start + 5, PathStyle::Windows, true);
        assert_eq!(found[0].source_start, 0);
        assert_eq!(found[0].source_end, text.len());
        assert_eq!(found[0].target, DetectedTarget::Uri(url.url));
    }
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
