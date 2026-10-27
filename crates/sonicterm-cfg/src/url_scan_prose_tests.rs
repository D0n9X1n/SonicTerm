//! Regression contracts for prose target detection.

use super::*;

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
                        let exact = candidates.iter().any(|candidate| {
                            candidate.start == start
                                && candidate.end == end
                                && candidate.target == expected
                        });
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
            .filter_map(|candidate| match &candidate.target {
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
            candidates.iter().any(
                |candidate| candidate.target == DetectedTarget::PathCandidate(expected.clone())
            ),
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
                matches.iter().any(|candidate| candidate.start == start
                    && candidate.end == start + path.len()
                    && candidate.target == DetectedTarget::PathCandidate(path.to_string())),
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
    assert!(found.iter().any(|candidate| candidate.start == start
        && candidate.end == start + path.len()
        && candidate.target == DetectedTarget::PathCandidate(path.into())));
    for malformed in [format!("file=\"{path}"), format!("file=\"{path}\"tail")] {
        let found = target_candidates_at_char_col_for_style(
            &malformed,
            start + 4,
            PathStyle::Windows,
            true,
        );
        assert!(!found
            .iter()
            .any(|candidate| candidate.target == DetectedTarget::PathCandidate(path.into())));
    }
    let text = "key='/tmp/My Folder'";
    let found = target_candidates_at_char_col_for_style(text, 8, PathStyle::Posix, true);
    assert!(found.iter().any(|candidate| candidate.start == 5
        && candidate.end == text.len() - 1
        && candidate.target == DetectedTarget::PathCandidate("/tmp/My Folder".into())));
    for text in ["key='src/file.rs'", "key='/tmp/file'tail", "prefix'/tmp/file'"] {
        let col = text.find("file").unwrap();
        assert!(
            target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true).is_empty()
        );
    }
    let literal = r"C:\work\name=value.txt";
    assert!(target_candidates_at_char_col_for_style(literal, 15, PathStyle::Windows, true)
        .iter()
        .any(|candidate| candidate.target == DetectedTarget::PathCandidate(literal.into())));
}

/// Hyphens in filenames are not list separators, and a short second name keeps its own bare-name provenance.
#[test]
fn punctuation_list_does_not_invent_parent_directories() {
    let text = "src/first.rs、second.rs";
    let second = text.find("second").unwrap();
    let col = text[..second].chars().count();
    let found = target_candidates_at_char_col_for_style(text, col, PathStyle::Windows, true);
    assert!(found.iter().any(|candidate| candidate.start == second
        && candidate.target == DetectedTarget::BareName("second.rs".into())));
    assert!(
        !found
            .iter()
            .any(|candidate| candidate.target
                == DetectedTarget::PathCandidate("src/second.rs".into()))
    );
    let literal = "src/first.rs-second.rs";
    let found = target_candidates_at_char_col_for_style(literal, 6, PathStyle::Windows, true);
    assert!(found
        .iter()
        .any(|candidate| candidate.target == DetectedTarget::PathCandidate(literal.into())));
    assert!(!found
        .iter()
        .any(|candidate| candidate.target == DetectedTarget::PathCandidate("src/first.rs".into())));
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
            .filter(|candidate| candidate.target == DetectedTarget::BareName("b.rs".into()))
            .collect::<Vec<_>>();
        assert!(!members.is_empty());
        assert!(members.iter().all(|candidate| candidate.missing_before.iter().any(|literal|
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
            assert!(found
                .iter()
                .any(|candidate| candidate.target == DetectedTarget::PathCandidate(text.clone())));
            assert!(
                found.iter().any(|candidate| candidate.start == 0
                    && candidate.end == path.len()
                    && candidate.target == DetectedTarget::PathCandidate(path.into())),
                "{found:?}"
            );
        }
    }
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
                        .filter(|candidate| {
                            candidate.target == DetectedTarget::PathCandidate(path.into())
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        !short.is_empty(),
                        (start..end).contains(&byte),
                        "{style:?} bare={include_bare_names} {path} at {col}: {found:?}"
                    );
                    if (start..source_end).contains(&byte) {
                        assert!(found.iter().any(|candidate| {
                            candidate.start == start
                                && candidate.end == source_end
                                && candidate.target == DetectedTarget::PathCandidate(literal.into())
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
                        .filter(|candidate| {
                            candidate.target == DetectedTarget::PathCandidate(path.into())
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(!short.is_empty(), byte < path.len(), "{text} at {col}: {found:?}");
                    assert!(found.iter().any(|candidate| {
                        candidate.start == 0
                            && candidate.end == text.len()
                            && candidate.target == DetectedTarget::PathCandidate(text.clone())
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
                    .filter(|candidate| {
                        candidate.target == DetectedTarget::PathCandidate(path.into())
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    !short.is_empty(),
                    boundary && byte < path.len(),
                    "{style:?} {text} at {col}: {found:?}"
                );
                assert!(found.iter().any(|candidate| {
                    candidate.start == 0
                        && candidate.end == text.len()
                        && candidate.target == DetectedTarget::PathCandidate(text.clone())
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
            assert!(found.iter().any(|candidate| {
                candidate.start == start
                    && candidate.end == start + literal.len()
                    && candidate.target == DetectedTarget::PathCandidate(literal.into())
            }));
            let short = found
                .iter()
                .filter(|candidate| candidate.target == DetectedTarget::PathCandidate(path.into()))
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
                assert!(found.iter().any(|candidate| {
                    candidate.start == 0
                        && candidate.end == text.len()
                        && matches!(&candidate.target, DetectedTarget::PathCandidate(value) | DetectedTarget::BareName(value) if value == &text)
                }), "{style:?} {text} at {col}: {found:?}");
                assert!(!found.iter().any(|candidate| {
                    matches!(&candidate.target, DetectedTarget::PathCandidate(value) | DetectedTarget::BareName(value) if value == path)
                }), "{style:?} {text} at {col}: {found:?}");
            }
        }
    }
}
