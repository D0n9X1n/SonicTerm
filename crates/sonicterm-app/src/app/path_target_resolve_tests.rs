//! Preserved resolve path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

/// A valid wide filename keeps both cells and never promotes the suffix to an independent target.
#[test]
fn mapped_wide_path_preserves_complete_identity() {
    let mut grid = Grid::new(60, 3);
    for character in "./目录/file.rs".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    for col in [0, 2, 3, 4, 5, 9] {
        let scan = logical_path_scan_at_cell(
            &grid,
            0,
            AbsoluteCell { row: 0, col },
            PathStyle::Posix,
            true,
        )
        .unwrap();
        assert!(scan
            .candidates
            .iter()
            .any(|candidate| candidate.target
                == DetectedTarget::PathCandidate("./目录/file.rs".into())));
        assert!(!scan
            .candidates
            .iter()
            .any(|candidate| candidate.target == DetectedTarget::PathCandidate("/file.rs".into())));
    }
}

/// The shortest existing candidate wins whatever a longer literal is, so a list member opens even when its literal exists.
#[test]
fn shortest_existing_candidate_wins_over_longer_literal() {
    let member = probe_candidate("a.rs", "/work/a.rs", 0);
    let literal = probe_candidate("a.rs、b.rs", "/work/a.rs、b.rs", 0);
    for decision in [
        PathOpenDecision::Missing,
        PathOpenDecision::Blocked,
        PathOpenDecision::Openable(PathKind::File),
    ] {
        let selected = probe_candidates(&[literal.clone(), member.clone()], |path| {
            if path == literal.resolved_path {
                decision
            } else {
                PathOpenDecision::Openable(PathKind::File)
            }
        })
        .unwrap();
        assert_eq!(selected.candidate, member, "literal decision {decision:?}");
    }
}

/// The shortest existing list member wins before its longer punctuation literal; the exact pane directory resolves both.
#[test]
fn list_paths_resolve_members_before_literals() {
    let root = native_test_root().join(format!(
        "sonicterm-list-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "first").unwrap();
    std::fs::write(root.join("b.rs"), "second").unwrap();
    let literal = root.join("src/a.rs、b.rs");
    std::fs::write(&literal, "literal").unwrap();
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", root.to_str().unwrap().replace('\\', "/")))
    } else {
        root.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("list");
    let window = app.main_window_id.unwrap();
    app.windows[&window].panes[&pane].parser.lock().resize(60, 3);
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        format!("\x1b]7;{cwd}\x1b\\src/a.rs、b.rs").as_bytes()
    ));
    for (col, member) in [(3, root.join("src/a.rs")), (10, root.join("b.rs"))] {
        let snapshot = app.cell_target_at(window, pane, 0, col).unwrap();
        let ResolvedCellTarget::Path(key) = snapshot.target else { panic!("path") };
        // The shorter existing member wins even though the longer literal also exists.
        assert_eq!(
            probe_candidates(&key.candidates, classify_local_target)
                .unwrap()
                .candidate
                .resolved_path,
            member
        );
        // Once the member is gone, the literal is the shortest existing candidate.
        let moved = root.join("moved");
        std::fs::rename(&member, &moved).unwrap();
        assert_eq!(
            probe_candidates(&key.candidates, classify_local_target)
                .unwrap()
                .candidate
                .resolved_path,
            literal
        );
        std::fs::rename(&moved, &member).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Home paths beside Chinese prose resolve without OSC 7, choose the shortest existing file over a longer literal, and
/// authorize only their exact cells.
#[test]
fn home_prose_paths_resolve_real_files_without_cwd() {
    assert_home_prose_paths_resolve("sonicterm-home-prose", |root| {
        std::fs::write(root.join(".claude/settings.json"), "inert fixture").unwrap();
    });
}

/// The same holds when `~/.claude/settings.json` is a symlink, as it is when settings sync from a dotfiles folder.
#[cfg(unix)]
#[test]
fn home_prose_paths_resolve_symlinked_settings_without_cwd() {
    assert_home_prose_paths_resolve("sonicterm-home-prose-link", |root| {
        let synced = root.join("dot-configs/settings.json");
        std::fs::create_dir_all(synced.parent().unwrap()).unwrap();
        std::fs::write(&synced, "inert fixture").unwrap();
        std::os::unix::fs::symlink(&synced, root.join(".claude/settings.json")).unwrap();
    });
}

/// Prose alternatives validate the full literal, including wide punctuation and suffix cells outside the active span.
#[test]
fn home_prose_paths_reject_unsafe_suffix_cells() {
    let path = "~/.claude.json";
    let text = format!("{path}，正文");
    for protected in path.len()..path.len() + 6 {
        for mutation in 0..3 {
            let mut grid = Grid::new(80, 4);
            for character in text.chars() {
                grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
            let baseline = logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 3 },
                PathStyle::native(),
                true,
            )
            .unwrap();
            assert!(baseline
                .candidates
                .iter()
                .any(|candidate| candidate.target == DetectedTarget::PathCandidate(path.into())));
            let cell = grid.row_mut(0).iter_mut().nth(protected).unwrap();
            match mutation {
                0 => cell.set_hyperlink(Some(HyperlinkId(7))),
                1 => cell.set_extras(Some("\u{301}".into())),
                _ => cell.flags.toggle(CellFlags::WIDE_CONT),
            }
            let scan = logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 3 },
                PathStyle::native(),
                true,
            );
            assert!(
                scan.is_none_or(|scan| scan.candidates.iter().all(
                    |candidate| candidate.target != DetectedTarget::PathCandidate(path.into())
                )),
                "cell {protected}, mutation {mutation}"
            );
        }
    }
}

