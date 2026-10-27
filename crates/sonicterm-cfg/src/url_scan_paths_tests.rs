//! Regression contracts for paths target detection.

use super::*;

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
        posix.iter().map(|candidate| &candidate.target).collect::<Vec<_>>(),
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
        windows.iter().map(|candidate| &candidate.target).collect::<Vec<_>>(),
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
        targets.iter().map(|candidate| &candidate.target).collect::<Vec<_>>(),
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
        targets
            .iter()
            .map(|candidate| (&text[candidate.start..candidate.end], &candidate.target))
            .collect::<Vec<_>>(),
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
