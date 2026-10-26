//! Preserved activate path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

/// Plain and wrapped text resolve the same real file, preserving pane-local spans and copy boundaries.
#[test]
fn tool_and_prose_paths_resolve_real_files_in_each_window() {
    let root = native_test_root().join(format!(
        "sonicterm-path-text-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let paths = [
        "crates/sonicterm-app/src/app/mod_tests.rs",
        ".github/scripts/validate_release.py",
        "tests/test_release_validation.py",
        "fixtures/foo and bar.py",
        "fixtures/and",
    ];
    for path in paths {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "harmless fixture").unwrap();
    }
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main path boundaries");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["path boundaries"]);
    // File URIs use a slash-prefixed drive path; the reveal URI encoder only accepts POSIX-shaped input.
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", root.to_str().unwrap().replace('\\', "/")))
    } else {
        root.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    for window in [main, child] {
        let pane = *app.windows[&window].panes.keys().next().unwrap();
        app.windows[&window].panes[&pane].parser.lock().resize(120, 24);
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        for (row, path) in [
            (paths[0].to_owned(), paths[0]),
            (format!("⏺ Update({})", paths[0]), paths[0]),
            (format!("Read({})", paths[0]), paths[0]),
            (format!("Write({})", paths[0]), paths[0]),
            (format!("{} and focused {}. Require stable", paths[1], paths[2]), paths[1]),
            (format!("{} and focused {}. Require stable", paths[1], paths[2]), paths[2]),
            (paths[3].to_owned(), paths[3]),
            (paths[4].to_owned(), paths[4]),
        ] {
            let output = format!("\x1b[2J\x1b[H\x1b]7;{cwd}\x1b\\{row}");
            assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
            let start_byte = row.find(path).unwrap();
            let start_col = row[..start_byte].chars().count() as u16;
            for col in start_col..start_col + path.len() as u16 {
                let target = app.cell_target_at(window, pane, 0, col).unwrap();
                let ResolvedCellTarget::Path(key) = target.target else {
                    panic!("filesystem target")
                };
                let selection = probe_candidates(&key.candidates, classify_local_target).unwrap();
                assert_eq!(selection.candidate.resolved_path, root.join(path), "{row}: {col}");
                assert_eq!(
                    selection.candidate.spans.as_slice(),
                    &[AbsoluteCellSpan {
                        row: 0,
                        start_col,
                        end_col: start_col + path.len() as u16,
                    }]
                );
                assert_eq!(
                    local_target_action(selection.decision),
                    Some(LocalTargetAction::Reveal)
                );
            }
        }
        let missing = "crates/sonicterm-app/src/app/missing.rs";
        let output = format!("\x1b[2J\x1b[HUpdate({missing})");
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        app.test_clipboard_text = Some("preserved".into());
        let target = app.cell_target_at(window, pane, 0, 10).unwrap();
        assert_eq!(target.explicit_path_text().as_deref(), Some(missing));
        let ResolvedCellTarget::Path(key) = target.target else { panic!("filesystem target") };
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result =
            PathProbeResult { request, selection: None, failure: Some("path-error-missing") };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        // A path that names no file is an ordinary click: no toast, no copy on a repeated click.
        for _ in 0..2 {
            assert!(!app.activate_target_at(window, pane, 0, 10));
            assert_eq!(app.test_clipboard_text.as_deref(), Some("preserved"));
            assert!(app.windows[&window].notification.is_none());
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Punctuated wrappers preserve source metadata and real-file reveal spans in main and child windows.
#[test]
fn punctuated_wrapped_paths_resolve_real_files_in_each_window() {
    let root = native_test_root().join(format!(
        "sonicterm-punctuated-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let path = "src/café.rs";
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(path), "harmless fixture").unwrap();
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", root.to_str().unwrap().replace('\\', "/")))
    } else {
        root.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main punctuation");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child punctuation"]);
    for window in [main, child] {
        let pane = *app.windows[&window].panes.keys().next().unwrap();
        app.windows[&window].panes[&pane].parser.lock().resize(120, 24);
        for suffix in ["", ":97", ":97:4", ":97–100"] {
            let display = format!("{path}{suffix}");
            for (left, right) in [("(", ")"), ("[", "]"), ("{", "}"), ("Read(", ")")] {
                let row = format!("é {left}{display}{right},.!? Next");
                let output = format!("\x1b[2J\x1b[H\x1b]7;{cwd}\x1b\\{row}");
                assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
                let start_col = 2 + left.len() as u16;
                let end_col = start_col + display.chars().count() as u16;
                for col in start_col..end_col {
                    let snapshot =
                        app.cell_target_at(window, pane, 0, col).expect("wrapped candidate");
                    let ResolvedCellTarget::Path(key) = snapshot.target else {
                        panic!("filesystem provenance")
                    };
                    let selection =
                        probe_candidates(&key.candidates, classify_local_target).unwrap();
                    assert_eq!(selection.candidate.resolved_path, root.join(path));
                    assert_eq!(selection.candidate.display(), display);
                    assert_eq!(
                        selection.candidate.spans.as_slice(),
                        &[AbsoluteCellSpan { row: 0, start_col, end_col }]
                    );
                    assert_eq!(
                        local_target_action(selection.decision),
                        Some(LocalTargetAction::Reveal)
                    );
                    let cells =
                        selection.candidate.visible_cells(pane, key.view_top, true).unwrap();
                    assert!(cells.contains(0, col));
                    assert!(!cells.contains(0, start_col - 1));
                    assert!(!cells.contains(0, end_col));
                }
                for col in [start_col - 1, end_col, end_col + 1] {
                    if let Some(snapshot) = app.cell_target_at(window, pane, 0, col) {
                        if let ResolvedCellTarget::Path(key) = snapshot.target {
                            assert!(key
                                .candidates
                                .iter()
                                .all(|candidate| candidate.resolved_path != root.join(path)));
                        }
                    }
                }
            }
        }
        let row = "(src/missing.rs:97).";
        let output = format!("\x1b[2J\x1b[H{row}");
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        let snapshot = app.cell_target_at(window, pane, 0, 5).unwrap();
        assert_eq!(snapshot.explicit_path_text().as_deref(), Some("src/missing.rs:97"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Missing-path feedback follows the pointed filename, not adjacent prose or a neighboring file.
#[test]
fn prose_path_feedback_excludes_neighboring_words() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["prose paths"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let cwd = if cfg!(windows) { "file:///C:/work" } else { "file:///work" };
    let row = ".github/scripts/validate_release.py and focused tests/test_release_validation.py. Require stable";
    let text = format!("\x1b]7;{cwd}\x1b\\{row}");
    app.windows[&window].panes[&pane].parser.lock().resize(120, 24);
    assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
    for path in [".github/scripts/validate_release.py", "tests/test_release_validation.py"] {
        let col = row.find(path).unwrap() as u16 + 1;
        let target = app.cell_target_at(window, pane, 0, col).unwrap();
        assert_eq!(target.explicit_path_text().as_deref(), Some(path), "col={col}");
    }
    for word in ["and", "focused", "Require", "stable"] {
        let col = row.find(word).unwrap() as u16;
        let target = app.cell_target_at(window, pane, 0, col);
        assert!(target.and_then(|target| target.explicit_path_text()).is_none(), "{word}");
    }
}

/// Failure feedback stays on the requesting window, escapes controls, and drops closed-pane responses.
#[test]
fn target_failure_notification_is_window_local() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let first = app.__test_seed_child_window(&["first"]);
    let second = app.__test_seed_child_window(&["second"]);
    let pane = app.__test_child_pane_ids(first).unwrap()[0];
    app.frontmost_window = Some(second);
    app.test_clipboard_text = Some("previous".into());
    app.report_failed_target(first, pane, "file manager unavailable\n", "/test/file");
    let message = &app.windows[&first].notification.as_ref().unwrap().message;
    assert!(message.contains("file manager unavailable\\u{a}"));
    assert!(app.windows[&second].notification.is_none());
    app.report_failed_target(second, pane, "late failure", "/stale/file");
    assert!(app.windows[&second].notification.is_none());
}

/// Probe rejection keeps missing and blocked distinct; equal-length present candidates resolve to the earlier start.
#[test]
fn probe_failure_reasons_remain_distinct() {
    let candidates = vec![probe_candidate_at("file", "/work/file", 0, 0)];
    assert_eq!(
        probe_candidates(&candidates, |_| PathOpenDecision::Missing),
        Err("path-error-missing")
    );
    assert_eq!(
        probe_candidates(&candidates, |_| PathOpenDecision::Blocked),
        Err("path-error-blocked")
    );
    let other = probe_candidate_at("tile", "/work/tile", 6, 0);
    for pair in [[candidates[0].clone(), other.clone()], [other.clone(), candidates[0].clone()]] {
        let selected =
            probe_candidates(&pair, |_| PathOpenDecision::Openable(PathKind::File)).unwrap();
        assert_eq!(selected.candidate.resolved_path, PathBuf::from("/work/file"));
    }
}

/// A local OSC 8 destination probes its actual file, never its unrelated visible label or a URI handler.
#[test]
fn local_hyperlink_routes_through_fresh_path_authorization() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["local"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let destination = if cfg!(windows) { "C://work/main.rs:7" } else { "/tmp/main.rs:7" };
    let output = format!("\x1b]8;;{destination}\x1b\\not-the-path\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 0, 1).unwrap();
    let ResolvedCellTarget::Path(key) = &target.target else {
        panic!("local destination must be probed")
    };
    assert_eq!(key.link_destination.as_deref(), Some(destination));
    assert_eq!(key.candidates[0].resolved_path.file_name().unwrap(), "main.rs");
    assert!(matches!(key.candidates[0].target, DetectedTarget::SourceReference(_)));
    assert_eq!(target.display, destination);
    let mut changed = key.clone();
    changed.link_destination = Some(format!("{destination}0"));
    assert!(!same_probe_context(key, &changed));
}

/// File manager URI encoding preserves filenames without interpreting fragments, query syntax, or percent escapes.
#[test]
fn file_manager_uri_encodes_path_bytes() {
    assert_eq!(
        local_file_uri(Path::new("/tmp/a b#c&d%.txt")).unwrap(),
        "file:///tmp/a%20b%23c%26d%25.txt"
    );
    assert!(local_file_uri(Path::new("relative.txt")).is_err());
    assert!(local_file_uri(Path::new("//server/file.txt")).is_err());
}

/// Filesystem disambiguation selects real names from arbitrary surrounding text, not the longest guess.
#[test]
fn spaced_listing_probe_selects_only_existing_file() {
    use sonicterm_cfg::url_scan::{target_candidates_at_char_col_for_style, PathStyle};
    for prefix in ["PM           3984 ", "metadata words ", "owner 123 "] {
        for name in ["test-local-link-actions.ps1", "my local script.txt"] {
            let row = format!("{prefix}{name}");
            let matches = target_candidates_at_char_col_for_style(
                &row,
                row.len() - 2,
                PathStyle::Windows,
                true,
            );
            let candidates = matches
                .iter()
                .map(|item| {
                    let display = &row[item.start..item.end];
                    probe_candidate_at(display, &format!("/work/{display}"), item.start as u16, 0)
                })
                .collect::<Vec<_>>();
            let expected = PathBuf::from(format!("/work/{name}"));
            let selected = probe_candidates(&candidates, |path| {
                if path == expected {
                    PathOpenDecision::Openable(PathKind::File)
                } else {
                    PathOpenDecision::Missing
                }
            })
            .unwrap();
            assert_eq!(selected.candidate.resolved_path, expected);
            assert_eq!(selected.candidate.display(), name);
            assert_eq!(
                probe_candidates(&candidates, |_| PathOpenDecision::Missing),
                Err("path-error-missing")
            );
        }
    }
}

/// File URI links cannot bypass local-target settings or use their visible label as a substitute.
#[test]
fn local_file_links_respect_policy_and_preserve_decoding() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["file uri"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let uri = if cfg!(windows) { "file:///C:/work/a%20b.txt" } else { "file:///tmp/a%20b.txt" };
    let output = format!("\x1b]8;;{uri}\x1b\\main.rs:7\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 0, 1).unwrap();
    let ResolvedCellTarget::Path(key) = target.target else {
        panic!("file URI cannot bypass probes")
    };
    assert_eq!(key.candidates[0].resolved_path.file_name().unwrap(), "a b.txt");
    app.config.terminal.clickable_local_targets = false;
    assert!(matches!(
        app.cell_target_at(window, pane, 0, 1).unwrap().target,
        ResolvedCellTarget::Rejected("path-error-disabled")
    ));
    app.config.terminal.clickable_local_targets = true;
    let invalid = b"\r\x1b]8;;file://remote/work/file.txt\x1b\\main.rs:7\x1b]8;;\x1b\\";
    assert!(app.__test_advance_child_pane_parser(window, pane, invalid));
    assert!(matches!(
        app.cell_target_at(window, pane, 0, 1).unwrap().target,
        ResolvedCellTarget::Rejected("nonlocal file URI")
    ));
}

/// Wrapped local links bind every visible fragment to one destination and preserve hover coverage.
#[test]
fn local_link_wrap_keeps_complete_hover_span() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["wrapped local"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(10, 4);
    let uri = if cfg!(windows) { "C://work/main.rs:7" } else { "/tmp/main.rs:7" };
    let output = format!("\x1b]8;;{uri}\x1b\\abcdefghijklmnop\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 1, 2).unwrap();
    assert_eq!(target.hover_cells.as_ref().unwrap().spans().len(), 2);
    let ResolvedCellTarget::Path(key) = target.target else { panic!("local probe required") };
    assert_eq!(key.rows.len(), 2);
    assert_eq!(key.candidates[0].spans.len(), 2);
}

/// An offscreen label cannot revoke the independently stored destination of its visible cell.
#[test]
fn local_link_without_complete_highlight_still_has_a_probe() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["clipped local"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(4, 12);
    let uri = if cfg!(windows) { "C://work/main.rs:7" } else { "/tmp/main.rs:7" };
    let output = format!("\x1b]8;;{uri}\x1b\\{}\x1b]8;;\x1b\\", "x".repeat(44));
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 5, 1).unwrap();
    assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
}

/// A file under a symlinked folder is selected like any other local file.
#[cfg(unix)]
#[test]
fn reveal_follows_symlinked_parent() {
    let root = native_test_root().join(format!(
        "sonicterm-parent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("notes.txt"), b"notes").unwrap();
    std::os::unix::fs::symlink(&real, root.join("alias")).unwrap();
    assert_eq!(
        classify_local_target(&root.join("alias/notes.txt")),
        PathOpenDecision::Openable(PathKind::File)
    );
    assert!(validate_reveal_target(
        &root.join("alias/notes.txt"),
        PathOpenDecision::Openable(PathKind::File)
    )
    .is_ok());
    std::fs::remove_file(root.join("alias")).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Filesystem roots use navigation rather than being rejected for having no filename.
#[test]
fn local_root_destinations_navigate() {
    let root = if cfg!(windows) { "C:/" } else { "/" };
    let target =
        sonicterm_cfg::url_scan::local_link_target(root, PathStyle::native()).unwrap().unwrap();
    let resolved =
        resolve_detected_path(&target, PathStyle::native(), None, None, "localhost").unwrap();
    assert_eq!(classify_local_target(&resolved), PathOpenDecision::Openable(PathKind::Directory));
}

/// All platforms choose the same action, independent of which policy admitted the file.
#[test]
fn local_file_actions_reveal_and_directory_actions_navigate() {
    assert_eq!(
        local_target_action(PathOpenDecision::Openable(PathKind::File)),
        Some(LocalTargetAction::Reveal)
    );
    assert_eq!(
        local_target_action(PathOpenDecision::SourceReveal),
        Some(LocalTargetAction::Reveal)
    );
    assert_eq!(
        local_target_action(PathOpenDecision::Openable(PathKind::Directory)),
        Some(LocalTargetAction::Navigate)
    );
    assert_eq!(local_target_action(PathOpenDecision::Blocked), None);
    assert_eq!(local_target_action(PathOpenDecision::Missing), None);
}

/// A labeled OSC 8 destination is previewed without normalizing or rewriting its stored URI.
#[test]
fn hyperlink_preview_preserves_targets_for_all_labels() {
    for (label, uri, expected) in [
        ("Docs", "https://example.com/docs", true),
        ("https://example.com/", "https://example.com/", true),
        ("example.com", "https://example.com/", true),
        ("https://trusted.test/", "https://other.test/", true),
        ("Editor", "vscode://file/C:/notes.txt", true),
        ("https://EXAMPLE.com/%2f", "https://EXAMPLE.com/%2f", true),
        ("https://例子.test/e\u{301}", "https://例子.test/e\u{301}", true),
        ("Code", "https://example.com/repo?path=%2Ffile.cs&line=1&lineEnd=10", true),
    ] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["preview"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        let output = format!("\x1b]8;;{uri}\x1b\\{label}\x1b]8;;\x1b\\");
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        let target = app.cell_target_at(window, pane, 0, 1).unwrap();
        assert!(matches!(&target.target, ResolvedCellTarget::Uri(value) if value == uri));
        assert_eq!(target.preview(true, (20.0, 30.0)).is_some(), expected, "{label}");
        assert!(target.preview(false, (20.0, 30.0)).is_none());
    }
}

/// Every wrapped fragment and repeated occurrence exposes the stored destination.
#[test]
fn hyperlink_preview_covers_wrapped_and_repeated_occurrences() {
    let uri = "https://example.com/";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["wrapped"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(12, 8);
    let output = format!(
        "\x1b]8;id=same;{uri}\x1b\\{uri}\x1b]8;;\x1b\\\r\n\x1b]8;id=same;{uri}\x1b\\Docs\x1b]8;;\x1b\\"
    );
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    for (row, col) in [(0, 1), (1, 2)] {
        assert!(app
            .cell_target_at(window, pane, row, col)
            .unwrap()
            .preview(true, (0.0, 0.0))
            .is_some());
    }
    assert!(app.cell_target_at(window, pane, 2, 1).unwrap().preview(true, (0.0, 0.0)).is_some());
}

/// Auto-detected URLs expose the complete query without requiring an OSC 8 label.
#[test]
fn hyperlink_preview_includes_unlabeled_urls() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["plain"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let uri = "https://example.com/?a=1&b=2";
    assert!(app.__test_advance_child_pane_parser(window, pane, uri.as_bytes()));
    let target = app.cell_target_at(window, pane, 0, 2).unwrap();
    let preview = target.preview(true, (0.0, 0.0)).unwrap();
    assert_eq!(preview.uri, uri);
    assert!(preview.available);
    assert!(target.preview(false, (0.0, 0.0)).is_none());
}

/// Preview remains informative for rejected targets and incomplete labels without granting activation.
#[test]
fn hyperlink_preview_preserves_unknown_and_unavailable_targets() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["bounded"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(4, 12);
    let uri = "vscode://file/C:/very/long/notes.txt";
    let output = format!("\x1b]8;;{uri}\x1b\\{uri}\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let target = app.cell_target_at(window, pane, 0, 1).unwrap();
    let preview = target.preview(true, (15.0, 20.0)).unwrap();
    assert_eq!(preview.uri, uri);
    assert!(!preview.available);
    assert_eq!(preview.pointer, (15.0, 20.0));
    app.windows.get_mut(&window).unwrap().modifiers = winit::keyboard::ModifiersState::empty();
    assert!(!app.activate_target_at(window, pane, 0, 1));
}

/// Re-reading the pointed cell observes replacements, and both invalidation paths revoke previews.
#[test]
fn hyperlink_preview_replacement_and_invalidation_are_window_local() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let first = app.__test_seed_child_window(&["first"]);
    let second = app.__test_seed_child_window(&["second"]);
    let pane = app.__test_child_pane_ids(first).unwrap()[0];
    for uri in ["https://first.test/", "https://second.test/"] {
        let output = format!("\r\x1b]8;;{uri}\x1b\\Docs\x1b]8;;\x1b\\");
        assert!(app.__test_advance_child_pane_parser(first, pane, output.as_bytes()));
        let preview =
            app.cell_target_at(first, pane, 0, 1).unwrap().preview(true, (1.0, 1.0)).unwrap();
        assert_eq!(preview.uri, uri);
        app.windows.get_mut(&first).unwrap().link_preview = Some(preview.clone());
        app.windows.get_mut(&second).unwrap().link_preview = Some(preview);
    }
    app.clear_target_hover(first);
    assert!(app.windows[&first].link_preview.is_none());
    assert!(app.windows[&second].link_preview.is_some());
    app.windows.get_mut(&second).unwrap().invalidate_path_hover();
    assert!(app.windows[&second].link_preview.is_none());
    assert!(app.__test_advance_child_pane_parser(first, pane, b"\r\x1b[2Kplain"));
    assert!(app.cell_target_at(first, pane, 0, 1).is_none());
}

/// A reclaimed registry ID cannot expose a fabricated destination.
#[test]
fn hyperlink_preview_missing_registry_entry_is_inert() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["missing"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane]
        .parser
        .lock()
        .grid_mut()
        .row_mut(0)
        .iter_mut()
        .next()
        .unwrap()
        .set_hyperlink(Some(HyperlinkId(u64::MAX)));
    assert!(app.cell_target_at(window, pane, 0, 0).is_none());
}

/// Identical displayed URLs need identical hover coverage regardless of OSC 8 provenance.
#[test]
fn hyperlink_hover_matches_plain_url_coverage() {
    let uri = "https://example.com/";
    for explicit in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["hover"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        let output = if explicit {
            format!("\x1b]8;;{uri}\x1b\\{uri}\x1b]8;;\x1b\\")
        } else {
            uri.to_string()
        };
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        let target = app.cell_target_at(window, pane, 0, 3).unwrap();
        for active in [false, true] {
            let hover = target.hovered(active).unwrap();
            eprintln!(
                "explicit={explicit} active={active} pane={pane} spans={:?}",
                hover.cells.spans()
            );
            assert_eq!(hover.cells.active, active);
            assert_eq!(
                hover.cells.spans(),
                &[sonicterm_render_model::inputs::HoveredUrlSpan {
                    row: 0,
                    start_col: 0,
                    end_col: uri.len() as u16,
                }]
            );
        }
    }
}

/// A multiplexer split redraws a wrapped link one pane row at a time with cursor moves, so no row
/// records a soft wrap. Hovering any fragment still underlines every fragment in its pane, on both
/// sides of the border, and never reaches across it.
#[test]
fn split_pane_links_underline_every_fragment() {
    // A 21-column alternate screen split at column 10, as tmux draws it: the left pane is columns
    // 0-9 and the right pane is columns 11-20.
    let mut output = String::from("\x1b[?1049h");
    for row in 1..=4 {
        output.push_str(&format!("\x1b[{row};11H\u{2502}"));
    }
    // The right pane keeps one tmux-style id and reopens it after each cursor move.
    let right = "\x1b]8;id=tmux4;https://example.com/right\x1b\\";
    output.push_str(&format!("\x1b[1;15H{right}abcdefg\x1b]8;;\x1b\\"));
    output.push_str(&format!("\x1b[2;12H{right}hijklmnopq\x1b]8;;\x1b\\"));
    output.push_str(&format!("\x1b[3;12H{right}rst\x1b]8;;\x1b\\"));
    // The left pane uses an anonymous link and moves with CR LF, as tmux does for the leftmost pane.
    output.push_str("\x1b[1;5H\x1b]8;;https://example.com/left\x1b\\ABCDEF");
    output.push_str("\r\nGHIJKLMNOP\r\nQR\x1b]8;;\x1b\\");
    assert_eq!(
        hover_spans_after(21, 4, &output, 1, 15),
        [hover_span(0, 14, 21), hover_span(1, 11, 21), hover_span(2, 11, 14)]
    );
    assert_eq!(
        hover_spans_after(21, 4, &output, 1, 3),
        [hover_span(0, 4, 10), hover_span(1, 0, 10), hover_span(2, 0, 2)]
    );
}

/// A multiplexer that positions every full-width row with a cursor move never records a soft
/// wrap, so hovering must still find each fragment of a link it redraws across rows.
#[test]
fn full_width_multiplexer_links_underline_every_fragment() {
    // rmux closes the link before each cursor move; Zellij reopens it without closing.
    for row_end in ["\x1b]8;;\x1b\\", ""] {
        let link = "\x1b]8;;https://example.com/full\x1b\\";
        let output = format!(
            "\x1b[?1049h\x1b[1;5H{link}abcdef{row_end}\x1b[2;1H{link}ghijklmnop{row_end}\x1b[3;1H{link}qr\x1b]8;;\x1b\\"
        );
        assert_eq!(
            hover_spans_after(10, 4, &output, 1, 5),
            [hover_span(0, 4, 10), hover_span(1, 0, 10), hover_span(2, 0, 2)]
        );
    }
}

/// On the primary screen only a recorded soft wrap continues a link underline, so separately
/// positioned rows stay separate even when they reach the grid's edges.
#[test]
fn primary_screen_links_continue_only_across_soft_wraps() {
    let link = "\x1b]8;;https://example.com/primary\x1b\\";
    let output = format!("\x1b[1;5H{link}abcdef\x1b[2;1Hghij\x1b]8;;\x1b\\");
    assert_eq!(hover_spans_after(10, 4, &output, 1, 1), [hover_span(1, 0, 4)]);
}

/// On the alternate screen a link continues to another row at a pane edge or across a hanging
/// indent; repeated short links that stop well before the edge keep separate underlines.
#[test]
fn alternate_screen_links_join_pane_edges_and_hanging_indents() {
    let link = "\x1b]8;id=same;https://example.com/edge\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let output = format!(
        "\x1b[?1049h{link}Docs{close}\x1b[2;1H{link}Docs{close}\x1b[3;7H{link}abcd{close}\x1b[4;3H{link}efgh{close}"
    );
    assert_eq!(hover_spans_after(10, 5, &output, 1, 1), [hover_span(1, 0, 4)]);
    assert_eq!(
        hover_spans_after(10, 5, &output, 2, 7),
        [hover_span(2, 6, 10), hover_span(3, 2, 6)]
    );
}

/// A vertical bar counts as a pane border only when both rows draw it in the same column, so a
/// bar in one row's text cannot narrow the pane used to join a link.
#[test]
fn pane_borders_must_appear_in_both_rows() {
    let link = "\x1b]8;;https://example.com/bar\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let output = format!("\x1b[?1049h\x1b[1;8H{link}abc{close}\x1b[2;1Hx\u{2502}{link}defg{close}");
    assert_eq!(hover_spans_after(10, 4, &output, 1, 3), [hover_span(1, 2, 6)]);
    assert_eq!(hover_spans_after(10, 4, &output, 0, 8), [hover_span(0, 7, 10)]);
}

/// A local link that a multiplexer redraws across rows binds its probe to every visible
/// fragment, as a soft-wrapped one does.
#[test]
fn multiplexer_local_link_binds_every_fragment() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["multiplexer local"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(10, 4);
    let uri = if cfg!(windows) { "C://work/main.rs:7" } else { "/tmp/main.rs:7" };
    let link = format!("\x1b]8;;{uri}\x1b\\");
    let output = format!("\x1b[?1049h\x1b[1;5H{link}abcdef\x1b[2;1H{link}ghij\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 1, 2).unwrap();
    assert_eq!(target.hover_cells.as_ref().unwrap().spans().len(), 2);
    let ResolvedCellTarget::Path(key) = target.target else { panic!("local probe required") };
    assert_eq!(key.rows.len(), 2);
    assert_eq!(key.candidates[0].spans.len(), 2);
}