/// Dot-relative prose alternatives need the exact local OSC 7 directory; home paths never supply that authority.
#[test]
fn prose_relative_paths_require_local_pane_cwd() {
    for path in ["./notes", "../notes"] {
        for authority in [None, Some("remote.invalid"), Some("")] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.local_hostname = "local.invalid".into();
            let pane = app.__test_seed_tab("relative prose");
            let window = app.main_window_id.unwrap();
            let cwd_path = if cfg!(windows) { "/C:/work/child" } else { "/work/child" };
            let mut output = String::new();
            if let Some(authority) = authority {
                output = format!("\x1b]7;file://{authority}{cwd_path}\x1b\\");
            }
            output.push_str(&format!("{path}，正文"));
            app.windows[&window].panes[&pane].parser.lock().advance(output.as_bytes());
            let snapshot = app.cell_target_at(window, pane, 0, 3);
            if authority == Some("") {
                let ResolvedCellTarget::Path(key) = snapshot.expect("local CWD candidate").target
                else {
                    panic!("path target")
                };
                let candidate = key
                    .candidates
                    .iter()
                    .find(|candidate| {
                        candidate.target == DetectedTarget::PathCandidate(path.into())
                    })
                    .expect("relative prose prefix");
                let directory = if cfg!(windows) { "C:\\work" } else { "/work" };
                let expected = if path.starts_with("../") {
                    PathBuf::from(directory).join("notes")
                } else {
                    PathBuf::from(directory).join("child/notes")
                };
                assert_eq!(candidate.resolved_path, expected);
            } else {
                assert!(snapshot.is_none(), "untrusted or absent CWD must not resolve {path}");
            }
        }
    }
}

