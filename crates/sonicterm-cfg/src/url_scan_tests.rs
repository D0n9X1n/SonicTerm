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
            DetectedTarget::Uri(_) | DetectedTarget::BareName(_) => true,
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
    for text in [
        "\"/tmp/My Folder\"",
        "'/tmp/My Folder'",
        "\"/tmp/My Folder",
        "/tmp/My Folder\"",
        r"/tmp/My\ Folder",
    ] {
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
