//! Regression contracts for structure target detection.

use super::*;

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
                    let exact = matches.iter().any(|candidate| {
                        candidate.start == start
                            && candidate.end == end
                            && candidate.target == DetectedTarget::PathCandidate(path.into())
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
            assert!(found.iter().any(|candidate| matches!(&candidate.target,
                DetectedTarget::SourceReference(reference) if reference.path == "src/My Folder/café.rs"
                    && reference.line == 12 && reference.column == Some(4))));
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
                        found.iter().any(|candidate| candidate.start == start
                            && candidate.end == start + body.len()
                            && matches!(&candidate.target, DetectedTarget::SourceReference(reference)
                            if reference.path == path && reference.display == body && reference.line == line
                                && reference.column == column && reference.end_line == end_line)),
                        "{text:?} at {needle}"
                    );
                }
                for (col, (_, character)) in text.char_indices().enumerate() {
                    if character == ',' || character == ' ' {
                        assert!(!target_candidates_at_char_col_for_style(&text, col, style, true)
                            .iter().any(|candidate| matches!(&candidate.target, DetectedTarget::SourceReference(reference) if reference.path == path)));
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
                .any(|candidate| candidate.target
                    == DetectedTarget::PathCandidate("/tmp/other.rs".into())),
            "{text}"
        );
    }
    let text = "(src/a.rs:12, :14) [src/b.rs:20, :24].";
    let col = text.find(":24").unwrap();
    assert!(target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
        .iter().any(|candidate| matches!(&candidate.target, DetectedTarget::SourceReference(reference) if reference.path == "src/b.rs" && reference.line == 24)));
    let text = "(src/a.rs:12,   :14)";
    assert!(target_candidates_at_char_col_for_style(text, 2, PathStyle::Posix, true)
        .iter()
        .any(|candidate| matches!(&candidate.target, DetectedTarget::SourceReference(reference) if reference.path == "src/a.rs")));
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
            matches!(&found[0].target, DetectedTarget::SourceReference(reference) if reference.path == "src/b.rs" && reference.line == 4),
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
        assert!(
            matches!(&found[0].target, DetectedTarget::SourceReference(reference) if reference.line == 8)
        );
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
                            let exact = found.iter().any(|candidate| {
                                candidate.start == start
                                    && candidate.end == end
                                    && match &candidate.target {
                                        DetectedTarget::PathCandidate(candidate_path) => {
                                            suffix.is_empty() && candidate_path == path
                                        }
                                        DetectedTarget::SourceReference(reference) => {
                                            !suffix.is_empty()
                                                && reference.path == path
                                                && reference.display == display
                                                && reference.line == 12
                                                && reference.column
                                                    == (suffix == ":12:4").then_some(4)
                                                && reference.end_line
                                                    == (suffix == ":12–20").then_some(20)
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
                assert!(found.iter().any(|candidate| candidate.start == start && candidate.end == start + group.len()
                    && matches!(&candidate.target, DetectedTarget::SourceReference(reference) if reference.path == "src/main.rs"
                        && reference.line == line && reference.column == column && reference.end_line == end_line)), "{text} {needle}: {found:?}");
            }
            let other = text.find("src/other").unwrap();
            let found = target_candidates_at_char_col_for_style(
                &text,
                text[..other].chars().count(),
                style,
                true,
            );
            assert!(found.iter().any(|candidate| matches!(&candidate.target, DetectedTarget::SourceReference(reference) if reference.path == "src/other.rs" && reference.line == 7)));
            for (col, (byte, character)) in text.char_indices().enumerate() {
                if (start..start + group.len()).contains(&byte) && matches!(character, ',' | ' ') {
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
                !found.iter().any(|candidate| match &candidate.target {
                    DetectedTarget::PathCandidate(candidate_path) =>
                        candidate_path == "src/main.rs",
                    DetectedTarget::SourceReference(reference) => reference.path == "src/main.rs",
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
                found.iter().any(|candidate| candidate.target == DetectedTarget::PathCandidate(path.into())),
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
                .find(|candidate| candidate.target == DetectedTarget::PathCandidate(path.into()))
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
                        .any(|candidate| candidate.target
                            == DetectedTarget::PathCandidate(path.into())),
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
                found.iter().any(
                    |candidate| candidate.target == DetectedTarget::PathCandidate(path.clone())
                ),
                extra == 0
            );
        }
        let path = "./file.txt,";
        let text = format!("({path})，后文");
        let found = target_candidates_at_char_col_for_style(&text, 3, style, true);
        assert_eq!(found[0].target, DetectedTarget::PathCandidate(path.into()));
        assert!(found.iter().any(
            |candidate| candidate.target == DetectedTarget::PathCandidate("./file.txt".into())
        ));
        for depth in [9, 100] {
            let text = format!("{}src/file.rs{}", "(".repeat(depth), ")".repeat(depth));
            let found = target_candidates_at_char_col_for_style(&text, depth + 2, style, true);
            assert!(found.len() <= MAX_PATH_CANDIDATES_PER_CELL);
            assert!(!found
                .iter()
                .any(|candidate| candidate.target
                    == DetectedTarget::PathCandidate("src/file.rs".into())));
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
                    found.iter().any(|candidate| candidate.start == start
                        && candidate.end == start + "src/main.rs".len()
                        && candidate.target == DetectedTarget::PathCandidate("src/main.rs".into())),
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

/// The structural parse visits bytes linearly in the line's length, with a bounded multiplier,
/// on inputs built from repeated pairs: doubling the line at most doubles the work. A rescan of
/// the prefix from the floor at every pair made this quadratic.
#[test]
fn structural_parse_work_grows_linearly_with_the_line() {
    for unit in ["()x ", "a() ", "()", "(a)b", "x(", "(", "()()x(", "[a]", "/a (b) "] {
        let work = |repeats: usize| {
            let text = unit.repeat(repeats);
            let last_col = text.chars().count() - 1;
            super::structure_work::measure(|| {
                let _ = target_candidates_at_char_col_for_style(
                    &text,
                    last_col,
                    PathStyle::Posix,
                    true,
                );
            })
        };
        let (single, double) = (work(1_000), work(2_000));
        // Linear work doubles plus a constant; a quadratic scan would about quadruple.
        assert!(
            double * 100 <= single * 205,
            "{unit:?}: {single} bytes, then {double} at twice the line"
        );
    }
}

/// The incremental prefix check equals the rescan it replaced at every structure: candidates over
/// a deterministic corpus of short lines, at every column and for both grammars, run with each
/// structure's prefix checked against the reference.
#[test]
fn incremental_prefix_check_matches_the_rescan_over_a_corpus() {
    const ALPHABET: [char; 16] =
        ['a', '/', '(', ')', '[', ']', ' ', 'x', '1', '_', '.', '~', '\t', ':', 'C', '\u{e9}'];
    let mut seed = 0x5eed_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    for _ in 0..1_500 {
        let length = 1 + next() % 24;
        let text = (0..length).map(|_| ALPHABET[next() % ALPHABET.len()]).collect::<String>();
        for col in 0..text.chars().count() {
            for style in [PathStyle::Posix, PathStyle::Windows] {
                let _ = target_candidates_at_char_col_for_style(&text, col, style, true);
            }
        }
    }
}