/// Long report paths resolve from the reporting pane under a spaced parent; removing the file changes the real probe result.
#[test]
fn report_paths_use_exact_spaced_pane_directory() {
    let root = native_test_root().join(format!(
        "sonicterm-report-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let directory = root.join("OneDrive - Microsoft/Desktop/AgentRelated/pax");
    std::fs::create_dir_all(directory.join("reports/investigate-online-issue")).unwrap();
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", directory.to_str().unwrap().replace('\\', "/")))
    } else {
        directory.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("report");
    let window = app.main_window_id.unwrap();
    app.windows[&window].panes[&pane].parser.lock().resize(160, 4);
    for (row, extension) in ["md", "html"].into_iter().enumerate() {
        let relative = format!(
            "reports/investigate-online-issue/20260918T180000Z-skills-classification-flight.{extension}"
        );
        let file = directory.join(&relative);
        std::fs::write(&file, "inert report").unwrap();
        assert!(app.__test_advance_child_pane_parser(
            window,
            pane,
            format!("\x1b]7;{cwd}\x1b\\({relative})，正文\r\n").as_bytes()
        ));
        let snapshot = app.cell_target_at(window, pane, row as u16, 12).unwrap();
        let ResolvedCellTarget::Path(key) = snapshot.target else { panic!("path") };
        let selected = probe_candidates(&key.candidates, classify_local_target).unwrap();
        assert_eq!(selected.candidate.resolved_path, file);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            probe_candidates(&key.candidates, classify_local_target).unwrap_err(),
            "path-error-missing"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Failed explicit clicks log the current pane snapshot, never an old probe or another pane's directory.
#[test]
fn failed_path_diagnostics_preserve_clicked_pane_identity() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["first", "second"]);
    let panes = app.__test_child_pane_ids(window).unwrap();
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    for (index, pane) in panes.iter().copied().enumerate() {
        let directory = if cfg!(windows) {
            format!("/C:/diagnostic-{index}")
        } else {
            format!("/diagnostic-{index}")
        };
        let text = format!(
            "\x1b]7;file://{directory}\x1b\\(reports/missing.md)，private-unrelated-output"
        );
        assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
        let target = app.cell_target_at(window, pane, 0, 3).unwrap();
        let ResolvedCellTarget::Path(key) = target.target else { panic!("path") };
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result =
            PathProbeResult { request, selection: None, failure: Some("path-error-missing") };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        // A confirmed-missing auto-detected target logs its pane identity, then stays an ordinary click.
        let log = capture_path_diagnostics(sonicterm_logging::LogLevel::Debug, || {
            assert!(!app.activate_target_at(window, pane, 0, 3));
        });
        assert!(log.contains("local path activation unverified"), "{log}");
        assert!(log.contains(&format!("pane_id={pane}")), "{log}");
        assert!(log.contains(&format!("window_id={window:?}")), "{log}");
        assert!(log.contains("reason=\"path-error-missing\""), "{log}");
        assert!(log.contains(&format!("cwd_revision={}", key.cwd_revision)), "{log}");
        assert!(log.contains(&format!("diagnostic-{index}")), "{log}");
        assert!(log.contains(&format!("{:?}", key.candidates[0].resolved_path)), "{log}");
        assert!(!log.contains(&format!("diagnostic-{}", 1 - index)), "{log}");
        assert!(!log.contains("private-unrelated-output"), "{log}");

        let changed_directory = directory.replace("diagnostic", "changed");
        assert!(app.__test_advance_child_pane_parser(
            window,
            pane,
            format!("\x1b]7;file://{changed_directory}\x1b\\").as_bytes()
        ));
        let log = capture_path_diagnostics(sonicterm_logging::LogLevel::Debug, || {
            assert!(app.activate_target_at(window, pane, 0, 3));
        });
        assert!(log.contains("reason=\"path-error-pending\""), "{log}");
        assert!(log.contains(&format!("changed-{index}")), "{log}");
        assert!(!log.contains("diagnostic-"), "{log}");
        assert!(!log.contains("path-error-missing"), "{log}");
    }
}

/// Default warning-level logging and unverified bare-name clicks never expose candidate paths.
#[test]
fn failed_path_diagnostics_remain_opt_in() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["diagnostics"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let cwd = if cfg!(windows) { "file:///C:/diagnostic" } else { "file:///diagnostic" };
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        format!("\x1b]7;{cwd}\x1b\\reports/missing.md").as_bytes()
    ));
    let log = capture_path_diagnostics(sonicterm_logging::LogLevel::Warn, || {
        assert!(app.activate_target_at(window, pane, 0, 3));
    });
    assert!(!log.contains("local path activation unverified"), "{log}");
    assert!(!log.contains("diagnostic"), "{log}");
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\r\x1b[2Kmissing.md"));
    let log = capture_path_diagnostics(sonicterm_logging::LogLevel::Debug, || {
        assert!(!app.activate_target_at(window, pane, 0, 3));
    });
    assert!(!log.contains("local path activation unverified"), "{log}");
}

/// Failed-click recovery copies only the target and reports real clipboard success or failure.
#[test]
fn failed_click_copies_target_without_error_text() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["copy failure"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.test_clipboard_text = Some("previous".into());
    app.report_failed_target(window, pane, "file not found", "C:\\work\\main.rs");
    assert_eq!(app.test_clipboard_text.as_deref(), Some("previous"));
    let message = &app.windows[&window].notification.as_ref().unwrap().message;
    assert!(message.contains("C:\\work\\main.rs"));
    assert!(message.contains(&app.i18n.translate("path-error-copy-again")));
    assert!(app.copy_confirmed_failure(window, pane, "C:\\work\\main.rs"));
    assert_eq!(app.test_clipboard_text.as_deref(), Some("C:\\work\\main.rs"));
    assert!(app.windows[&window]
        .notification
        .as_ref()
        .unwrap()
        .message
        .contains(&app.i18n.translate("path-error-copied")));
    let message = &app.windows[&window].notification.as_ref().unwrap().message;
    assert_eq!(message.lines().next(), Some(app.i18n.translate("path-error-copied").as_str()));
    assert!(message.lines().nth(2).unwrap().contains("file not found"));
    app.test_clipboard_write_failure = true;
    app.report_failed_target(window, pane, "blocked", "another path");
    assert!(app.copy_confirmed_failure(window, pane, "another path"));
    assert_eq!(app.test_clipboard_text.as_deref(), Some("C:\\work\\main.rs"));
    assert!(app.windows[&window]
        .notification
        .as_ref()
        .unwrap()
        .message
        .contains(&app.i18n.translate("path-error-copy-failed")));
    app.test_clipboard_write_failure = false;
    app.report_failed_target(window, u64::MAX, "late error", "stale path");
    assert_eq!(app.test_clipboard_text.as_deref(), Some("C:\\work\\main.rs"));
}

/// Only current actionable paths notify; retrying the same visible failure copies without opening again.
#[test]
fn local_click_requires_authorization_and_copy_confirmation() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["click"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    app.test_clipboard_text = Some("previous".into());
    let path = if cfg!(windows) { "C:/work/file.txt" } else { "/work/file.txt" };
    let output = format!("\x1b]8;;{path}\x1b\\file\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let target = app.cell_target_at(window, pane, 0, 1).unwrap();
    let ResolvedCellTarget::Path(key) = target.target else { panic!("local target") };
    assert!(app.activate_target_at(window, pane, 0, 1));
    assert!(app.windows[&window].notification.as_ref().unwrap().message.contains(path));
    app.windows.get_mut(&window).unwrap().notification = None;
    assert_eq!(app.test_clipboard_text.as_deref(), Some("previous"));
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    let missing = PathProbeResult {
        request: request.clone(),
        selection: None,
        failure: Some("path-error-missing"),
    };
    assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&missing, Some(&key)));
    assert!(app.activate_target_at(window, pane, 0, 1));
    assert!(app.windows[&window].notification.as_ref().unwrap().message.contains(path));
    app.windows.get_mut(&window).unwrap().notification = None;
    let openable = openable_result(request);
    assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&openable, Some(&key)));
    let resolved = key.candidates[0].resolved_path.to_string_lossy().into_owned();
    assert!(app.activate_target_at(window, pane, 0, 1));
    assert_eq!(app.test_clipboard_text.as_deref(), Some("previous"));
    assert!(app.windows[&window].notification.as_ref().unwrap().message.contains(&resolved));
    assert!(app.activate_target_at(window, pane, 0, 1));
    assert_eq!(app.test_clipboard_text.as_deref(), Some(resolved.as_str()));
    app.report_failed_target(window, pane, "failed", &resolved);
    app.windows.get_mut(&window).unwrap().notification = None;
    assert!(!app.copy_confirmed_failure(window, pane, &resolved));
    app.report_failed_target(window, pane, "failed", &resolved);
    assert!(!app.copy_confirmed_failure(window, pane, "different"));
    assert!(!app.copy_confirmed_failure(window, pane, &resolved));
}

/// Unverified bare-name guesses stay inert even when the scanner enumerates listing metadata.
#[test]
fn unverified_plain_text_click_has_no_side_effects() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["plain"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.test_clipboard_text = Some("previous".into());
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let cwd = if cfg!(windows) { "file:///C:/work" } else { "file:///work" };
    let row = "-a---           9/11/2026  4:04 PM           4230 missing-script.ps1";
    let text = format!("\x1b]7;{cwd}\x1b\\{row}");
    assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
    let col = row.find("script").unwrap() as u16;
    let target = app.cell_target_at(window, pane, 0, col).unwrap();
    assert!(target.explicit_path_text().is_none());
    let ResolvedCellTarget::Path(key) = target.target else { panic!("bare candidates") };
    assert!(!app.activate_target_at(window, pane, 0, col));
    for reason in ["path-error-missing", "path-error-blocked"] {
        app.windows.get_mut(&window).unwrap().path_probe.invalidate();
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result = PathProbeResult { request, selection: None, failure: Some(reason) };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        assert!(!app.activate_target_at(window, pane, 0, col));
        assert!(app.windows[&window].notification.is_none());
        assert_eq!(app.test_clipboard_text.as_deref(), Some("previous"));
    }
}

/// A source location permits feedback, but unverified prose around its bare filename does not.
#[test]
fn source_location_feedback_excludes_unverified_prose() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["source location"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let cwd = if cfg!(windows) { "file:///C:/work" } else { "file:///work" };
    let explicit = if cfg!(windows) {
        "C:/work/Test Folder/missing.rs"
    } else {
        "/work/Test Folder/missing.rs"
    };
    for row in [explicit.to_owned(), format!("{explicit}:7")] {
        let text = format!("\x1b]7;{cwd}\x1b\\\r\x1b[2K{row}");
        assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
        let col = row.find("missing").unwrap() as u16;
        let target = app.cell_target_at(window, pane, 0, col).unwrap();
        assert_eq!(target.explicit_path_text().as_deref(), Some(row.as_str()));
    }
    for row in ["missing.ps1:42", "some words missing.ps1:42"] {
        let text = format!("\x1b]7;{cwd}\x1b\\\r\x1b[2K{row}");
        assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
        let col = row.find("missing").unwrap() as u16;
        let target = app.cell_target_at(window, pane, 0, col).unwrap();
        assert_eq!(target.explicit_path_text().as_deref(), Some("missing.ps1:42"));
        if row.starts_with("some") {
            let prose = app.cell_target_at(window, pane, 0, 1).unwrap();
            assert!(prose.explicit_path_text().is_none());
        }
    }
}
