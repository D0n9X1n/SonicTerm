use super::linux::{linux_xdg_open_spec, local_file_uri, with_opened_target};
use super::macos::{macos_directory_policy, macos_open_spec, macos_reveal_spec};
use super::windows_os::windows_path_policy;
use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme, url_scan::DetectedTarget};
use sonicterm_grid::grid::{Cell, CellFlags, Color, Row};
use sonicterm_types::HyperlinkId;

// Temp roots can include a symlink (macOS) or a junction (Windows); resolve the fixture root on
// every platform, never the target under test.
pub(super) fn native_test_root() -> PathBuf {
    plain_test_root(std::env::temp_dir().canonicalize().unwrap())
}

// Unix canonical paths are already in the plain form the classifier accepts.
#[cfg(not(windows))]
fn plain_test_root(root: PathBuf) -> PathBuf {
    root
}

// Windows canonical paths carry a verbatim `\\?\C:\` prefix, which the classifier rejects as a
// UNC form; rebuild the ordinary drive path from the same resolved components.
#[cfg(windows)]
fn plain_test_root(root: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    let drive = match root.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => Some(letter),
            _ => None,
        },
        _ => None,
    };
    let Some(drive) = drive else {
        return root;
    };
    let rest =
        root.components().skip(1).filter(|component| !matches!(component, Component::RootDir));
    let mut plain = PathBuf::from(format!("{}:\\", drive as char));
    plain.extend(rest);
    plain
}

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

/// Creates `~/.claude/settings.json` with `create_settings` and `~/.claude.json` as a regular file in a scratch
/// home, then checks both paths in the Chinese sentence in a main and a child window at two widths.
fn assert_home_prose_paths_resolve(prefix: &str, create_settings: impl Fn(&Path)) {
    let root = native_test_root().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    create_settings(&root);
    std::fs::write(root.join(".claude.json"), "inert fixture").unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.home_dir = Some(root.clone());
    app.__test_seed_tab("home prose main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["home prose child"]);
    let text = "同步时会将客户端设置写入 ~/.claude/settings.json，将 MCP 配置块写入顶层的 ~/.claude.json，并将权限";
    for window in [main, child] {
        let pane = *app.windows[&window].panes.keys().next().unwrap();
        app.frontmost_window = Some(window);
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        for width in [160, 40] {
            let parser = app.windows[&window].panes[&pane].parser.clone();
            {
                let mut parser = parser.lock();
                parser.resize(width, 8);
                parser.advance(format!("\x1b[2J\x1b[H{text}").as_bytes());
                assert!(parser.osc7_cwd().is_none());
            }
            let cells = parser
                .lock()
                .grid()
                .rows_iter()
                .enumerate()
                .flat_map(|(row, line)| {
                    line.iter().enumerate().map(move |(col, cell)| {
                        (AbsoluteCell { row: row as u64, col: col as u16 }, cell.ch)
                    })
                })
                .collect::<Vec<_>>();
            let starts = cells
                .iter()
                .enumerate()
                .filter_map(|(index, (_, character))| (*character == '~').then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(starts.len(), 2);
            for (start, (path, suffix)) in starts
                .into_iter()
                .zip([(".claude/settings.json", "，将"), (".claude.json", "，并将权限")])
            {
                let literal = root.join(format!("{path}{suffix}"));
                let expected = root.join(path);
                let end = start + path.len() + 2;
                for (pointed, _) in &cells[start..end] {
                    let snapshot = app
                        .cell_target_at(window, pane, pointed.row as u16, pointed.col)
                        .expect("home path must have candidates without OSC 7");
                    let ResolvedCellTarget::Path(key) = snapshot.target.clone() else {
                        panic!("path target")
                    };
                    let selection = probe_candidates(&key.candidates, classify_local_target)
                        .expect("the shortest existing home file is selected");
                    assert_eq!(selection.candidate.resolved_path, expected);
                    assert_eq!(selection.candidate.char_len(), path.len() + 2);
                    assert!(!selection
                        .candidate
                        .spans
                        .iter()
                        .any(|span| span.contains(cells[end].0)));
                    let request =
                        app.windows.get_mut(&window).unwrap().path_probe.request(key.clone());
                    if let Some(request) = request {
                        let result =
                            PathProbeResult { request, selection: Some(selection), failure: None };
                        assert!(app
                            .windows
                            .get_mut(&window)
                            .unwrap()
                            .path_probe
                            .accept(&result, Some(&key)));
                    }
                    app.apply_target_hover(window, Some(snapshot));
                    assert!(app.windows[&window].path_probe.authorized(&key, true));
                    assert_eq!(
                        PathBuf::from(&app.windows[&window].link_preview.as_ref().unwrap().uri),
                        expected
                    );
                    // A blocked or existing longer literal cannot displace the shorter existing file.
                    assert_eq!(
                        probe_candidates(&key.candidates, |candidate| {
                            if candidate == literal {
                                PathOpenDecision::Blocked
                            } else {
                                classify_local_target(candidate)
                            }
                        })
                        .unwrap()
                        .candidate
                        .resolved_path,
                        expected
                    );
                    std::fs::write(&literal, "literal fixture").unwrap();
                    assert_eq!(
                        probe_candidates(&key.candidates, classify_local_target)
                            .unwrap()
                            .candidate
                            .resolved_path,
                        expected
                    );
                    std::fs::remove_file(&literal).unwrap();
                }
                // Both cells of the wide comma and the following prose must not borrow the shorter path's authority.
                for (pointed, _) in &cells[end..end + suffix.chars().count() * 2] {
                    if let Some(snapshot) =
                        app.cell_target_at(window, pane, pointed.row as u16, pointed.col)
                    {
                        if let ResolvedCellTarget::Path(key) = snapshot.target {
                            assert!(
                                probe_candidates(&key.candidates, classify_local_target).is_err()
                            );
                            assert!(!key
                                .candidates
                                .iter()
                                .any(|candidate| candidate.resolved_path == expected));
                        }
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
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

#[derive(Clone, Default)]
struct PathDiagnosticLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for PathDiagnosticLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Capture only this thread's configured path diagnostics without uncaptured siblings disabling their call sites.
fn capture_path_diagnostics(level: sonicterm_logging::LogLevel, action: impl FnOnce()) -> String {
    let log = PathDiagnosticLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(sonicterm_logging::filter_for_level(level))
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let output = log.0.lock().unwrap().clone();
    String::from_utf8(output).unwrap()
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

/// Build a child pane of `cols` by `rows`, feed `output`, and resolve the target at one cell.
fn target_after(
    cols: u16,
    rows: u16,
    output: &str,
    row: u16,
    col: u16,
) -> Option<CellTargetSnapshot> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["multiplexer"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.cell_target_at(window, pane, row, col)
}

/// Resolve the target at one cell of a fed pane and return its hover spans.
fn hover_spans_after(
    cols: u16,
    rows: u16,
    output: &str,
    row: u16,
    col: u16,
) -> Vec<sonicterm_render_model::inputs::HoveredUrlSpan> {
    target_after(cols, rows, output, row, col).unwrap().hover_cells.unwrap().spans().to_vec()
}

fn hover_span(
    row: u16,
    start_col: u16,
    end_col: u16,
) -> sonicterm_render_model::inputs::HoveredUrlSpan {
    sonicterm_render_model::inputs::HoveredUrlSpan { row, start_col, end_col }
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

/// Draw a vertical pane border down one 1-based column on every row, as tmux does for a split.
fn pane_border(column: u16, rows: u16) -> String {
    (1..=rows).map(|row| format!("\x1b[{row};{column}H\u{2502}")).collect()
}

/// A plain URL that fills its pane on the alternate screen continues on the next row of that pane,
/// which a multiplexer positions with a cursor move instead of a soft wrap, at the grid's edge and
/// at a split border.
#[test]
fn multiplexer_pane_edge_urls_join_the_next_row() {
    // The URL fills all 20 columns of a full-width pane and the next row is positioned directly.
    let full = "\x1b[?1049h\x1b[1;1Hhttps://example.com/\x1b[2;1Hmore";
    let target = target_after(20, 4, full, 0, 5).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/more".into()));
    // In a 41-column split with its border in column 20, both panes' URLs fill their rows.
    let mut split = format!("\x1b[?1049h{}", pane_border(21, 4));
    split.push_str("\x1b[1;1Hhttps://example.com.\x1b[2;1Htail");
    split.push_str("\x1b[1;22Hhttps://example.com/\x1b[2;22Hmore");
    let left = target_after(41, 4, &split, 0, 5).unwrap();
    assert_eq!(left.target, ResolvedCellTarget::Uri("https://example.com.tail".into()));
    let right = target_after(41, 4, &split, 0, 25).unwrap();
    assert_eq!(right.target, ResolvedCellTarget::Uri("https://example.com/more".into()));
}

/// A URL that stops short of its pane's edge, or that the terminal itself soft-wrapped, is
/// complete on the alternate screen and still resolves to its full destination.
#[test]
fn complete_urls_resolve_on_alternate_screen() {
    let short = format!("\x1b[?1049h{}\x1b[1;1Hhttps://example.com/a next", pane_border(31, 4));
    let target = target_after(41, 4, &short, 0, 3).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/a".to_string()));
    let wrapped = "\x1b[?1049h\x1b[1;1Hhttps://example.com/abcdefghij";
    let target = target_after(20, 4, wrapped, 0, 5).unwrap();
    let full = "https://example.com/abcdefghij".to_string();
    assert_eq!(target.target, ResolvedCellTarget::Uri(full));
}

/// A target starting at its pane's left edge under a row that filled the pane may be the rest of
/// a longer target, so the alternate screen refuses it; under a shorter row it resolves.
#[test]
fn pane_edge_continuations_are_refused() {
    let url = "https://example.com/next";
    let continued = format!("\x1b[?1049h\x1b[1;1Habcdefghijklmnopqrstuvwxyz0123\x1b[2;1H{url}");
    assert!(target_after(30, 4, &continued, 1, 5).is_none());
    let separate = format!("\x1b[?1049h\x1b[1;1Habcdefghij\x1b[2;1H{url}");
    let target = target_after(30, 4, &separate, 1, 5).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri(url.to_string()));
}

/// Programs write primary-screen lines in order, so a row that ends at the grid's edge without a
/// soft wrap is a real line end, and its URL resolves as it always has.
#[test]
fn primary_screen_urls_at_the_edge_resolve() {
    let target = target_after(20, 4, "https://example.com/\r\nnext", 0, 5).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/".to_string()));
}

/// Return a native absolute path ending in `tail`, in this build's path grammar.
fn native_path(tail: &str) -> String {
    let root = if cfg!(windows) { "C:/" } else { "/" };
    format!("{root}{tail}")
}

/// A complete plain path in either half of a split pane resolves: the scan reads only the pointed
/// pane, so the padding, the border and the other pane's text never join a candidate.
#[test]
fn split_pane_plain_paths_resolve() {
    let path = native_path("tmp/hosts");
    let mut split = format!("\x1b[?1049h{}", pane_border(21, 4));
    split.push_str(&format!("\x1b[1;1H{path}\x1b[1;23H{path}"));
    for col in [2, 24] {
        let target = target_after(41, 4, &split, 0, col)
            .unwrap_or_else(|| panic!("the path at column {col} resolved to nothing"));
        assert!(matches!(target.target, ResolvedCellTarget::Path(_)), "column {col}");
    }
}

/// A plain path followed by words that fill its pane joins the next row on the alternate screen, and
/// the shortest candidate, the path itself, is probed first; with room left on the row it resolves alone.
#[test]
fn plain_paths_before_words_that_fill_the_row_resolve_shortest_first() {
    let line = format!("{} and more words here", native_path("tmp/hosts"));
    let filled = u16::try_from(line.chars().count()).unwrap();
    let output = format!("\x1b[?1049h\x1b[1;1H{line}\x1b[2;1Hnext");
    for cols in [filled, filled + 1] {
        let target = target_after(cols, 4, &output, 0, 2).unwrap();
        let ResolvedCellTarget::Path(key) = target.target else { panic!("path at {cols}") };
        assert_eq!(key.candidates[0].display(), native_path("tmp/hosts"), "cols {cols}");
    }
}

/// A spaced name that fills its pane joins the next row of the same pane, at the grid's edge or at
/// a split border, and the shortest candidate is probed first; with a blank column it ends there.
#[test]
fn cut_spaced_names_join_the_next_pane_row() {
    let line = format!("{} full.txt", native_path("tmp/report"));
    let cols = u16::try_from(line.chars().count()).unwrap();
    let output = format!("\x1b[?1049h\x1b[1;1H{line}\x1b[2;1H-more");
    // `pane_border` takes a 1-based column, so this border sits right after the name.
    let cut = format!("\x1b[?1049h{}\x1b[1;1H{line}\x1b[2;1H-more", pane_border(cols + 1, 4));
    let spare = format!("\x1b[?1049h{}\x1b[1;1H{line}\x1b[2;1H-more", pane_border(cols + 2, 4));
    for (width, fed, joined) in
        [(cols, &output, true), (cols + 21, &cut, true), (cols + 22, &spare, false)]
    {
        let target = target_after(width, 4, fed, 0, 3).unwrap();
        let ResolvedCellTarget::Path(key) = target.target else { panic!("path at {width}") };
        assert_eq!(key.candidates[0].display(), native_path("tmp/report"), "width {width}");
        assert_eq!(
            key.candidates.iter().any(|candidate| candidate.display().ends_with("-more")),
            joined,
            "width {width}"
        );
    }
}

/// A bracketed URL that fills a split pane's right side is not rebuilt from the next grid row,
/// which begins with another pane's text; the pane-bounded scan refuses it as cut. In a
/// full-width pane, the same application hard wrap still joins.
#[test]
fn bracketed_urls_join_only_in_full_width_panes() {
    let mut split = format!("\x1b[?1049h{}", pane_border(21, 4));
    split.push_str("\x1b[1;22H(https://ex.co/aaaaa\x1b[2;1Hbb) done");
    assert!(target_after(41, 4, &split, 0, 25).is_none());
    let full_width = "\x1b[?1049h\x1b[1;1H(https://ex.co/aaaaa\x1b[2;1Hbb) done";
    let target = target_after(20, 4, full_width, 0, 3).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://ex.co/aaaaabb".to_string()));
}

/// A plain URL that the terminal wrapped beside a program-drawn vertical guide resolves from
/// either row: a recorded wrap is read whole, so the guide bounds neither row of it.
#[test]
fn wrapped_urls_beside_a_guide_join_from_either_row() {
    let url = "https://example.com/abcdefghij";
    let output = format!("\x1b[?1049h\u{2502} heading\r\n\u{2502} {url}");
    for (row, col) in [(1, 5), (2, 0)] {
        let target = target_after(30, 5, &output, row, col).unwrap_or_else(|| {
            panic!("the wrapped URL at row {row}, column {col} resolved to nothing")
        });
        assert_eq!(target.target, ResolvedCellTarget::Uri(url.to_string()));
    }
}

/// A bracketed URL that an application hard-wrapped beside a program-drawn guide still joins: the
/// guide marks only the first row, so no pane border runs down both rows.
#[test]
fn bracketed_urls_beside_a_guide_still_join() {
    let url_head = format!("https://ex.co/{}", "a".repeat(13));
    let output = format!("\x1b[?1049h\u{2502} heading\r\n\u{2502} ({url_head}\x1b[3;1Hbb) done");
    let target = target_after(30, 5, &output, 1, 5).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri(format!("{url_head}bb")));
}

/// A hard-wrap chain never continues across a pane border that both rows draw: the next grid row
/// belongs to the other pane, whether the joined text would validate (`) more`) or not (`)z`).
#[test]
fn hardwrap_chains_never_cross_a_split_border() {
    for lower in ["/tmp/hosts) more", "/tmp/hosts)z more"] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["multiplexer"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(41, 4);
        let output = format!(
            "\x1b[?1049h{}\x1b[1;22H(https://ex.co/aaaaa\x1b[2;1H{lower}",
            pane_border(21, 4)
        );
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        let parser = app.windows[&window].panes[&pane].parser.lock();
        // A fresh grid has no scrollback, so the alternate screen's first row is absolute row 0.
        for (row, col) in [(0, 25), (1, 3)] {
            let found = hardwrap_uri_at_cell(parser.grid(), 0, AbsoluteCell { row, col });
            assert!(
                matches!(found, HardwrapUri::NotApplicable),
                "{lower:?} at row {row}, column {col} joined across the split border"
            );
        }
    }
}

/// A plain path that fills its pane continues on the next row of that pane, at the grid's edge and
/// at a split border, so the row-0 prefix is never offered on its own.
#[test]
fn multiplexer_pane_edge_paths_join_the_next_row() {
    let path = native_path(&"p".repeat(20 - native_path("").chars().count()));
    let full = format!("\x1b[?1049h\x1b[1;1H{path}\x1b[2;1Hmore");
    let mut split = format!("\x1b[?1049h{}", pane_border(21, 4));
    split.push_str(&format!("\x1b[1;1H{path}\x1b[2;1Hmore"));
    for (cols, fed) in [(20, &full), (41, &split)] {
        let target = target_after(cols, 4, fed, 0, 3).unwrap();
        let ResolvedCellTarget::Path(key) = target.target else { panic!("path at {cols}") };
        assert!(key.candidates.iter().all(|candidate| candidate.spans.len() == 2), "cols {cols}");
        assert_eq!(key.candidates[0].display(), format!("{path}more"), "cols {cols}");
    }
}

/// A plain path at its pane's left edge under a row that filled the pane may be the rest of a
/// longer path, so the alternate screen refuses it; under a shorter row it resolves.
#[test]
fn pane_edge_path_continuations_are_refused() {
    let path = native_path("tmp/hosts");
    let continued = format!("\x1b[?1049h\x1b[1;1H{}\x1b[2;1H{path}", "x".repeat(30));
    assert!(target_after(30, 4, &continued, 1, 3).is_none());
    let separate = format!("\x1b[?1049h\x1b[1;1Habcdefghij\x1b[2;1H{path}");
    let target = target_after(30, 4, &separate, 1, 3).unwrap();
    assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
}

/// A wrap that the right pane's text records at the grid's edge does not continue a left-pane URL
/// that ends at the border between them, so the cut check still applies at that border.
#[test]
fn grid_wraps_do_not_continue_a_tail_at_a_pane_border() {
    let output = format!(
        "\x1b[?1049hhttps://example.com/\u{2502}{}more{}\u{2502}",
        "r".repeat(20),
        " ".repeat(16)
    );
    assert!(target_after(41, 4, &output, 0, 5).is_none());
}

/// A right-pane URL that starts at the border on a row the grid wrapped into is still checked as a
/// possible continuation, because the wrap came from the row above filling the right pane.
#[test]
fn grid_wraps_do_not_continue_a_head_at_a_pane_border() {
    let output = format!(
        "\x1b[?1049h{}\u{2502}{}{}\u{2502}https://ex.co/a",
        " ".repeat(20),
        "r".repeat(20),
        " ".repeat(20)
    );
    assert!(target_after(41, 4, &output, 1, 25).is_none());
}

/// A grid wrap joins only the pane that reaches the grid's right edge to the pane that starts at
/// its left edge. A spaced name that fills the left pane stays cut at the border when the right
/// pane's text wrapped, so the name is refused and only its visibly complete prefix is offered.
#[test]
fn grid_wraps_do_not_join_another_panes_text() {
    let name = format!("{} full.txt", native_path("tmp/report"));
    let name_cols = name.chars().count();
    let cols = u16::try_from(name_cols * 2 + 1).unwrap();
    // The right pane's text fills its row and wraps, so `-more` lands in the left pane's next row.
    let output = format!(
        "\x1b[?1049h{name}\u{2502}{}-more{}\u{2502}",
        "r".repeat(name_cols),
        " ".repeat(name_cols - 5)
    );
    let target = target_after(cols, 4, &output, 0, 3).unwrap();
    let ResolvedCellTarget::Path(key) = target.target else { panic!("path") };
    let displays = key.candidates.iter().map(PathProbeCandidate::display).collect::<Vec<_>>();
    assert_eq!(displays, [native_path("tmp/report")]);
}

/// A recorded wrap between rows that a split border divides joins no pane: the wrapped text is not
/// the right pane's own next row, so a URL that fills that pane stays cut and is refused.
#[test]
fn wraps_across_a_split_join_no_pane() {
    let mut output = format!("\x1b[?1049h{}", pane_border(21, 4));
    output.push_str("\x1b[1;22Hhttps://example.com/abcde");
    assert!(target_after(41, 4, &output, 0, 25).is_none());
}

/// A multiplexer relays only its active pane's directory, and the screen cannot show which pane
/// holds a relative path: a pane border may be box drawing, ASCII or blank, or look the same as a
/// program's own full-width rule. So on the alternate screen relative text gets no directory in
/// any pane, while an absolute path still resolves, and on the primary screen the same relative
/// path resolves against the reported directory.
#[test]
fn alternate_screen_relative_text_gets_no_directory() {
    let (osc7, relative) = if cfg!(windows) {
        ("\x1b]7;file:///C:/work/left\x1b\\", "src\\notes.txt")
    } else {
        ("\x1b]7;file:///work/left\x1b\\", "src/notes.txt")
    };
    // The cursor stays in the path's own pane beside each kind of border: box drawing, ASCII, and
    // a stacked split's full-width rule.
    let ascii_border: String = (1..=4).map(|row| format!("\x1b[{row};21H|")).collect();
    let stacked_rule = format!("\x1b[3;1H{}", "\u{2500}".repeat(41));
    for border in [pane_border(21, 4), ascii_border, stacked_rule] {
        let output = format!("{osc7}\x1b[?1049h{border}\x1b[1;1H{relative}");
        assert!(target_after(41, 4, &output, 0, 3).is_none());
    }
    let absolute = native_path("work/left/src/notes.txt");
    let output = format!("{osc7}\x1b[?1049h{}\x1b[1;1H{absolute}", pane_border(31, 4));
    let target = target_after(41, 4, &output, 0, 3).expect("an absolute path resolves");
    assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
    let primary = format!("{osc7}{relative}");
    let target = target_after(41, 4, &primary, 0, 3).expect("a primary-screen path resolves");
    let ResolvedCellTarget::Path(key) = target.target else {
        panic!("the relative path resolves to a local target");
    };
    assert!(key.candidates[0].resolved_path.ends_with("left/src/notes.txt"));
}

/// The production hover-state transform must forward explicit links to renderer inputs.
#[test]
fn hyperlink_hover_reaches_window_render_state() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["hover"]);
    let other = app.__test_seed_child_window(&["other"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        b"\x1b]8;;https://example.com/\x1b\\Docs\x1b]8;;\x1b\\"
    ));
    for active in [false, true, false] {
        app.windows.get_mut(&window).unwrap().modifiers = if !active {
            winit::keyboard::ModifiersState::empty()
        } else if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        let target = app.cell_target_at(window, pane, 0, 1);
        app.apply_target_hover(window, target);
        let hover = app.windows[&window].hovered_url.as_ref().expect("OSC 8 renderer input");
        assert_eq!(hover.cells.active, active);
        assert!(app.windows[&other].hovered_url.is_none());
    }
    app.apply_target_hover(window, None);
    assert!(app.windows[&window].hovered_url.is_none());
    assert!(!app.windows[&window].hover_link);
}

/// A busy parser is not proof that a hovered URL disappeared during a TUI output burst.
#[test]
fn busy_target_lookup_preserves_visuals_but_real_absence_clears_them() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["hover contention"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    let target = app.cell_target_at(window, pane, 0, 3);
    app.apply_target_hover(window, target);
    let before = app.windows[&window].hovered_url.clone();
    assert!(before.is_some());
    let parser = std::sync::Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let lookup = app.cell_target_lookup(window, pane, 0, 3);
    assert!(lookup.is_err());
    assert!(app.cell_target_at(window, pane, 0, 3).is_none());
    app.apply_target_lookup(window, lookup);
    assert_eq!(app.windows[&window].hovered_url, before);
    drop(guard);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\r\x1b[2Kplain"));
    let target = app.cell_target_at(window, pane, 0, 3);
    app.apply_target_hover(window, target);
    assert!(app.windows[&window].hovered_url.is_none());
}

/// Releasing the open modifier during contention cannot leave a recolored, clickable-looking target.
#[test]
fn busy_target_lookup_drops_active_feedback_when_modifier_is_released() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["hover modifier"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    let target = app.cell_target_at(window, pane, 0, 3);
    app.apply_target_hover(window, target);
    assert!(app.windows[&window].hovered_url.as_ref().unwrap().active());
    assert!(app.windows[&window].link_preview.is_some());
    let parser = std::sync::Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    app.windows.get_mut(&window).unwrap().modifiers = winit::keyboard::ModifiersState::empty();
    let lookup = app.cell_target_lookup(window, pane, 0, 3);
    app.apply_target_lookup(window, lookup);
    assert!(!app.windows[&window].hovered_url.as_ref().unwrap().active());
    assert!(app.windows[&window].link_preview.is_none());
    assert!(!app.windows[&window].hover_link);
    assert!(!app.activate_target_at(window, pane, 0, 3));
    drop(guard);
}

/// Busy hover never grants freshness; a coherent parser snapshot activates only the host modifier in either renderer mode.
#[test]
fn busy_hover_revalidates_host_modifier_for_main_and_child_renderers() {
    for degraded in [false, true] {
        for child in [false, true] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.software_render_degrade = degraded;
            let (window, pane) = if child {
                let window = app.__test_seed_child_window(&["hover"]);
                (window, app.__test_child_pane_ids(window).unwrap()[0])
            } else {
                let pane = app.__test_seed_tab("hover");
                (app.main_window_id.unwrap(), pane)
            };
            app.frontmost_window = Some(window);
            let parser = app.windows[&window].panes[&pane].parser.clone();
            parser.lock().advance(b"\x1b[?1049h\x1b[?1003h\x1b[?1006hhttps://example.com/");
            let target = app.cell_target_at(window, pane, 0, 3);
            app.apply_target_hover(window, target);
            assert!(!app.windows[&window].hovered_url.as_ref().unwrap().active());
            app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
                winit::keyboard::ModifiersState::SUPER
            } else {
                winit::keyboard::ModifiersState::CONTROL
            };
            let guard = parser.lock();
            let busy = app.cell_target_lookup(window, pane, 0, 3);
            assert!(busy.is_err());
            app.apply_target_lookup(window, busy);
            assert!(!app.windows[&window].hovered_url.as_ref().unwrap().active());
            assert!(!app.activate_target_at(window, pane, 0, 3));
            let target = app.cell_target_from_parser(window, pane, 0, 3, &guard, None);
            app.apply_target_hover(window, target);
            assert!(app.windows[&window].hovered_url.as_ref().unwrap().active());
            assert_eq!(
                app.windows[&window].link_preview.as_ref().unwrap().uri,
                "https://example.com/"
            );
            app.windows.get_mut(&window).unwrap().modifiers =
                winit::keyboard::ModifiersState::empty();
            let busy = app.cell_target_lookup(window, pane, 0, 3);
            app.apply_target_lookup(window, busy);
            assert!(!app.windows[&window].hovered_url.as_ref().unwrap().active());
            assert!(app.windows[&window].link_preview.is_none());
        }
    }
}

/// Frame collection revalidates retained visuals against the locked grid without recursively locking it.
#[test]
fn collected_parser_rejects_a_hover_after_content_changes() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["coherent hover"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    let target = app.cell_target_at(window, pane, 0, 3);
    app.apply_target_hover(window, target);
    let parser = std::sync::Arc::clone(&app.windows[&window].panes[&pane].parser);
    let mut guard = parser.lock();
    let lookup = app.cell_target_lookup(window, pane, 0, 3);
    app.apply_target_lookup(window, lookup);
    assert!(app.windows[&window].hovered_url.is_some());
    guard.advance(b"\r\x1b[2Kplain");
    let target = app.cell_target_from_parser(window, pane, 0, 3, &guard, None);
    app.apply_target_hover(window, target);
    assert!(app.windows[&window].hovered_url.is_none());
}

/// Parenthesized plain and OSC 8 URLs reach the same window hover renderer without underlining wrappers.
#[test]
fn parenthesized_url_hover_reaches_render_state() {
    for explicit in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["parentheses"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        let uri = "https://leetcode.com/";
        let output = if explicit {
            format!("LeetCode (\x1b]8;;{uri}\x1b\\{uri}\x1b]8;;\x1b\\)")
        } else {
            format!("LeetCode ({uri})")
        };
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        let target = app.cell_target_at(window, pane, 0, 15);
        app.apply_target_hover(window, target);
        let cells = app.windows[&window].hovered_url.as_ref().unwrap().cells;
        assert_eq!(
            (cells.spans()[0].start_col, cells.spans()[0].end_col),
            (10, 10 + uri.len() as u16)
        );
    }
}

/// The same destination resolves whole at every width, wrapped or not, from any fragment.
#[test]
fn plain_url_width_contract_resolves_full_destination() {
    let uri = "https://github.com/D0n9X1n/SonicTerm/issues/1349";
    let prefix_cols = "https://github.com/D0n9".len() as u16;
    for cols in [80, prefix_cols] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["URL width baseline"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, 8);
        assert!(app.__test_advance_child_pane_parser(window, pane, uri.as_bytes()));
        let wrapped = app.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(1)
            .unwrap()
            .soft_wrapped_from_previous();
        assert_eq!(wrapped, cols < uri.len() as u16);
        let rows = uri.len().div_ceil(usize::from(cols));
        let target = app.cell_target_at(window, pane, 0, 2).expect("URI first row");
        assert_eq!(target.display, uri);
        assert!(matches!(&target.target, ResolvedCellTarget::Uri(found) if found == uri));
        let hover = target.hovered(true).expect("URI highlight");
        assert_eq!(hover.cells.spans().len(), rows);
        if wrapped {
            let continuation =
                app.cell_target_at(window, pane, 1, 2).expect("URI continuation row");
            assert_eq!(continuation.display, uri);
        }
    }
}

/// Soft wraps join one label, but hard lines and same-row gaps separate equal hyperlink IDs.
#[test]
fn hyperlink_hover_preserves_occurrence_and_wide_cell_geometry() {
    use sonicterm_render_model::inputs::HoveredUrlSpan;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["wrapped"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(6, 10);
    let open = "\x1b]8;id=same;https://example.com/\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let output = format!("{open}ab界cdef{close}\r\n{open}xy{close} {open}zz{close}");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    for (row, col) in [(0, 2), (0, 3), (1, 1)] {
        let cells =
            app.cell_target_at(window, pane, row, col).unwrap().hovered(true).unwrap().cells;
        assert_eq!(
            cells.spans(),
            &[
                HoveredUrlSpan { row: 0, start_col: 0, end_col: 6 },
                HoveredUrlSpan { row: 1, start_col: 0, end_col: 2 },
            ]
        );
    }
    for (col, start, end) in [(0, 0, 2), (4, 3, 5)] {
        let cells = app.cell_target_at(window, pane, 2, col).unwrap().hovered(true).unwrap().cells;
        assert_eq!(cells.spans(), &[HoveredUrlSpan { row: 2, start_col: start, end_col: end }]);
    }
    let target = app.cell_target_at(window, pane, 0, 3);
    app.apply_target_hover(window, target);
    app.clear_target_hover(window);
    assert!(app.windows[&window].hovered_url.is_none());
}

/// Long visible labels retain the pointed row within the fixed renderer fragment budget.
#[test]
fn hyperlink_hover_bounds_long_labels_without_losing_pointer() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["long"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    // 144 cells on 4 columns span 36 rows, more than the renderer's fragment cap.
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(4, 40);
    let output = format!("\x1b]8;;https://example.com/\x1b\\{}\x1b]8;;\x1b\\", "x".repeat(144));
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let mut capped = false;
    for row in 0..36 {
        let cells = app.cell_target_at(window, pane, row, 2).unwrap().hovered(false).unwrap().cells;
        assert!(cells.contains(row, 2));
        assert!(cells.spans().len() <= MAX_WRAPPED_PATH_ROWS);
        capped |= cells.spans().len() == MAX_WRAPPED_PATH_ROWS;
    }
    assert!(capped, "an occurrence longer than the cap must fill it");
}

/// Quoted command paths survive automatic wraps while their quotes and surrounding words own no path.
#[test]
fn quoted_command_paths_preserve_app_spans_across_wraps() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["quoted command"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let cols = 40;
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, 8);
    let path = if cfg!(windows) {
        r"C:\work\OneDrive - Team\Promotion Analysis"
    } else {
        "/work/OneDrive - Team/Promotion Analysis"
    };
    let prefix = "python -m http.server --directory '";
    let text = format!("{prefix}{path}' stopped");
    assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
    for index in prefix.len()..prefix.len() + path.len() {
        let snapshot = app
            .cell_target_at(
                window,
                pane,
                (index / cols as usize) as u16,
                (index % cols as usize) as u16,
            )
            .unwrap();
        let ResolvedCellTarget::Path(key) = snapshot.target else { panic!("path probe required") };
        assert_eq!(key.candidates.len(), 1);
        assert_eq!(key.candidates[0].resolved_path, PathBuf::from(path));
        assert_eq!(key.candidates[0].display(), path);
        assert!(key.rows.len() > 1);
    }
    for index in [prefix.len() - 1, prefix.len() + path.len()] {
        assert!(app
            .cell_target_at(
                window,
                pane,
                (index / cols as usize) as u16,
                (index % cols as usize) as u16
            )
            .is_none());
    }
}

/// A grouped reference yields one probe, with all anchor cells in its authorization and selected location metadata.
#[test]
fn grouped_source_references_keep_one_probe_and_complete_identity() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["grouped source"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let cwd = if cfg!(windows) { "file:///C:/workspace" } else { "file:///workspace" };
    let text = "(src/main.rs:924, :934, :375).";
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        format!("\x1b]7;{cwd}\x1b\\{text}").as_bytes()
    ));
    for (needle, line) in [("src", 924), (":934", 934), (":375", 375)] {
        let col = text.find(needle).unwrap() as u16;
        let snapshot = app.cell_target_at(window, pane, 0, col).unwrap();
        let ResolvedCellTarget::Path(key) = snapshot.target else {
            panic!("source path probe required")
        };
        assert_eq!(key.candidates.len(), 1);
        assert!(
            matches!(&key.candidates[0].target, DetectedTarget::SourceReference(reference) if reference.line == line && reference.path == "src/main.rs")
        );
        assert_eq!(key.candidates[0].spans[0].start_col, 1);
        assert_eq!(key.candidates[0].display(), "src/main.rs:924, :934, :375");
    }
    for (col, character) in text.chars().enumerate() {
        if character == ',' || character == ' ' {
            assert!(app.cell_target_at(window, pane, 0, col as u16).is_none());
        }
    }
}

/// Structured paths pass real filesystem classification and stale quote edits revoke cached authorization.
#[test]
fn structured_paths_reach_filesystem_probe_and_reject_stale_identity() {
    let root = native_test_root().join(format!(
        "sonicterm-quoted-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let folder = root.join("My Folder");
    std::fs::create_dir_all(&folder).unwrap();
    let source = root.join("main.rs");
    std::fs::write(&source, b"fn main() {}\n").unwrap();
    for (text, needle, expected, decision) in [
        (
            format!("--directory '{}' stopped", folder.display()),
            "My Folder",
            folder.clone(),
            PathOpenDecision::Openable(PathKind::Directory),
        ),
        (
            format!("({}:12, :14).", source.display()),
            ":14",
            source.clone(),
            PathOpenDecision::SourceReveal,
        ),
    ] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["filesystem probe"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(240, 4);
        assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
        let col = text.find(needle).unwrap() as u16;
        let snapshot = app.cell_target_at(window, pane, 0, col).unwrap();
        let ResolvedCellTarget::Path(key) = snapshot.target else { panic!("path probe") };
        let selected = probe_candidates(&key.candidates, classify_local_target).unwrap();
        assert_eq!(selected.candidate.resolved_path, expected);
        assert_eq!(selected.decision, decision);
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result = PathProbeResult { request, selection: Some(selected), failure: None };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        assert!(app.windows[&window].path_probe.authorized(&key, true));
        assert!(app.__test_advance_child_pane_parser(window, pane, b"\r!"));
        let changed = app.cell_target_at(window, pane, 0, col);
        assert!(changed.is_none_or(|snapshot| match snapshot.target {
            ResolvedCellTarget::Path(next) =>
                !app.windows[&window].path_probe.authorized(&next, true),
            _ => true,
        }));
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Source locations resolve the filename against pane CWD, keeping the complete displayed reference.
#[test]
fn source_reference_resolves_and_underlines_full_location() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["source"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let cwd =
        if cfg!(target_os = "windows") { "file:///C:/workspace" } else { "file:///workspace" };
    let text = "install.sh:889–919";
    let output = format!("\x1b]7;{cwd}\x1b\\{text}");
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    for col in 0..text.chars().count() as u16 {
        let target = app.cell_target_at(window, pane, 0, col).unwrap();
        let ResolvedCellTarget::Path(key) = target.target else {
            panic!("source path provenance");
        };
        let candidate = key
            .candidates
            .iter()
            .find(|candidate| matches!(candidate.target, DetectedTarget::SourceReference(_)))
            .unwrap();
        assert_eq!(candidate.resolved_path.file_name().unwrap(), "install.sh");
        assert_eq!(candidate.display(), text);
        assert_eq!(
            candidate.visible_cells(pane, key.view_top, true).unwrap().spans()[0].end_col,
            text.chars().count() as u16
        );
    }
}

/// Executable text sources get only source-specific reveal authorization; generic opening stays blocked.
#[cfg(unix)]
#[test]
fn source_reference_reveal_never_authorizes_script_execution() {
    use std::os::unix::fs::PermissionsExt;
    let root = native_test_root().join(format!(
        "sonicterm-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let source = root.join("install.sh");
    std::fs::write(&source, b"#!/bin/sh\nprintf safe\\n").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(classify_source_reference(&source), PathOpenDecision::SourceReveal);
    assert_eq!(classify_local_target(&source), PathOpenDecision::Openable(PathKind::File));
    assert!(validate_reveal_target(&source, PathOpenDecision::SourceReveal).is_ok());
    assert_eq!(
        local_target_action(PathOpenDecision::SourceReveal),
        Some(LocalTargetAction::Reveal)
    );
    assert_eq!(classify_source_reference(&root.join("See Makefile")), PathOpenDecision::Missing);
    let link = root.join("link.sh");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    // A symlink to the source is followed and revealed like the source itself.
    assert_eq!(classify_source_reference(&link), PathOpenDecision::SourceReveal);
    std::fs::write(&source, b"\x7fELF\0binary").unwrap();
    assert_eq!(classify_source_reference(&source), PathOpenDecision::SourceReveal);
    assert!(validate_reveal_target(&source, PathOpenDecision::SourceReveal).is_ok());
    assert_eq!(classify_source_reference(&root.join("missing.sh")), PathOpenDecision::Missing);
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_dir(root).unwrap();
}

fn ascii_row(text: &str) -> Row {
    Row::from_flat(
        text.chars()
            .map(|character| {
                Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
            })
            .collect(),
    )
}

/// ASCII paths retain typed provenance and exact cell bounds.
#[test]
fn row_lookup_returns_path_candidate_with_cell_span() {
    let row = ascii_row("see ./file now");
    let found = target_at_row_cell(&row, 7, PathStyle::Posix).expect("path under column");

    assert_eq!(found.start_col, 4);
    assert_eq!(found.end_col, 10);
    assert_eq!(found.matched.target, DetectedTarget::PathCandidate("./file".into()));
}

/// Home-relative paths retain typed provenance and exact cell bounds before expansion.
#[test]
fn row_lookup_returns_home_relative_path_with_cell_span() {
    let row = ascii_row("see ~/notes now");
    let found = target_at_row_cell(&row, 7, PathStyle::Posix).expect("home path under column");

    assert_eq!(found.start_col, 4);
    assert_eq!(found.end_col, 11);
    assert_eq!(found.matched.target, DetectedTarget::PathCandidate("~/notes".into()));
}

/// Contextual `ls` names retain bare-name provenance and exact cell bounds.
#[test]
fn row_lookup_returns_contextual_bare_name_span() {
    let row = ascii_row("drwxr-xr-x user 18 Aug 12:30 sonicterm");
    let found = bare_target_at_row_cell(&row, 34, PathStyle::Posix)
        .expect("bare directory name under column");
    assert_eq!(found.start_col, 29);
    assert_eq!(found.end_col, 38);
    assert_eq!(found.matched.target, DetectedTarget::BareName("sonicterm".into()));
}

/// Contextual bare names require the exact pane's trusted local OSC 7 directory.
#[test]
fn bare_name_resolution_is_host_aware() {
    let target = DetectedTarget::BareName("sonicterm".into());
    let local = Osc7Cwd { authority: "localhost".into(), path: "/work".into() };
    assert_eq!(
        resolve_detected_path(&target, PathStyle::Posix, Some(&local), None, "host"),
        Some(PathBuf::from("/work/sonicterm"))
    );
    let foreign = Osc7Cwd { authority: "remote".into(), path: "/work".into() };
    assert_eq!(
        resolve_detected_path(&target, PathStyle::Posix, Some(&foreign), None, "host"),
        None
    );
    assert_eq!(resolve_detected_path(&target, PathStyle::Posix, None, None, "host"), None);

    let root = Osc7Cwd { authority: String::new(), path: "/".into() };
    assert_eq!(
        resolve_detected_path(&target, PathStyle::Posix, Some(&root), None, "host"),
        Some(PathBuf::from("/sonicterm"))
    );
}

/// App lookup expands a home-relative row token before constructing its probe key.
#[test]
fn app_cell_lookup_resolves_home_relative_paths() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (home, candidate, expected) = if cfg!(target_os = "windows") {
        (PathBuf::from(r"C:\Users\tester"), r"~\notes", PathBuf::from(r"C:\Users\tester\notes"))
    } else {
        (PathBuf::from("/Users/tester"), "~/notes", PathBuf::from("/Users/tester/notes"))
    };
    app.home_dir = Some(home);
    let window = app.__test_seed_child_window(&["home"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    assert!(app.__test_advance_child_pane_parser(window, pane, candidate.as_bytes()));

    let resolved = app.cell_target_at(window, pane, 0, 2).expect("home-relative target");
    assert!(matches!(
        resolved.target,
        ResolvedCellTarget::Path(ref key)
            if key.candidates.iter().any(|probe| {
                probe.display() == candidate && probe.resolved_path == expected
            })
    ));
}

/// App lookup resolves a separator-relative row token from the clicked pane's OSC 7 CWD.
#[test]
fn app_cell_lookup_resolves_contextual_relative_paths() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (osc7, candidate, expected) = if cfg!(target_os = "windows") {
        (
            b"\x1b]7;file:///C:/work/project\x1b\\".as_slice(),
            r"src\main.rs",
            PathBuf::from(r"C:\work\project\src\main.rs"),
        )
    } else {
        (
            b"\x1b]7;file:///work/project\x1b\\".as_slice(),
            "src/main.rs",
            PathBuf::from("/work/project/src/main.rs"),
        )
    };
    let window = app.__test_seed_child_window(&["relative"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let mut output = osc7.to_vec();
    output.extend_from_slice(candidate.as_bytes());
    assert!(app.__test_advance_child_pane_parser(window, pane, &output));

    let resolved = app.cell_target_at(window, pane, 0, 2).expect("separator-relative target");
    assert!(matches!(
        resolved.target,
        ResolvedCellTarget::Path(ref key)
            if key.candidates.iter().any(|probe| {
                probe.display() == candidate && probe.resolved_path == expected
            })
    ));
}

/// Real app lookup resolves one trusted relative path from either automatic-wrap fragment.
#[test]
fn app_cell_lookup_resolves_soft_wrapped_relative_path_from_each_fragment() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["wrapped"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows
        .get(&window)
        .unwrap()
        .panes
        .get(&pane)
        .unwrap()
        .parser
        .lock()
        .grid_mut()
        .resize(9, 3);
    let (output, display, expected) = if cfg!(target_os = "windows") {
        (
            b"\x1b]7;file:///C:/work/project\x1b\\src\\long\\path.rs".as_slice(),
            r"src\long\path.rs",
            PathBuf::from(r"C:\work\project\src\long\path.rs"),
        )
    } else {
        (
            b"\x1b]7;file:///work/project\x1b\\src/long/path.rs".as_slice(),
            "src/long/path.rs",
            PathBuf::from("/work/project/src/long/path.rs"),
        )
    };
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    let expected_spans = [
        AbsoluteCellSpan { row: 0, start_col: 0, end_col: 9 },
        AbsoluteCellSpan {
            row: 1,
            start_col: 0,
            end_col: u16::try_from(display.chars().count() - 9).unwrap(),
        },
    ];

    for (row, col) in [(0, 2), (1, 2)] {
        let snapshot = app.cell_target_at(window, pane, row, col).expect("wrapped path target");
        let ResolvedCellTarget::Path(key) = snapshot.target else {
            panic!("wrapped relative path must require a filesystem probe")
        };
        let candidate = key
            .candidates
            .iter()
            .find(|candidate| candidate.display() == display)
            .expect("complete wrapped candidate");
        assert_eq!(candidate.spans.as_slice(), expected_spans);
        assert_eq!(candidate.resolved_path, expected);
        assert_eq!(key.rows.len(), 2);
    }
}

/// App lookup resolves a shell-quoted spaced `ll` name from the exact pane CWD.
#[test]
fn app_cell_lookup_resolves_shell_quoted_spaced_name() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let row = "drwxr-xr-x@ - d0n9x1n 23 Aug 21:54 'ff ff'";
    let name_start = row.find("ff ff").unwrap();
    let name_start_col = row[..name_start].chars().count();
    let window = app.__test_seed_child_window(&["quoted"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let (osc7, expected) = if cfg!(target_os = "windows") {
        (
            b"\x1b]7;file:///C:/Users/d0n9x1n\x1b\\".as_slice(),
            PathBuf::from(r"C:\Users\d0n9x1n\ff ff"),
        )
    } else {
        (b"\x1b]7;file:///Users/d0n9x1n\x1b\\".as_slice(), PathBuf::from("/Users/d0n9x1n/ff ff"))
    };
    let mut output = osc7.to_vec();
    output.extend_from_slice(row.as_bytes());
    assert!(app.__test_advance_child_pane_parser(window, pane, &output));

    for col in name_start_col..name_start_col + "ff ff".chars().count() {
        let resolved = app
            .cell_target_at(window, pane, 0, u16::try_from(col).unwrap())
            .expect("shell-quoted contextual target");
        assert!(matches!(
            resolved.target,
            ResolvedCellTarget::Path(ref key)
                if key.candidates.iter().any(|probe| {
                    probe.display() == "ff ff"
                        && probe.spans.len() == 1
                        && usize::from(probe.spans[0].start_col) == name_start_col
                        && usize::from(probe.spans[0].end_col)
                            == name_start_col + "ff ff".chars().count()
                        && probe.resolved_path == expected
                })
        ));
    }
}

/// App lookup resolves a spaced name from the exact pane CWD across every cell in its span.
#[test]
fn app_cell_lookup_resolves_spaced_contextual_names() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (osc7, candidate, expected) = if cfg!(target_os = "windows") {
        (
            b"\x1b]7;file:///C:/Users/tester\x1b\\".as_slice(),
            "OneDrive - Microsoft",
            PathBuf::from(r"C:\Users\tester\OneDrive - Microsoft"),
        )
    } else {
        (
            b"\x1b]7;file:///home/tester\x1b\\".as_slice(),
            "My Folder",
            PathBuf::from("/home/tester/My Folder"),
        )
    };
    let window = app.__test_seed_child_window(&["spaced"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let mut output = osc7.to_vec();
    output.extend_from_slice(candidate.as_bytes());
    assert!(app.__test_advance_child_pane_parser(window, pane, &output));

    for col in 0..candidate.chars().count() {
        let resolved = app
            .cell_target_at(window, pane, 0, u16::try_from(col).unwrap())
            .expect("spaced contextual target");
        assert!(matches!(
            resolved.target,
            ResolvedCellTarget::Path(ref key)
                if key.candidates.iter().any(|probe| {
                    probe.display() == candidate
                        && probe.spans.len() == 1
                        && probe.spans[0].start_col == 0
                        && usize::from(probe.spans[0].end_col) == candidate.chars().count()
                        && probe.resolved_path == expected
                })
        ));
    }
}

/// App lookup resolves a bare row token from only the clicked pane's local OSC 7 state.
#[test]
fn app_cell_lookup_uses_exact_pane_osc7_for_bare_names() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let local_window = app.__test_seed_child_window(&["local"]);
    let local_pane = app.__test_child_pane_ids(local_window).unwrap()[0];
    let (local_osc7, expected) = if cfg!(target_os = "windows") {
        (
            b"\x1b]7;file:///C:/tmp/work\x1b\\sonicterm".as_slice(),
            PathBuf::from(r"C:\tmp\work\sonicterm"),
        )
    } else {
        (b"\x1b]7;file:///tmp/work\x1b\\sonicterm".as_slice(), PathBuf::from("/tmp/work/sonicterm"))
    };
    assert!(app.__test_advance_child_pane_parser(local_window, local_pane, local_osc7));

    let resolved = app.cell_target_at(local_window, local_pane, 0, 2).expect("local bare target");
    assert!(matches!(
        resolved.target,
        ResolvedCellTarget::Path(ref key)
            if key.candidates.iter().any(|probe| {
                probe.display() == "sonicterm" && probe.resolved_path == expected
            })
    ));

    let foreign_window = app.__test_seed_child_window(&["foreign"]);
    let foreign_pane = app.__test_child_pane_ids(foreign_window).unwrap()[0];
    let foreign_osc7 = if cfg!(target_os = "windows") {
        b"\x1b]7;file://remote-host/C:/tmp/work\x1b\\sonicterm".as_slice()
    } else {
        b"\x1b]7;file://remote-host/tmp/work\x1b\\sonicterm".as_slice()
    };
    assert!(app.__test_advance_child_pane_parser(foreign_window, foreign_pane, foreign_osc7));
    assert!(app.cell_target_at(foreign_window, foreign_pane, 0, 2).is_none());
}

/// Spaced paths map scanner byte ranges back to one exact grid-cell span.
#[test]
fn row_candidates_preserve_complete_spaced_path_span() {
    for (style, text) in [
        (PathStyle::Windows, r"C:\Program Files\SonicTerm"),
        (PathStyle::Posix, "/tmp/My Folder"),
        (PathStyle::Posix, "My Folder"),
    ] {
        for col in 0..text.chars().count() {
            let candidates = row_target_candidates_at_cell(
                &ascii_row(text),
                u16::try_from(col).unwrap(),
                style,
                true,
            );
            assert!(
                candidates.iter().any(|candidate| {
                    candidate.start_col == 0
                        && usize::from(candidate.end_col) == text.chars().count()
                        && match &candidate.matched.target {
                            DetectedTarget::PathCandidate(value)
                            | DetectedTarget::BareName(value) => value == text,
                            DetectedTarget::Uri(_) | DetectedTarget::SourceReference(_) => false,
                        }
                }),
                "missing full row span at {col} in {text:?}: {candidates:?}"
            );
        }
    }
}

/// A proven automatic wrap reconstructs one complete target with ordered absolute spans.
#[test]
fn logical_path_scan_reconstructs_two_wrapped_rows_from_each_fragment() {
    let mut grid = Grid::new(9, 3);
    for character in "src/long/path.rs".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    let expected = [
        AbsoluteCellSpan { row: 0, start_col: 0, end_col: 9 },
        AbsoluteCellSpan { row: 1, start_col: 0, end_col: 7 },
    ];

    for pointed in [AbsoluteCell { row: 0, col: 2 }, AbsoluteCell { row: 1, col: 2 }] {
        let scan = logical_path_scan_at_cell(&grid, 0, pointed, PathStyle::Posix, false)
            .expect("complete wrapped path");
        let candidate = scan
            .candidates
            .iter()
            .find(|candidate| {
                candidate.target == DetectedTarget::PathCandidate("src/long/path.rs".into())
            })
            .expect("complete separator-relative candidate");
        assert_eq!(candidate.spans.as_slice(), expected);
        assert_eq!(scan.rows.len(), 2);
    }
}

/// Identical row fragments separated by a hard linefeed never become one path.
#[test]
fn logical_path_scan_never_joins_hard_newlines() {
    let mut grid = Grid::new(9, 3);
    for character in "src/long/".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.linefeed();
    grid.carriage_return();
    for character in "path.rs".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }

    assert!(logical_path_scan_at_cell(
        &grid,
        0,
        AbsoluteCell { row: 1, col: 2 },
        PathStyle::Posix,
        false,
    )
    .is_none());
}

/// A chain of exactly `MAX_WRAPPED_PATH_ROWS` rows is accepted, while one more row fails closed.
#[test]
fn logical_path_scan_enforces_row_bound() {
    for (rows, accepted) in [(MAX_WRAPPED_PATH_ROWS, true), (MAX_WRAPPED_PATH_ROWS + 1, false)] {
        let mut grid = Grid::new(2, u16::try_from(rows).unwrap());
        // `rows` - 1 two-cell `a/` rows, then a final `h` row, as a 2-column grid wraps them.
        for character in format!("{}h", "a/".repeat(rows - 1)).chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        let pointed = AbsoluteCell { row: u64::try_from(rows - 1).unwrap(), col: 0 };
        let scan = logical_path_scan_at_cell(&grid, 0, pointed, PathStyle::Posix, false);
        assert_eq!(scan.is_some(), accepted, "rows {rows}");
    }
}

/// Offscreen or evicted wrapped predecessors never expose a partial target.
#[test]
fn logical_path_scan_rejects_incomplete_visible_or_evicted_chain() {
    let mut offscreen = Grid::new(4, 3);
    for character in "src/path.rs".chars() {
        offscreen.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert!(logical_path_scan_at_cell(
        &offscreen,
        1,
        AbsoluteCell { row: 1, col: 1 },
        PathStyle::Posix,
        false,
    )
    .is_none());

    let mut evicted = Grid::new(4, 2);
    evicted.set_scrollback_limit(1);
    for character in "src/long/path.rs".chars() {
        evicted.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert!(evicted.scrollback_evicted() > 0);
    assert!(evicted.row_at_abs(0).unwrap().soft_wrapped_from_previous());
    assert!(logical_path_scan_at_cell(
        &evicted,
        0,
        AbsoluteCell { row: 0, col: 1 },
        PathStyle::Posix,
        false,
    )
    .is_none());
}

/// Wide outer punctuation and wrappers never enter the path span, even beside Chinese prose or across soft rows.
#[test]
fn structural_boundary_cells_preserve_exact_ascii_path() {
    let path = "reports/flight.html";
    for (left, right) in [("(", ")"), ("【", "】"), ("“", "”"), ("'", "'")] {
        for tail in ["，正文", ",next", "。", "，「引用」"] {
            for width in [100, 17] {
                let text = format!("前文 {left}{path}{right}{tail}");
                let mut grid = Grid::new(width, 8);
                for character in text.chars() {
                    grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
                }
                let cells = grid
                    .rows_iter()
                    .enumerate()
                    .flat_map(|(row, line)| {
                        line.iter().enumerate().map(move |(col, cell)| {
                            (AbsoluteCell { row: row as u64, col: col as u16 }, cell.ch)
                        })
                    })
                    .collect::<Vec<_>>();
                let begin = cells.iter().position(|(_, character)| *character == 'r').unwrap();
                for index in begin..begin + path.len() {
                    let pointed = cells[index].0;
                    let scan =
                        logical_path_scan_at_cell(&grid, 0, pointed, PathStyle::native(), true)
                            .unwrap_or_else(|| panic!("{text} width {width} at {pointed:?}"));
                    let candidate = scan
                        .candidates
                        .iter()
                        .find(|candidate| {
                            candidate.target == DetectedTarget::PathCandidate(path.into())
                        })
                        .unwrap();
                    assert!(candidate.spans.iter().any(|span| span.contains(pointed)));
                    assert_eq!(
                        candidate.spans.iter().copied().map(AbsoluteCellSpan::len).sum::<usize>(),
                        path.len()
                    );
                    for outside in [cells[begin - 1].0, cells[begin + path.len()].0] {
                        assert!(!candidate.spans.iter().any(|span| span.contains(outside)));
                    }
                }
            }
        }
    }
}

/// Real-file probes, hover, and click authorization agree across windows, and CWD changes revoke the decision.
#[test]
fn structural_boundaries_resolve_real_files_and_revoke_changed_cwd() {
    let root = native_test_root().join(format!(
        "sonicterm-structure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join("reports")).unwrap();
    for name in ["flight.html", "flight.md", "flight.html,"] {
        std::fs::write(root.join("reports").join(name), "inert fixture").unwrap();
    }
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", root.to_str().unwrap().replace('\\', "/")))
    } else {
        root.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main boundaries");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child boundaries"]);
    for window in [main, child] {
        let pane = *app.windows[&window].panes.keys().next().unwrap();
        app.frontmost_window = Some(window);
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        app.windows[&window].panes[&pane].parser.lock().resize(120, 8);
        for (text, path, col) in [
            ("前文 【reports/flight.html】，正文", "reports/flight.html", 8),
            ("前文 (reports/flight.md)，正文", "reports/flight.md", 8),
            ("(reports/flight.html,)，正文", "reports/flight.html", 4),
        ] {
            let output = format!("\x1b[2J\x1b[H\x1b]7;{cwd}\x1b\\{text}");
            assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
            let snapshot = app.cell_target_at(window, pane, 0, col).unwrap();
            let ResolvedCellTarget::Path(key) = snapshot.target.clone() else {
                panic!("path target")
            };
            let selection = probe_candidates(&key.candidates, classify_local_target).unwrap();
            assert_eq!(selection.candidate.resolved_path, root.join(path));
            let request =
                app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
            let result = PathProbeResult { request, selection: Some(selection), failure: None };
            assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
            app.apply_target_hover(window, Some(snapshot));
            assert_eq!(
                PathBuf::from(&app.windows[&window].link_preview.as_ref().unwrap().uri),
                root.join(path)
            );
            assert!(app.windows[&window].path_probe.authorized(&key, true));
            let change = format!("\x1b]7;{cwd}/other\x1b\\");
            assert!(app.__test_advance_child_pane_parser(window, pane, change.as_bytes()));
            let changed = app.cell_target_at(window, pane, 0, col).unwrap();
            let ResolvedCellTarget::Path(changed_key) = changed.target else {
                panic!("changed path")
            };
            assert!(!app.windows[&window].path_probe.authorized(&changed_key, true));
            assert!(!app
                .windows
                .get_mut(&window)
                .unwrap()
                .path_probe
                .accept(&result, Some(&changed_key)));
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Composite quote wrappers retain every source delimiter in authorization, including removed outer punctuation.
#[test]
fn structural_quoted_wrappers_reject_unsafe_outer_cells() {
    let text = "(\"./safe.rs\").";
    for index in [0, text.len() - 2, text.len() - 1] {
        for hyperlink in [false, true] {
            let mut grid = Grid::new(80, 4);
            for character in text.chars() {
                grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
            let cell = grid.row_mut(0).iter_mut().nth(index).unwrap();
            if hyperlink {
                cell.set_hyperlink(Some(HyperlinkId(7)));
            } else {
                cell.set_extras(Some("\u{301}".into()));
            }
            let scan = logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 5 },
                PathStyle::native(),
                true,
            );
            assert!(scan.is_none(), "unsafe outer cell {index}, hyperlink={hyperlink}");
        }
    }
}

/// URI scanning cannot discard unsafe wrapper cells while preserving the inner destination.
#[test]
fn structural_uri_wrappers_reject_unsafe_source_cells() {
    let text = "(https://example.com/a).";
    for index in [0, text.len() - 2, text.len() - 1] {
        for hyperlink in [false, true] {
            let mut grid = Grid::new(80, 4);
            for character in text.chars() {
                grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
            let cell = grid.row_mut(0).iter_mut().nth(index).unwrap();
            if hyperlink {
                cell.set_hyperlink(Some(HyperlinkId(7)));
            } else {
                cell.set_extras(Some("\u{301}".into()));
            }
            assert!(logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 6 },
                PathStyle::native(),
                true
            )
            .is_none());
        }
    }
}

/// Every cell establishing a wide boundary remains protected from combining, hyperlink, and broken-pair mutations.
#[test]
fn structural_boundary_cells_reject_unsafe_source_identity() {
    let text = "【src/main.rs】，正文";
    for protected in [0, 1, 13, 14, 15, 16] {
        for mutation in [0, 1, 2] {
            let mut grid = Grid::new(80, 4);
            for character in text.chars() {
                grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
            let cell = grid.row_mut(0).iter_mut().nth(protected).unwrap();
            match mutation {
                0 => cell.set_hyperlink(Some(HyperlinkId(7))),
                1 => cell.set_extras(Some("\u{301}".into())),
                _ => cell.flags.toggle(CellFlags::WIDE_CONT),
            }
            let scan = logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 5 },
                PathStyle::native(),
                true,
            );
            assert!(
                scan.is_none_or(|path_scan| path_scan
                    .candidates
                    .iter()
                    .all(|candidate| candidate.target
                        != DetectedTarget::PathCandidate("src/main.rs".into()))),
                "protected {protected} mutation {mutation}"
            );
        }
    }
}

/// Unsafe identity in any wrapped fragment rejects the complete candidate.
#[test]
fn logical_path_scan_rejects_cross_row_combining_and_osc8_cells() {
    for hyperlink in [false, true] {
        let mut grid = Grid::new(9, 3);
        for character in "src/long/path.rs".chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        let cell = grid.row_mut(1).iter_mut().nth(2).unwrap();
        if hyperlink {
            cell.set_hyperlink(Some(HyperlinkId(9)));
        } else {
            cell.set_extras(Some("\u{301}".into()));
        }
        assert!(logical_path_scan_at_cell(
            &grid,
            0,
            AbsoluteCell { row: 0, col: 2 },
            PathStyle::Posix,
            false,
        )
        .is_none());
    }
}

/// Final-row prose punctuation retains literal-first and fully trimmed candidates across a wrap.
#[test]
fn logical_path_scan_keeps_wrapped_prose_punctuation_alternates() {
    let mut grid = Grid::new(9, 3);
    for character in "src/long/path.rs,.".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    let scan = logical_path_scan_at_cell(
        &grid,
        0,
        AbsoluteCell { row: 0, col: 2 },
        PathStyle::Posix,
        false,
    )
    .expect("wrapped punctuation candidates");

    assert!(scan.candidates.iter().any(|candidate| {
        candidate.target == DetectedTarget::PathCandidate("src/long/path.rs,.".into())
    }));
    assert!(scan.candidates.iter().any(|candidate| {
        candidate.target == DetectedTarget::PathCandidate("src/long/path.rs".into())
            && candidate.spans.last().is_some_and(|span| span.end_col == 7)
    }));
}

/// Automatic wrapping preserves the source span but hard line breaks never reconstruct it.
#[test]
fn punctuated_wrapped_paths_follow_only_automatic_rows() {
    let text = "(src/long/path.rs:97).";
    let display = "src/long/path.rs:97";
    let width = 11;
    let mut grid = Grid::new(width, 3);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    for index in 1..1 + display.len() {
        let scan = logical_path_scan_at_cell(
            &grid,
            0,
            AbsoluteCell {
                row: (index / width as usize) as u64,
                col: (index % width as usize) as u16,
            },
            PathStyle::Posix,
            false,
        )
        .unwrap();
        let candidate = scan.candidates.iter().find(|candidate| matches!(&candidate.target,
            DetectedTarget::SourceReference(reference) if reference.display == display && reference.path == "src/long/path.rs")).expect("complete source across automatic rows");
        assert_eq!(
            candidate.spans.as_slice(),
            &[
                AbsoluteCellSpan { row: 0, start_col: 1, end_col: width },
                AbsoluteCellSpan {
                    row: 1,
                    start_col: 0,
                    end_col: (1 + display.len() - width as usize) as u16
                },
            ]
        );
    }
    let mut hard = Grid::new(width, 3);
    for character in "(src/long/".chars() {
        hard.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    hard.linefeed();
    hard.carriage_return();
    for character in "path.rs:97).".chars() {
        hard.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    for pointed in [AbsoluteCell { row: 0, col: 2 }, AbsoluteCell { row: 1, col: 2 }] {
        if let Some(scan) = logical_path_scan_at_cell(&hard, 0, pointed, PathStyle::Posix, false) {
            assert!(scan.candidates.iter().all(|candidate| !matches!(&candidate.target,
                DetectedTarget::SourceReference(reference) if reference.display == display)));
        }
    }
}

/// Quote delimiters and inherited citation anchors remain inside unsafe-cell authorization checks.
#[test]
fn structured_paths_reject_unsafe_delimiters_and_anchors() {
    for (text, pointed, protected) in [
        ("'/tmp/My Folder'", 3, vec![0, 14]),
        ("\"/tmp/My Folder\"", 3, vec![0, 14]),
        ("`/tmp/My Folder`", 3, vec![0, 14]),
        ("(src/main.rs:12, :14)", 17, vec![0, 3, 18]),
    ] {
        for index in protected {
            for hyperlink in [false, true] {
                let mut grid = Grid::new(11, 4);
                for character in text.chars() {
                    grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
                }
                let cell = grid.row_mut((index / 11) as u16).iter_mut().nth(index % 11).unwrap();
                if hyperlink {
                    cell.set_hyperlink(Some(HyperlinkId(7)));
                } else {
                    cell.set_extras(Some("\u{301}".into()));
                }
                assert!(
                    logical_path_scan_at_cell(
                        &grid,
                        0,
                        AbsoluteCell { row: pointed / 11, col: (pointed % 11) as u16 },
                        PathStyle::Posix,
                        true
                    )
                    .is_none(),
                    "{text} protected {index}"
                );
            }
        }
    }
}

/// Removed wrapper and punctuation cells still retain unsafe combining and hyperlink provenance.
#[test]
fn punctuated_wrapped_paths_reject_unsafe_removed_cells() {
    let text = "(src/main.rs:97).";
    for index in [0, text.len() - 2, text.len() - 1] {
        let mut cells = text
            .chars()
            .map(|character| {
                Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
            })
            .collect::<Vec<_>>();
        cells[index].set_extras(Some("\u{301}".into()));
        assert!(row_target_candidates_at_cell(&Row::from_flat(cells), 3, PathStyle::Posix, true)
            .is_empty());
        let mut row = ascii_row(text);
        row.iter_mut().nth(index).unwrap().set_hyperlink(Some(HyperlinkId(7)));
        assert!(row_target_candidates_at_cell(&row, 3, PathStyle::Posix, true).is_empty());
        for hyperlink in [false, true] {
            let mut grid = Grid::new(11, 3);
            for character in text.chars() {
                grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
            }
            let cell = grid.row_mut((index / 11) as u16).iter_mut().nth(index % 11).unwrap();
            if hyperlink {
                cell.set_hyperlink(Some(HyperlinkId(7)));
            } else {
                cell.set_extras(Some("\u{301}".into()));
            }
            assert!(logical_path_scan_at_cell(
                &grid,
                0,
                AbsoluteCell { row: 0, col: 3 },
                PathStyle::Posix,
                true
            )
            .is_none());
        }
    }
}

/// Hyperlink provenance inside a candidate prevents plain-text path reconstruction.
#[test]
fn row_candidates_do_not_cross_osc8_cells() {
    let mut row = ascii_row("/tmp/My Folder");
    row.iter_mut().nth(6).unwrap().set_hyperlink(Some(HyperlinkId(7)));
    assert!(row_target_candidates_at_cell(&row, 2, PathStyle::Posix, true)
        .iter()
        .all(|candidate| candidate.end_col <= 6));
}

/// A wide continuation is token content, not whitespace that can expose a suffix path.
#[test]
fn wide_cell_rejects_the_entire_surrounding_path_token() {
    let mut cells = "/tmp/"
        .chars()
        .map(|character| Cell::plain(character, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells.push(Cell::plain('界', Color::Default, Color::Default, CellFlags::WIDE));
    cells.push(Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT));
    cells.extend("/file".chars().map(|character| {
        Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
    }));
    let row = Row::from_flat(cells);

    assert!(target_at_row_cell(&row, 1, PathStyle::Posix).is_none());
    assert!(target_at_row_cell(&row, 8, PathStyle::Posix).is_none());
}

/// Combining extras reject a whole token instead of exposing an ASCII fragment.
#[test]
fn combining_extras_reject_the_entire_surrounding_path_token() {
    let mut cells = "./cafe/file"
        .chars()
        .map(|character| Cell::plain(character, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells[5].set_extras(Some("\u{301}".into()));
    let row = Row::from_flat(cells);

    assert!(target_at_row_cell(&row, 2, PathStyle::Posix).is_none());
    assert!(target_at_row_cell(&row, 8, PathStyle::Posix).is_none());
}

/// A prose fallback remains inert when removed punctuation carries combining identity.
#[test]
fn trimmed_punctuation_cannot_bypass_combining_cell_rejection() {
    let mut cells = "src/main.rs,"
        .chars()
        .map(|character| Cell::plain(character, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells.last_mut().unwrap().set_extras(Some("\u{301}".into()));
    let row = Row::from_flat(cells);

    assert!(row_target_candidates_at_cell(&row, 2, PathStyle::Posix, true).is_empty());
}

/// A prose fallback remains inert when removed punctuation carries OSC 8 identity.
#[test]
fn trimmed_punctuation_cannot_bypass_osc8_cell_rejection() {
    let mut row = ascii_row("src/main.rs,");
    row.iter_mut().last().unwrap().set_hyperlink(Some(HyperlinkId(7)));

    assert!(row_target_candidates_at_cell(&row, 2, PathStyle::Posix, true).is_empty());
}

/// POSIX relative paths resolve only from an accepted local OSC 7 snapshot.
#[test]
fn posix_relative_resolution_is_host_aware_and_root_clamped() {
    let cwd = Osc7Cwd { authority: "localhost".into(), path: "/work/project".into() };
    assert_eq!(
        resolve_path_candidate("../../file", PathStyle::Posix, Some(&cwd), None, "my-host"),
        Some(PathBuf::from("/file"))
    );

    let foreign = Osc7Cwd { authority: "remote-host".into(), path: "/work/project".into() };
    assert_eq!(
        resolve_path_candidate("./file", PathStyle::Posix, Some(&foreign), None, "my-host"),
        None
    );
    assert_eq!(resolve_path_candidate("./file", PathStyle::Posix, None, None, "my-host"), None);
}

/// Separator-relative paths resolve only from the exact pane's trusted local OSC 7 directory.
#[test]
fn contextual_relative_resolution_is_host_aware() {
    let local = Osc7Cwd { authority: "localhost".into(), path: "/work/project".into() };
    assert_eq!(
        resolve_path_candidate("src/main.rs", PathStyle::Posix, Some(&local), None, "host"),
        Some(PathBuf::from("/work/project/src/main.rs"))
    );
    let foreign = Osc7Cwd { authority: "remote".into(), path: "/work/project".into() };
    assert_eq!(
        resolve_path_candidate("src/main.rs", PathStyle::Posix, Some(&foreign), None, "host"),
        None
    );
    assert_eq!(resolve_path_candidate("src/main.rs", PathStyle::Posix, None, None, "host"), None);

    let windows = Osc7Cwd { authority: String::new(), path: "/C:/work/project".into() };
    assert_eq!(
        resolve_path_candidate(r"src\main.rs", PathStyle::Windows, Some(&windows), None, "host"),
        Some(PathBuf::from(r"C:\work\project\src\main.rs"))
    );
}

/// Current-home paths use only a validated native absolute home and normalize below its root.
#[test]
fn home_relative_resolution_uses_valid_native_home() {
    assert_eq!(
        resolve_path_candidate(
            "~/work/../notes",
            PathStyle::Posix,
            None,
            Some(Path::new("/Users/tester")),
            "host",
        ),
        Some(PathBuf::from("/Users/tester/notes"))
    );
    assert_eq!(
        resolve_path_candidate(
            "~/../../notes",
            PathStyle::Posix,
            None,
            Some(Path::new("/Users/tester")),
            "host",
        ),
        Some(PathBuf::from("/notes"))
    );
    assert_eq!(
        resolve_path_candidate(
            r"~\work\..\notes",
            PathStyle::Windows,
            None,
            Some(Path::new(r"C:\Users\tester")),
            "host",
        ),
        Some(PathBuf::from(r"C:\Users\tester\notes"))
    );
    assert_eq!(
        resolve_path_candidate(
            "~/notes",
            PathStyle::Posix,
            None,
            Some(Path::new("relative/home")),
            "host",
        ),
        None
    );
    assert_eq!(
        resolve_path_candidate(r"~\file", PathStyle::Windows, None, Some(Path::new("C:")), "host",),
        None
    );
    assert_eq!(resolve_path_candidate("~/notes", PathStyle::Posix, None, None, "host"), None);
}

/// The exact local hostname is accepted case-insensitively without weakening foreign-host checks.
#[test]
fn local_hostname_authority_is_accepted() {
    let cwd = Osc7Cwd { authority: "MY-HOST".into(), path: "/work".into() };
    assert_eq!(
        resolve_path_candidate("./file", PathStyle::Posix, Some(&cwd), None, "my-host"),
        Some(PathBuf::from("/work/file"))
    );
}

/// Windows OSC 7 drive paths normalize before dot-relative resolution.
#[test]
fn windows_relative_resolution_normalizes_drive_form() {
    let cwd = Osc7Cwd { authority: String::new(), path: "/C:/work/project".into() };
    assert_eq!(
        resolve_path_candidate(r"..\..\file", PathStyle::Windows, Some(&cwd), None, "host"),
        Some(PathBuf::from(r"C:\file"))
    );
    assert_eq!(
        resolve_path_candidate("C:/Users/dotan/", PathStyle::Windows, None, None, "host"),
        Some(PathBuf::from(r"C:\Users\dotan"))
    );
    assert_eq!(
        resolve_path_candidate(r"C:\Users\bad.\name", PathStyle::Windows, None, None, "host"),
        None
    );
    assert_eq!(
        resolve_path_candidate(r"C:\Users\bad \name", PathStyle::Windows, None, None, "host"),
        None
    );
}

/// URI, explicit-path, and contextual-bare provenance obey independent kill-switch precedence.
#[test]
fn local_target_switches_do_not_change_uri_behavior() {
    let uri = DetectedTarget::Uri("https://example.com".into());
    let explicit = DetectedTarget::PathCandidate("./file".into());
    let bare = DetectedTarget::BareName("file".into());

    for local in [false, true] {
        for contextual in [false, true] {
            assert!(detected_target_enabled(&uri, local, contextual));
            assert_eq!(detected_target_enabled(&explicit, local, contextual), local);
            assert_eq!(detected_target_enabled(&bare, local, contextual), local && contextual);
        }
    }
}

/// Probe epochs wrap rather than saturate so identity can never stick at u64::MAX.
#[test]
fn probe_epoch_advances_and_wraps() {
    assert_eq!(ProbeEpoch::INITIAL.next(), ProbeEpoch(1));
    assert_eq!(ProbeEpoch(u64::MAX).next(), ProbeEpoch::INITIAL);
}

/// macOS direct-open and reveal use fixed argv with an explicit option terminator.
#[test]
fn macos_open_specs_keep_targets_out_of_options() {
    let open = macos_open_spec("/tmp/file/").expect("valid absolute path");
    assert_eq!(open.program, PathBuf::from("/usr/bin/open"));
    assert_eq!(open.args, ["--", "/tmp/file"]);

    let reveal = macos_reveal_spec("/tmp/file name").expect("valid absolute path");
    assert_eq!(reveal.program, PathBuf::from("/usr/bin/open"));
    assert_eq!(reveal.args, ["-R", "--", "/tmp/file name"]);
    assert_eq!(macos_open_spec("relative/file"), None);
    assert_eq!(macos_reveal_spec("relative/file"), None);
}

/// macOS reveals inert source suffixes but continues blocking executable and launcher classes.
#[test]
fn macos_directory_policy_selects_packages_without_launching() {
    for path in [
        "/tmp/App.app",
        "/tmp/run.command",
        "/tmp/install.pkg",
        "/tmp/image.dmg",
        "/tmp/link.webloc",
        "/tmp/run.scpt",
        "/tmp/run.applescript",
        "/tmp/run.osascript",
    ] {
        assert_eq!(
            macos_directory_policy(Path::new(path)),
            PathOpenDecision::Revealable(PathKind::Directory)
        );
    }
    assert_eq!(
        macos_directory_policy(Path::new("/tmp/folder")),
        PathOpenDecision::Openable(PathKind::Directory)
    );
}

/// Reveal eligibility depends on filesystem identity, not executable suffix, content, or mode.
#[test]
fn regular_files_are_revealable_regardless_of_type() {
    let root = native_test_root().join(format!("sonicterm-reveal-types-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (name, contents) in [
        ("script.ps1", b"Write-Output test".as_slice()),
        ("run.exe", b"MZ\0binary".as_slice()),
        ("run.cmd", b"@exit".as_slice()),
        ("install.msi", b"binary\0".as_slice()),
        ("link.lnk", b"inert shortcut fixture".as_slice()),
        ("tool.desktop", b"[Desktop Entry]".as_slice()),
        ("tool", b"#!/bin/sh\nexit".as_slice()),
        ("my script.ps1", b"text".as_slice()),
    ] {
        let path = root.join(name);
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let decision = classify_local_target(&path);
        assert_eq!(
            local_target_action(decision),
            Some(LocalTargetAction::Reveal),
            "{name}: {decision:?}"
        );
        validate_reveal_target(&path, decision).unwrap();
        assert_eq!(classify_source_reference(&path), PathOpenDecision::SourceReveal);
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Windows reveal keeps ADS and trailing-dot ambiguity blocked without filtering file types.
#[test]
fn windows_open_policy_blocks_launcher_classes() {
    for path in [r"C:\tmp\file.txt:payload", r"C:\tmp\name. "] {
        assert!(windows_path_policy(Path::new(path)).is_blocked(), "unexpected openable {path}");
    }
    assert_eq!(
        windows_path_policy(Path::new(r"C:\tmp\readme.txt")),
        PathOpenDecision::Openable(PathKind::File)
    );
}

/// The Linux fallback uses only a fixed executable and one argv-separated absolute target.
#[test]
fn linux_fallback_spec_keeps_target_out_of_the_command_name() {
    let Some(spec) = linux_xdg_open_spec(Path::new("/tmp/file name")) else {
        return;
    };
    assert!(matches!(spec.program.to_str(), Some("/usr/bin/xdg-open" | "/bin/xdg-open")));
    assert_eq!(spec.args, ["/tmp/file name"]);
    assert_eq!(linux_xdg_open_spec(Path::new("relative/file")), None);
}

/// Real filesystem classification follows symlinks to files and folders, and rejects dangling links, loops and sockets.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn filesystem_classification_follows_links_and_rejects_special_entries() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    // Darwin's long per-user temp root can exceed sockaddr_un.sun_path before the socket name.
    let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
        "st-kind-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("notes.txt");
    let directory = root.join("folder");
    let file_link = root.join("file-link");
    let directory_link = root.join("folder-link");
    let dangling_link = root.join("dangling-link");
    let looping_link = root.join("looping-link");
    let socket = root.join("socket");
    std::fs::write(&file, b"notes").unwrap();
    std::fs::create_dir(&directory).unwrap();
    symlink(&file, &file_link).unwrap();
    symlink(&directory, &directory_link).unwrap();
    symlink(root.join("gone"), &dangling_link).unwrap();
    symlink(&looping_link, &looping_link).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();

    assert_eq!(classify_local_target(&file), PathOpenDecision::Openable(PathKind::File));
    assert_eq!(classify_local_target(&directory), PathOpenDecision::Openable(PathKind::Directory));
    assert_eq!(classify_local_target(&file_link), PathOpenDecision::Openable(PathKind::File));
    assert_eq!(
        classify_local_target(&directory_link),
        PathOpenDecision::Openable(PathKind::Directory)
    );
    // A link that exists but resolves to nothing is present, so it blocks instead of reading as missing.
    assert_eq!(classify_local_target(&dangling_link), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&looping_link), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&socket), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&root.join("missing")), PathOpenDecision::Missing);

    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// The Linux open adapter receives an already-opened file identity, not its pathname.
#[test]
fn open_target_adapter_owns_an_open_file() {
    use std::io::Write;

    let path = native_test_root().join(format!(
        "sonicterm-path-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let mut created = std::fs::File::create(&path).unwrap();
    created.write_all(b"identity").unwrap();
    drop(created);

    let length = with_opened_target(&path, |file| Ok(file.metadata()?.len())).unwrap();
    assert_eq!(length, 8);
    std::fs::remove_file(&path).unwrap();
    assert!(with_opened_target(&path, |_| Ok(())).is_err());
}

fn probe_candidate_at(display: &str, path: &str, start_col: u16, row: u64) -> PathProbeCandidate {
    PathProbeCandidate {
        spans: smallvec::smallvec![AbsoluteCellSpan {
            row,
            start_col,
            end_col: start_col + u16::try_from(display.chars().count()).unwrap(),
        }],
        target: DetectedTarget::PathCandidate(display.into()),
        resolved_path: PathBuf::from(path),
    }
}

fn probe_candidate(display: &str, path: &str, start_col: u16) -> PathProbeCandidate {
    probe_candidate_at(display, path, start_col, 22)
}

fn probe_key(path: &str, view_top: u64) -> PathProbeKey {
    let row = view_top + 2;
    PathProbeKey {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        pointed: AbsoluteCell { row, col: 4 },
        view_top,
        candidates: vec![probe_candidate_at("./file", path, 4, row)],
        rows: smallvec::smallvec![PathRowIdentity { row, fingerprint: 11 }],
        cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
        cwd_revision: 3,
        link_destination: None,
        scrollback_evicted: 0,
        screen_epoch: 0,
        alt_screen: false,
    }
}

fn openable_result(request: PathProbeRequest) -> PathProbeResult {
    PathProbeResult {
        failure: None,
        selection: Some(PathProbeSelection {
            candidate: request.key.candidates[0].clone(),
            decision: PathOpenDecision::Openable(PathKind::File),
        }),
        request,
    }
}

/// Both window roles retain completed probes until a fresh snapshot validates them, never authorizing clicks before that.
#[test]
fn completed_probe_waits_for_fresh_hover_without_reprobing() {
    for main in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["pending probe"]);
        if main {
            app.main_window_id = Some(window);
        }
        let mut key = probe_key("/work/source.rs", 0);
        key.window_id = window;
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let epoch = request.epoch;
        app.handle_path_probe_finished(openable_result(request));
        assert!(!app.windows[&window].path_probe.authorized(&key, true));
        app.apply_target_lookup(window, Err(()));
        assert_eq!(app.windows[&window].path_probe.epoch, epoch);
        app.apply_target_hover(
            window,
            Some(CellTargetSnapshot {
                pane_id: key.pane_id,
                hover_cells: None,
                display: "./file".into(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Path(key.clone()),
            }),
        );
        assert!(app.windows[&window].path_probe.authorized(&key, true));
        assert_eq!(app.windows[&window].path_probe.epoch, epoch);
        assert!(app.windows.get_mut(&window).unwrap().path_probe.request(key).is_none());
    }
}

/// The next snapshot must reject a completed result whose row changed and schedule a new probe.
#[test]
fn completed_probe_cannot_authorize_changed_content() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["changed probe"]);
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    let epoch = request.epoch;
    app.handle_path_probe_finished(openable_result(request));
    key.rows[0].fingerprint += 1;
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            pane_id: key.pane_id,
            hover_cells: None,
            display: "./file".into(),
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key.clone()),
        }),
    );
    let state = &app.windows[&window].path_probe;
    assert!(!state.authorized(&key, true));
    assert_ne!(state.epoch, epoch);
    assert_eq!(state.current.as_ref(), Some(&key));
}

/// A delayed old completion cannot displace the current result or survive explicit invalidation.
#[test]
fn completed_probe_ignores_old_epochs_and_clears_on_leave() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["probe epochs"]);
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let old = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.clear_target_hover(window);
    let current = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.handle_path_probe_finished(openable_result(current));
    app.handle_path_probe_finished(openable_result(old));
    let target = CellTargetSnapshot {
        pane_id: key.pane_id,
        hover_cells: None,
        display: "./file".into(),
        explicit_hyperlink: false,
        target: ResolvedCellTarget::Path(key.clone()),
    };
    app.apply_target_hover(window, Some(target.clone()));
    assert!(app.windows[&window].path_probe.authorized(&key, true));
    app.clear_target_hover(window);
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.handle_path_probe_finished(openable_result(request));
    app.clear_target_hover(window);
    app.apply_target_hover(window, Some(target));
    assert!(!app.windows[&window].path_probe.authorized(&key, true));
}

/// A completed probe requests its own frame even after PTY output and pointer movement stop.
#[test]
fn completed_probe_schedules_its_validation_frame() {
    let source = include_str!("path_target.rs");
    let handler = source
        .split("fn handle_path_probe_finished(")
        .nth(1)
        .unwrap()
        .split("fn open_modifier_held(")
        .next()
        .unwrap();
    assert!(handler.contains("request_window_redraw()"));
    assert!(!handler.contains("pointer_target("));
}

/// Local previews wait for the exact probe and disappear on modifier release or invalidation.
#[test]
fn local_path_preview_requires_current_authorization() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["preview"]);
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let target = CellTargetSnapshot {
        pane_id: key.pane_id,
        hover_cells: None,
        display: "./file".into(),
        explicit_hyperlink: false,
        target: ResolvedCellTarget::Path(key.clone()),
    };
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.apply_target_hover(window, Some(target.clone()));
    assert!(
        app.windows[&window].link_preview.is_none(),
        "unverified candidates are not destinations"
    );
    let mut result = openable_result(request);
    result.selection.as_mut().unwrap().decision = PathOpenDecision::SourceReveal;
    assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
    app.apply_target_hover(window, Some(target.clone()));
    let preview = app.windows[&window].link_preview.as_ref().expect("validated path preview");
    assert!(preview.uri.contains("/work/source.rs"));
    assert!(preview.available);
    app.windows.get_mut(&window).unwrap().modifiers = winit::keyboard::ModifiersState::empty();
    app.apply_target_hover(window, Some(target));
    assert!(app.windows[&window].link_preview.is_none());
    app.clear_target_hover(window);
    assert!(app.windows[&window].link_preview.is_none());
}

/// Listing guesses, missing paths, and rejected local links never become preview destinations.
#[test]
fn unresolved_local_path_preview_is_hidden() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["preview"]);
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    for failure in ["path-error-missing", "path-error-blocked"] {
        app.windows.get_mut(&window).unwrap().path_probe.invalidate();
        let mut key = probe_key("/work/setup-windows-cairo.ps1", 0);
        key.window_id = window;
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result = PathProbeResult { request, selection: None, failure: Some(failure) };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        app.apply_target_hover(
            window,
            Some(CellTargetSnapshot {
                pane_id: key.pane_id,
                hover_cells: None,
                display: "AM     3595 setup-windows-cairo.ps1".into(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Path(key),
            }),
        );
        assert!(app.windows[&window].link_preview.is_none(), "{failure}");
    }
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            pane_id: 0,
            hover_cells: None,
            display: "file://remote/missing".into(),
            explicit_hyperlink: true,
            target: ResolvedCellTarget::Rejected("path-error-blocked"),
        }),
    );
    assert!(app.windows[&window].link_preview.is_none());
}

/// Failure reasons cannot survive a new target, stale result, or explicit invalidation.
#[test]
fn probe_failure_is_bound_to_current_identity() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/missing", 0);
    let request = state.request(key.clone()).unwrap();
    let result = PathProbeResult { request, selection: None, failure: Some("path-error-missing") };
    assert!(state.accept(&result, Some(&key)));
    assert_eq!(state.failure_for(&key), Some("path-error-missing"));
    assert!(!state.authorized(&key, true));
    let next = probe_key("/work/other", 0);
    state.request(next.clone()).unwrap();
    assert_eq!(state.failure_for(&next), None);
    assert!(!state.accept(&result, Some(&next)));
    assert_eq!(state.failure_for(&next), None);
    state.invalidate();
    assert_eq!(state.failure_for(&key), None);
}

/// A result authorizes a click only when epoch, key, and live modifier all match.
#[test]
fn probe_state_requires_current_epoch_key_and_modifier() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = openable_result(request);

    assert!(state.accept(&result, Some(&key)));
    assert!(!state.authorized(&key, false), "modifier release must deactivate authorization");
    assert!(state.authorized(&key, true));
    assert_eq!(state.decision_for(&key), Some(PathOpenDecision::Openable(PathKind::File)));
}

/// Accepted spans retain authorization when a destination adds only a longer candidate.
#[test]
fn probe_state_retains_selection_across_irrelevant_candidate_changes() {
    let mut state = PathProbeState::default();
    let mut key = probe_key("/work/Folder", 20);
    let selected = probe_candidate("Folder", "/work/Folder", 7);
    key.pointed.col = 8;
    key.candidates = vec![selected.clone()];
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    // Shortest-first never lets a longer candidate displace an existing shorter winner.
    let mut moved = key.clone();
    moved.pointed.col = 9;
    moved.candidates.push(probe_candidate("My Folder", "/work/My Folder", 4));
    assert!(state.request(moved.clone()).is_none());
    assert!(state.authorized(&moved, true));
}

/// A newly visible equal or shorter candidate revokes authorization and schedules a fresh probe.
#[test]
fn probe_state_reprobes_for_new_competing_candidates() {
    for competing in [
        probe_candidate("Name Here", "/work/Name Here", 5),
        probe_candidate("Name", "/work/Name", 5),
    ] {
        let mut state = PathProbeState::default();
        let selected = probe_candidate("Left Name", "/work/Left Name", 0);
        let mut key = probe_key("/work/Left Name", 20);
        key.pointed.col = 1;
        key.candidates = vec![selected.clone()];
        let result = openable_result(state.request(key.clone()).unwrap());
        assert!(state.accept(&result, Some(&key)));

        let mut moved = key.clone();
        moved.pointed.col = 6;
        moved.candidates = vec![competing.clone(), selected.clone()];
        assert!(state.request(moved.clone()).is_some(), "candidate: {competing:?}");
        assert!(!state.authorized(&moved, true), "candidate: {competing:?}");
    }
}

/// An in-flight result cannot authorize a destination with an unprobed competing candidate.
#[test]
fn probe_state_rejects_results_after_candidate_set_expands() {
    let mut state = PathProbeState::default();
    let selected = probe_candidate("Left Name", "/work/Left Name", 0);
    let mut key = probe_key("/work/Left Name", 20);
    key.pointed.col = 1;
    key.candidates = vec![selected.clone()];
    let request = state.request(key.clone()).unwrap();

    let mut moved = key.clone();
    moved.pointed.col = 6;
    moved.candidates = vec![probe_candidate("Name Here", "/work/Name Here", 5), selected];
    assert!(!state.accept(&openable_result(request), Some(&moved)));
    assert!(state.request(moved).is_some());
}

/// A transient inability to re-read the pointer target must allow the same path to be probed again.
#[test]
fn unavailable_fresh_target_does_not_wedge_probe_state() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("first request");

    assert!(!state.accept(&openable_result(request), None));
    assert!(state.request(key).is_some(), "the same target must be eligible for a retry");
}

/// Delayed results from a prior same-value incarnation cannot survive an ABA transition.
#[test]
fn probe_epoch_rejects_same_key_after_leave_and_reenter() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let first = state.request(key.clone()).expect("first request");
    state.invalidate();
    let second = state.request(key.clone()).expect("re-enter schedules a new epoch");

    assert!(!state.accept(&openable_result(first), Some(&key)));
    assert!(!state.authorized(&key, true));
    assert!(state.accept(&openable_result(second), Some(&key)));
    assert!(state.authorized(&key, true));
}

/// Unrelated grid mutations preserve authorization when pointed-row identity is unchanged.
#[test]
fn probe_state_ignores_unrelated_grid_mutation() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let unchanged_row = key.clone();
    assert!(state.request(unchanged_row.clone()).is_none());
    assert!(state.authorized(&unchanged_row, true));
}

/// Pointed-row identity changes revoke authorization and schedule a fresh probe.
#[test]
fn probe_state_reprobes_after_pointed_row_changes() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let mut changed = key.clone();
    changed.rows[0].fingerprint = changed.rows[0].fingerprint.wrapping_add(1);
    assert!(state.request(changed.clone()).is_some());
    assert!(!state.authorized(&changed, true));
}

/// Moving between wrapped fragments preserves authorization only while every row identity matches.
#[test]
fn wrapped_probe_authorization_tracks_all_rows_and_fragments() {
    let candidate = PathProbeCandidate {
        spans: smallvec::smallvec![
            AbsoluteCellSpan { row: 20, start_col: 4, end_col: 10 },
            AbsoluteCellSpan { row: 21, start_col: 0, end_col: 7 },
        ],
        target: DetectedTarget::PathCandidate("src/long/path.rs".into()),
        resolved_path: PathBuf::from("/work/src/long/path.rs"),
    };
    let mut key = probe_key("/work/src/long/path.rs", 20);
    key.pointed = AbsoluteCell { row: 20, col: 6 };
    key.candidates = vec![candidate.clone()];
    key.rows = smallvec::smallvec![
        PathRowIdentity { row: 20, fingerprint: 11 },
        PathRowIdentity { row: 21, fingerprint: 12 },
    ];
    let mut state = PathProbeState::default();
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let mut second_fragment = key.clone();
    second_fragment.pointed = AbsoluteCell { row: 21, col: 2 };
    assert!(state.request(second_fragment.clone()).is_none());
    assert!(state.authorized(&second_fragment, true));

    let mut changed_row = second_fragment.clone();
    changed_row.rows[1].fingerprint = changed_row.rows[1].fingerprint.wrapping_add(1);
    assert!(state.request(changed_row.clone()).is_some());
    assert!(!state.authorized(&changed_row, true));
}

/// Screen incarnation and viewport projection remain part of wrapped authorization identity.
#[test]
fn wrapped_probe_rejects_screen_or_viewport_aba() {
    for mutate in [
        |key: &mut PathProbeKey| key.screen_epoch = key.screen_epoch.wrapping_add(1),
        |key: &mut PathProbeKey| key.view_top = key.view_top.saturating_add(1),
    ] {
        let mut state = PathProbeState::default();
        let key = probe_key("/work/file", 20);
        let result = openable_result(state.request(key.clone()).unwrap());
        assert!(state.accept(&result, Some(&key)));

        let mut changed = key.clone();
        mutate(&mut changed);
        assert!(state.request(changed.clone()).is_some());
        assert!(!state.authorized(&changed, true));
    }
}

/// A viewport round trip with the same visible value still gets a distinct epoch.
#[test]
fn probe_epoch_rejects_viewport_round_trip_results() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let old = state.request(key.clone()).expect("initial request");
    state.invalidate();
    let away = probe_key("/work/file", 30);
    state.request(away).expect("scroll-away request");
    state.invalidate();
    let current = state.request(key.clone()).expect("scroll-back request");

    assert!(!state
        .accept(&PathProbeResult { failure: None, request: old, selection: None }, Some(&key)));
    assert_eq!(state.decision_for(&key), None);
    assert!(state.accept(&openable_result(current), Some(&key)));
}

/// The shortest openable candidate wins, and a blocked shortest tier ends the probe instead of falling to a longer path.
#[test]
fn candidate_selection_prefers_shortest_and_stops_at_blocked() {
    let candidates = vec![
        probe_candidate("OneDrive - Microsoft", "/work/OneDrive - Microsoft", 0),
        probe_candidate("OneDrive", "/work/OneDrive", 0),
    ];
    let selected =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::Directory))
            .expect("shortest candidate");
    assert_eq!(selected.candidate.display(), "OneDrive");

    let longer = select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive") {
            PathOpenDecision::Missing
        } else {
            PathOpenDecision::Openable(PathKind::Directory)
        }
    })
    .expect("a missing shorter candidate permits the longer one");
    assert_eq!(longer.candidate.display(), "OneDrive - Microsoft");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::Directory)
        }
    })
    .is_none());
}

/// A trimmed prose path wins over its punctuation-ending literal; only its absence lets the literal open.
#[test]
fn candidate_selection_prefers_trimmed_path_over_literal() {
    let candidates = vec![
        probe_candidate("src/main.rs,", "/work/src/main.rs,", 0),
        probe_candidate("src/main.rs", "/work/src/main.rs", 0),
    ];
    let trimmed =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::File))
            .expect("trimmed path wins");
    assert_eq!(trimmed.candidate.display(), "src/main.rs");

    let literal = select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs") {
            PathOpenDecision::Missing
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .expect("a missing trimmed path permits the literal");
    assert_eq!(literal.candidate.display(), "src/main.rs,");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .is_none());
}

/// Openable and revealable results share one tier, where the earlier start wins the tie in either input order.
#[test]
fn actionable_candidate_kinds_tie_break_by_start() {
    let candidates = vec![
        probe_candidate("left.lua", "/work/left.lua", 0),
        probe_candidate("right.rs", "/work/right.rs", 9),
    ];
    for reversed in [false, true] {
        let mut ordered = candidates.clone();
        if reversed {
            ordered.reverse();
        }
        let selected = select_openable_candidate(&ordered, |path| {
            if path.ends_with("left.lua") {
                PathOpenDecision::Revealable(PathKind::File)
            } else {
                PathOpenDecision::Openable(PathKind::File)
            }
        })
        .expect("earlier candidate");
        assert_eq!(selected.candidate.display(), "left.lua");
        assert_eq!(selected.decision, PathOpenDecision::Revealable(PathKind::File));
    }
}

/// Revealable probe results authorize the same epoch-keyed activation path as openable results.
#[test]
fn revealable_selection_is_authorized_without_weakening_freshness() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/source.lua", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = PathProbeResult {
        failure: None,
        selection: Some(PathProbeSelection {
            candidate: request.key.candidates[0].clone(),
            decision: PathOpenDecision::Revealable(PathKind::File),
        }),
        request,
    };

    assert!(state.accept(&result, Some(&key)));
    assert!(state.authorized(&key, true));
    assert!(!state.authorized(&key, false));
}

/// Real filesystem classification selects an existing short prefix first and the complete spaced entry once it is gone.
#[test]
fn filesystem_selection_prefers_shortest_existing_entry() {
    let root = native_test_root().join(format!(
        "sonicterm-spaced-selection-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let short = root.join("OneDrive");
    let complete = root.join("OneDrive - Microsoft");
    std::fs::create_dir_all(&short).unwrap();
    std::fs::create_dir(&complete).unwrap();
    let candidates = vec![
        PathProbeCandidate {
            spans: smallvec::smallvec![AbsoluteCellSpan { row: 0, start_col: 0, end_col: 20 }],
            target: DetectedTarget::BareName("OneDrive - Microsoft".into()),
            resolved_path: complete.clone(),
        },
        PathProbeCandidate {
            spans: smallvec::smallvec![AbsoluteCellSpan { row: 0, start_col: 0, end_col: 8 }],
            target: DetectedTarget::BareName("OneDrive".into()),
            resolved_path: short.clone(),
        },
    ];

    let selected = select_openable_candidate(&candidates, classify_local_target)
        .expect("short directory is openable");
    assert_eq!(selected.candidate.resolved_path, short);
    std::fs::remove_dir(&short).unwrap();
    let selected = select_openable_candidate(&candidates, classify_local_target)
        .expect("complete spaced directory is openable");
    assert_eq!(selected.candidate.resolved_path, complete);
    std::fs::remove_dir_all(root).unwrap();
}

/// Equal-length present candidates resolve to the earlier start rather than staying inert.
#[test]
fn equal_length_openable_candidates_prefer_earlier_start() {
    let candidates = vec![
        probe_candidate("Name Here", "/work/Name Here", 5),
        probe_candidate("Left Name", "/work/Left Name", 0),
    ];
    let selected =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::Directory))
            .expect("earlier start");
    assert_eq!(selected.candidate.display(), "Left Name");
}

/// Filesystem probes authorize an openable entry and revoke the same entry once missing.
#[test]
fn filesystem_openability_controls_path_authorization() {
    let path = native_test_root().join(format!(
        "sonicterm-path-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"present").unwrap();

    let mut state = PathProbeState::default();
    let key = probe_key(path.to_str().unwrap(), 20);
    let request = state.request(key.clone()).unwrap();
    let existing = PathProbeResult {
        failure: None,
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&existing, Some(&key)));
    assert!(state.authorized(&key, true));

    std::fs::remove_file(&path).unwrap();
    state.invalidate();
    let request = state.request(key.clone()).unwrap();
    let missing = PathProbeResult {
        failure: None,
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&missing, Some(&key)));
    assert!(!state.authorized(&key, true));
}

/// A window's newer request replaces only its own waiting request while one wake is pending.
#[test]
fn probe_mailbox_replaces_only_the_same_windows_waiting_request() {
    let (mailbox, wakes) = PathProbeMailbox::new();
    let mut state = PathProbeState::default();
    let first = state.request(probe_key("/work/first", 20)).unwrap();
    state.invalidate();
    let latest = state.request(probe_key("/work/latest", 20)).unwrap();

    mailbox.submit(first).unwrap();
    mailbox.submit(latest.clone()).unwrap();
    assert_eq!(wakes.len(), 1, "only one worker wake is queued");
    assert_eq!(mailbox.waiting_len(), 1, "one window keeps one waiting request");
    wakes.recv().unwrap();
    assert_eq!(mailbox.take_next(), Some(latest));
    assert_eq!(mailbox.take_next(), None);
}

// Generous bound for a loaded CI host; a healthy worker answers in microseconds.
const HELD_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
// Short settle window used only to prove that no further probe starts.
const IDLE_PROBE_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// Production worker loop whose classifier parks each probe until the test releases it.
///
/// Holding one execution lets a test queue work from several windows behind it
/// and then observe exactly which request the worker serves next.
struct HeldProbeWorker {
    mailbox: PathProbeMailbox,
    started: Receiver<PathBuf>,
    release: Sender<()>,
    results: Receiver<PathProbeResult>,
}

impl HeldProbeWorker {
    fn spawn() -> Self {
        let (mailbox, wake) = PathProbeMailbox::new();
        let queue = Arc::clone(&mailbox.queue);
        let (started_tx, started) = crossbeam_channel::unbounded();
        let (release, release_rx) = crossbeam_channel::unbounded::<()>();
        let (results_tx, results) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            run_probe_worker(
                &wake,
                &queue,
                |path| {
                    let _ = started_tx.send(path.to_path_buf());
                    // A dropped harness releases the worker so its thread can exit.
                    if release_rx.recv().is_ok() {
                        PathOpenDecision::Openable(PathKind::File)
                    } else {
                        PathOpenDecision::Missing
                    }
                },
                |result| results_tx.send(result).is_ok(),
            );
        });
        Self { mailbox, started, release, results }
    }

    /// Wait until the worker parks on `path`, proving it chose that request next.
    fn expect_started(&self, path: &str) {
        let started = self
            .started
            .recv_timeout(HELD_PROBE_TIMEOUT)
            .unwrap_or_else(|_| panic!("worker never started a probe; expected {path}"));
        assert_eq!(started.as_path(), Path::new(path), "worker served the wrong request next");
    }

    /// Release the parked probe and return its delivered result.
    fn finish(&self) -> PathProbeResult {
        self.release.send(()).unwrap();
        self.results.recv_timeout(HELD_PROBE_TIMEOUT).expect("held probe delivers a result")
    }

    /// Confirm the worker takes `path` next, then complete it.
    fn serve(&self, path: &str) -> PathProbeResult {
        self.expect_started(path);
        self.finish()
    }

    /// Prove nothing is waiting and no further probe starts once the queue drains.
    fn assert_idle(&self) {
        assert_eq!(self.mailbox.waiting_len(), 0);
        assert!(self.started.recv_timeout(IDLE_PROBE_WAIT).is_err(), "no further probe runs");
    }
}

/// Probe key owned by synthetic window `window`, so several windows can race one worker.
fn window_probe_key(window: u64, path: &str) -> PathProbeKey {
    PathProbeKey { window_id: WindowId::from(window), ..probe_key(path, 0) }
}

/// Probe key owned by a seeded App window.
fn seeded_probe_key(window: WindowId, path: &str) -> PathProbeKey {
    PathProbeKey { window_id: window, ..probe_key(path, 0) }
}

/// One redraw's hover refresh for a stationary local path under the pointer.
fn hover_probe_key(app: &mut App, window: WindowId, key: &PathProbeKey) {
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            pane_id: key.pane_id,
            hover_cells: None,
            display: "./file".into(),
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key.clone()),
        }),
    );
}

/// Route an App's probe handle to `worker`; these tests never dispatch an open.
fn install_held_worker(app: &mut App, worker: &HeldProbeWorker) {
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers { probe: worker.mailbox.clone(), open });
}

/// A newer request from another window never displaces a waiting one;
/// both windows reach a terminal result.
#[test]
fn probe_mailbox_keeps_waiting_requests_from_distinct_windows() {
    let worker = HeldProbeWorker::spawn();
    let (mut blocker, mut first, mut second) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let blocker_key = window_probe_key(3, "/work/blocker");
    let first_key = window_probe_key(1, "/work/first");
    let second_key = window_probe_key(2, "/work/second");
    worker.mailbox.submit(blocker.request(blocker_key.clone()).unwrap()).unwrap();
    worker.expect_started("/work/blocker");

    // The first window queues behind the held execution, then a second window submits.
    worker.mailbox.submit(first.request(first_key.clone()).unwrap()).unwrap();
    worker.mailbox.submit(second.request(second_key.clone()).unwrap()).unwrap();
    // The first window's key is in flight, so its unchanged hover never asks again.
    assert!(first.request(first_key.clone()).is_none());

    assert!(blocker.accept(&worker.finish(), Some(&blocker_key)));
    let first_result = worker.serve("/work/first");
    let second_result = worker.serve("/work/second");
    worker.assert_idle();
    assert!(first.accept(&first_result, Some(&first_key)));
    assert!(first.authorized(&first_key, true));
    assert!(second.accept(&second_result, Some(&second_key)));
    assert!(second.authorized(&second_key, true));
    // A result carries its window identity, so it can never complete another window's key.
    assert!(!second.accept(&first_result, Some(&second_key)));
    assert!(second.authorized(&second_key, true));
}

/// Same-window churn A→B→A replaces only that window's waiting key, keeps its turn,
/// and runs only the newest epoch.
#[test]
fn probe_mailbox_replaces_a_windows_own_waiting_key_in_place() {
    let worker = HeldProbeWorker::spawn();
    let (mut blocker, mut churn, mut peer) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let key_a = window_probe_key(1, "/work/a");
    let key_b = window_probe_key(1, "/work/b");
    let peer_key = window_probe_key(2, "/work/peer");
    worker.mailbox.submit(blocker.request(window_probe_key(3, "/work/blocker")).unwrap()).unwrap();
    worker.expect_started("/work/blocker");

    let stale = churn.request(key_a.clone()).unwrap();
    worker.mailbox.submit(stale.clone()).unwrap();
    worker.mailbox.submit(peer.request(peer_key.clone()).unwrap()).unwrap();
    worker.mailbox.submit(churn.request(key_b).unwrap()).unwrap();
    worker.mailbox.submit(churn.request(key_a.clone()).unwrap()).unwrap();
    assert_eq!(worker.mailbox.waiting_len(), 2, "each window keeps one waiting key");

    worker.finish();
    // The churning window keeps its first-queued turn ahead of the peer
    // and runs only its newest epoch.
    let served = worker.serve("/work/a");
    assert_ne!(served.request.epoch, stale.epoch);
    let peer_result = worker.serve("/work/peer");
    worker.assert_idle();
    // The displaced epoch can never complete, even though it names the same key.
    let displaced = PathProbeResult { request: stale, ..served.clone() };
    assert!(!churn.accept(&displaced, Some(&key_a)));
    assert!(churn.accept(&served, Some(&key_a)));
    assert!(churn.authorized(&key_a, true));
    assert!(peer.accept(&peer_result, Some(&peer_key)));
}

/// Closing a window drops only its waiting request; an executing request still
/// completes for its own window.
#[test]
fn probe_mailbox_close_drops_only_that_windows_waiting_request() {
    let worker = HeldProbeWorker::spawn();
    let (mut running, mut closing, mut survivor) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let survivor_key = window_probe_key(3, "/work/survivor");
    worker.mailbox.submit(running.request(window_probe_key(1, "/work/running")).unwrap()).unwrap();
    worker.expect_started("/work/running");
    worker.mailbox.submit(closing.request(window_probe_key(2, "/work/closing")).unwrap()).unwrap();
    worker.mailbox.submit(survivor.request(survivor_key.clone()).unwrap()).unwrap();

    assert!(worker.mailbox.cancel_window(WindowId::from(2_u64)));
    assert!(!worker.mailbox.cancel_window(WindowId::from(2_u64)), "nothing else waits for it");
    // An executing request is not waiting, so closing its window leaves it to finish.
    assert!(!worker.mailbox.cancel_window(WindowId::from(1_u64)));
    assert_eq!(worker.mailbox.waiting_len(), 1);
    assert_eq!(worker.finish().request.key.window_id, WindowId::from(1_u64));
    let survivor_result = worker.serve("/work/survivor");
    worker.assert_idle();
    assert!(survivor.accept(&survivor_result, Some(&survivor_key)));
}

/// A peer that re-requests every frame cannot starve a quiet window's single waiting request.
#[test]
fn probe_mailbox_keeps_fair_progress_under_a_churning_peer() {
    let worker = HeldProbeWorker::spawn();
    let (mut quiet, mut churn) = (PathProbeState::default(), PathProbeState::default());
    let quiet_key = window_probe_key(1, "/work/quiet");
    let frame = |index: usize| window_probe_key(2, &format!("/work/frame-{index}"));
    worker.mailbox.submit(churn.request(frame(0)).unwrap()).unwrap();
    worker.expect_started("/work/frame-0");
    worker.mailbox.submit(quiet.request(quiet_key.clone()).unwrap()).unwrap();
    for index in 1..=32 {
        worker.mailbox.submit(churn.request(frame(index)).unwrap()).unwrap();
        assert_eq!(worker.mailbox.waiting_len(), 2, "churn replaces its own waiting slot");
    }

    worker.finish();
    worker.expect_started("/work/quiet");
    // Churn while the quiet request executes still collapses into one waiting request.
    for index in 33..=64 {
        worker.mailbox.submit(churn.request(frame(index)).unwrap()).unwrap();
        assert_eq!(worker.mailbox.waiting_len(), 1);
    }
    let quiet_result = worker.finish();
    let churn_result = worker.serve("/work/frame-64");
    worker.assert_idle();
    assert!(quiet.accept(&quiet_result, Some(&quiet_key)));
    assert!(quiet.authorized(&quiet_key, true));
    assert!(churn.accept(&churn_result, Some(&frame(64))));
}

/// Production child teardown cancels a closed window's waiting probe and discards
/// a late result for it.
#[test]
fn closing_a_child_window_cancels_its_probe_and_discards_its_late_result() {
    let worker = HeldProbeWorker::spawn();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    install_held_worker(&mut app, &worker);
    let running = app.__test_seed_child_window(&["running"]);
    let closing = app.__test_seed_child_window(&["closing"]);
    let survivor = app.__test_seed_child_window(&["survivor"]);
    let survivor_key = seeded_probe_key(survivor, "/work/survivor");
    hover_probe_key(&mut app, running, &seeded_probe_key(running, "/work/running"));
    worker.expect_started("/work/running");
    hover_probe_key(&mut app, closing, &seeded_probe_key(closing, "/work/closing"));
    hover_probe_key(&mut app, survivor, &survivor_key);
    assert_eq!(worker.mailbox.waiting_len(), 2);

    assert!(app.close_child_window(closing));
    assert_eq!(worker.mailbox.waiting_len(), 1, "closing drops only its own waiting probe");
    assert!(app.close_child_window(running));
    // The running probe completes after its window closed, so no owner receives it.
    app.handle_path_probe_finished(worker.finish());
    assert!(!app.windows.contains_key(&running));
    app.handle_path_probe_finished(worker.serve("/work/survivor"));
    worker.assert_idle();
    hover_probe_key(&mut app, survivor, &survivor_key);
    assert!(app.windows[&survivor].path_probe.authorized(&survivor_key, true));
}

/// A stationary hover keeps its feedback while a second window submits a new probe on every
/// redraw, through the real worker loop and filesystem classifier.
///
/// App-level coverage with the real filesystem: it drives hover probes through test helpers,
/// so it does not exercise native presentation or PTY output.
///
/// Only the streaming window's first probe is held, standing in for a slow filesystem
/// call, so the quiet window's single request queues behind it as it would on a busy host.
#[test]
fn quiet_hover_keeps_first_window_feedback_while_a_second_window_streams() {
    let root = native_test_root().join(format!(
        "sonicterm-quiet-hover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let quiet_path = root.join("quiet.txt");
    std::fs::write(&quiet_path, b"quiet").unwrap();
    let frame_path = |index: usize| root.join(format!("frame-{index}.txt"));
    let gate = frame_path(0);
    std::fs::write(&gate, b"frame").unwrap();

    let (mailbox, wake) = PathProbeMailbox::new();
    let queue = Arc::clone(&mailbox.queue);
    let (gate_started_tx, gate_started) = crossbeam_channel::unbounded::<()>();
    let (gate_release, gate_release_rx) = crossbeam_channel::unbounded::<()>();
    let (results_tx, results) = crossbeam_channel::unbounded();
    let (worker_stopped_tx, worker_stopped) = crossbeam_channel::bounded::<()>(1);
    std::thread::spawn(move || {
        run_probe_worker(
            &wake,
            &queue,
            |path| {
                if path == gate.as_path() {
                    // Hold only the streaming window's first probe so later work queues behind it.
                    let _ = gate_started_tx.send(());
                    let _ = gate_release_rx.recv();
                }
                classify_local_target(path)
            },
            |result| results_tx.send(result).is_ok(),
        );
        let _ = worker_stopped_tx.send(());
    });
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers { probe: mailbox, open });
    let quiet = app.__test_seed_child_window(&["quiet"]);
    let peer = app.__test_seed_child_window(&["peer"]);
    app.frontmost_window = Some(quiet);
    app.windows.get_mut(&quiet).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let quiet_key = seeded_probe_key(quiet, quiet_path.to_str().unwrap());
    let peer_key = |index: usize| seeded_probe_key(peer, frame_path(index).to_str().unwrap());
    let assert_quiet_feedback = |app: &App, moment: &str| {
        let window = &app.windows[&quiet];
        assert!(
            window.path_probe.authorized(&quiet_key, true),
            "{moment}: the quiet target is authorized"
        );
        assert!(
            window.hovered_url.as_ref().is_some_and(|hover| hover.active()),
            "{moment}: the quiet underline stays active"
        );
        assert!(window.hover_link, "{moment}: the quiet pointer cursor stays active");
    };

    hover_probe_key(&mut app, peer, &peer_key(0));
    gate_started.recv_timeout(HELD_PROBE_TIMEOUT).expect("the streaming probe is held");
    for index in 1..=16 {
        // Each redraw refreshes both windows; only the streaming window's content changes.
        hover_probe_key(&mut app, quiet, &quiet_key);
        hover_probe_key(&mut app, peer, &peer_key(index));
    }
    gate_release.send(()).unwrap();
    for _ in 0..3 {
        let result =
            results.recv_timeout(HELD_PROBE_TIMEOUT).expect("both windows' queued probes complete");
        app.handle_path_probe_finished(result);
        hover_probe_key(&mut app, quiet, &quiet_key);
    }
    assert_quiet_feedback(&app, "after the queued probes drain");
    for index in 17..=24 {
        hover_probe_key(&mut app, peer, &peer_key(index));
        hover_probe_key(&mut app, quiet, &quiet_key);
        assert_quiet_feedback(&app, "while the peer keeps streaming");
    }
    // A running Windows probe holds each part of its path open without delete sharing, so stop
    // the worker before removing `root`: dropping the app closes its mailbox, and the worker ends
    // once its queued probes finish.
    drop(app);
    drop(gate_release);
    worker_stopped.recv_timeout(HELD_PROBE_TIMEOUT).expect("the probe worker stops");
    std::fs::remove_dir_all(root).unwrap();
}

/// Pending and displaced probe work never authorizes activation; a current result opens only
/// after the click validates it against the key the click computes for its own cell.
#[cfg(unix)]
#[test]
fn only_a_validated_current_probe_authorizes_activation() {
    let root = native_test_root().join(format!(
        "sonicterm-pending-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), b"notes").unwrap();
    // No worker thread runs: the test takes queued requests itself, so they stay pending.
    let (mailbox, _wake) = PathProbeMailbox::new();
    let (open, opens) = crossbeam_channel::bounded(1);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.path_workers = Some(PathWorkers { probe: mailbox.clone(), open });
    let window = app.__test_seed_child_window(&["activation"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let output = format!("\x1b]7;file://{}\x1b\\./notes.txt", root.display());
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let hover = |app: &mut App| {
        let target = app.cell_target_at(window, pane, 0, 3).expect("explicit path target");
        assert!(matches!(target.target, ResolvedCellTarget::Path(_)), "the path needs a probe");
        app.apply_target_hover(window, Some(target));
    };
    // Each click starts unarmed so a pending-failure click never becomes a copy confirmation.
    let click = |app: &mut App| {
        app.windows.get_mut(&window).unwrap().path_probe.failed_click = None;
        app.activate_target_at(window, pane, 0, 3)
    };
    let serve = |request: PathProbeRequest| {
        let outcome = probe_candidates(&request.key.candidates, classify_local_target);
        PathProbeResult {
            failure: outcome.as_ref().err().copied(),
            selection: outcome.ok(),
            request,
        }
    };

    hover(&mut app);
    click(&mut app);
    assert!(opens.try_recv().is_err(), "a queued request never authorizes an open");

    // A newer hover epoch displaces the probe the worker already took for this window.
    let displaced = mailbox.take_next().expect("hover queued a probe");
    app.clear_target_hover(window);
    hover(&mut app);
    app.handle_path_probe_finished(serve(displaced));
    hover(&mut app);
    click(&mut app);
    assert!(opens.try_recv().is_err(), "a displaced completion never authorizes an open");

    // The click validates the current completion against the key it computes for its own cell.
    app.handle_path_probe_finished(serve(mailbox.take_next().expect("newer epoch queued")));
    assert!(click(&mut app));
    let request = opens.try_recv().expect("the validated current result dispatches one open");
    assert!(request.path.ends_with("notes.txt"));
    assert!(opens.try_recv().is_err());
    std::fs::remove_dir_all(root).unwrap();
}

/// A finished check that no hover has applied yet still authorizes a click on the same cell,
/// because the click accepts it against the key the click computes itself. Once the viewport
/// has moved, the click's key differs and the waiting result authorizes nothing.
#[cfg(unix)]
#[test]
fn clicks_accept_a_finished_check_only_for_their_own_key() {
    for viewport_moves in [false, true] {
        let root = native_test_root().join(format!(
            "sonicterm-waiting-open-{}-{viewport_moves}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), b"notes").unwrap();
        // No worker thread runs: the test serves the queued request itself.
        let (mailbox, _wake) = PathProbeMailbox::new();
        let (open, opens) = crossbeam_channel::bounded(1);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.path_workers = Some(PathWorkers { probe: mailbox.clone(), open });
        let window = app.__test_seed_child_window(&["waiting"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(40, 3);
        // The path sits on the second row, so two more lines scroll it to the top row.
        let output = format!("\x1b]7;file://{}\x1b\\\r\n./notes.txt", root.display());
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        let target = app.cell_target_at(window, pane, 1, 3).expect("explicit path target");
        app.apply_target_hover(window, Some(target));
        let request = mailbox.take_next().expect("hover queued a probe");
        let outcome = probe_candidates(&request.key.candidates, classify_local_target);
        let expected = outcome.as_ref().ok().map(|selection| selection.decision);
        app.handle_path_probe_finished(PathProbeResult {
            failure: outcome.as_ref().err().copied(),
            selection: outcome.ok(),
            request,
        });
        let row = if viewport_moves {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\r\n\r\n"));
            0
        } else {
            1
        };
        // No hover refresh ran, so the finished result still waits when the click arrives.
        assert!(app.windows[&window].path_probe.pending_result.is_some());
        assert!(app.activate_target_at(window, pane, row, 3));
        if viewport_moves {
            assert!(opens.try_recv().is_err(), "a moved viewport never uses the waiting result");
        } else {
            let request = opens.try_recv().expect("the click accepts the waiting result");
            assert!(request.path.ends_with("notes.txt"));
            assert_eq!(Some(request.expected_decision), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

// Seed one child pane at `cols`x`rows` and feed `output` through the real parser.
fn wrapped_url_pane(app: &mut App, cols: u16, rows: u16, output: &[u8]) -> (WindowId, u64) {
    let window = app.__test_seed_child_window(&["wrapped url"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    (window, pane)
}

const ISSUE_URI: &str = "https://github.com/D0n9X1n/SonicTerm/issues/1349";

/// Every fragment of a soft-wrapped URL resolves one whole destination with spans covering it exactly.
#[test]
fn wrapped_url_fragments_share_one_full_destination() {
    for cols in [21u16, 31, 80] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = wrapped_url_pane(&mut app, cols, 10, ISSUE_URI.as_bytes());
        let rows = (ISSUE_URI.len() as u16).div_ceil(cols);
        for row in 0..rows {
            let target = app.cell_target_at(window, pane, row, 0).expect("fragment target");
            assert_eq!(target.display, ISSUE_URI, "cols {cols} row {row}");
            assert!(
                matches!(&target.target, ResolvedCellTarget::Uri(destination) if destination == ISSUE_URI)
            );
            let cells = target.hovered(true).expect("highlight").cells;
            assert_eq!(cells.spans().len(), usize::from(rows), "cols {cols} row {row}");
            let covered: usize =
                cells.spans().iter().map(|span| usize::from(span.end_col - span.start_col)).sum();
            assert_eq!(covered, ISSUE_URI.len(), "cols {cols} row {row}");
            assert!(cells.contains(row, 0));
        }
    }
}

/// A wrap inside the scheme still yields the whole URI while the chain stays within its row bound.
#[test]
fn wrapped_url_split_inside_scheme_resolves_whole_uri() {
    let uri = "https://a.test/x";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = wrapped_url_pane(&mut app, 5, 8, uri.as_bytes());
    for row in 0..(uri.len() as u16).div_ceil(5) {
        let target = app.cell_target_at(window, pane, row, 0).expect("scheme-split fragment");
        assert_eq!(target.display, uri, "row {row}");
    }
}

/// Wrapped prose keeps query text and drops the trailing bracket and sentence punctuation.
#[test]
fn wrapped_url_excludes_surrounding_prose_punctuation() {
    let uri = "https://github.com/D0n9X1n/SonicTerm/issues/1349?tab=all";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let text = format!("See ({uri}) now.");
    let (window, pane) = wrapped_url_pane(&mut app, 21, 10, text.as_bytes());
    let target = app.cell_target_at(window, pane, 0, 6).expect("prose URI");
    assert_eq!(target.display, uri);
    assert!(matches!(&target.target, ResolvedCellTarget::Uri(destination) if destination == uri));
}

/// Network URLs stay clickable when both local-target settings are disabled.
#[test]
fn wrapped_network_url_ignores_local_target_settings() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.terminal.clickable_local_targets = false;
    app.config.terminal.clickable_bare_names = false;
    let (window, pane) = wrapped_url_pane(&mut app, 21, 10, ISSUE_URI.as_bytes());
    for row in 0..3 {
        let target = app.cell_target_at(window, pane, row, 0).expect("network URI");
        assert!(
            matches!(&target.target, ResolvedCellTarget::Uri(destination) if destination == ISSUE_URI)
        );
    }
}

/// A hard line break with indentation is never concatenated into one destination.
#[test]
fn hard_break_with_indent_never_joins_fragments() {
    let head = "https://github.com/D0n9X1n/So";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let output = format!("{head}\r\n    nicTerm/issues/1349");
    let (window, pane) = wrapped_url_pane(&mut app, 80, 10, output.as_bytes());
    assert!(!app.windows[&window].panes[&pane]
        .parser
        .lock()
        .grid()
        .row_at_abs(1)
        .unwrap()
        .soft_wrapped_from_previous());
    let first = app.cell_target_at(window, pane, 0, 2).expect("head URI");
    assert_eq!(first.display, head);
    let second = app.cell_target_at(window, pane, 1, 8);
    assert!(!matches!(
        second.as_ref().map(|snapshot| &snapshot.target),
        Some(ResolvedCellTarget::Uri(destination)) if destination == ISSUE_URI
    ));
}

/// An incomplete wrap chain stays inert instead of activating a visible prefix.
#[test]
fn incomplete_wrap_chains_stay_inert() {
    // Head scrolled above the viewport: the visible rows cannot prove the whole destination.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = wrapped_url_pane(&mut app, 10, 3, ISSUE_URI.as_bytes());
    {
        let parser = app.windows[&window].panes[&pane].parser.lock();
        let grid = parser.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, None);
        assert!(view_top > 0);
        assert!(grid.row_at_abs(view_top).unwrap().soft_wrapped_from_previous());
    }
    assert!(app.cell_target_at(window, pane, 0, 2).is_none());
    assert!(app.cell_target_at(window, pane, 1, 2).is_none());

    // Chain longer than the row bound: 150 cells on 4 columns wrap across 38 rows.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let long_chain = format!("https://example.test/{}", "a".repeat(150 - 21));
    let (window, pane) = wrapped_url_pane(&mut app, 4, 40, long_chain.as_bytes());
    assert!(app.cell_target_at(window, pane, 0, 2).is_none());
    assert!(app.cell_target_at(window, pane, 2, 2).is_none());

    // Flattened logical text beyond the 16 KiB scan bound, within the row bound.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let long = format!("https://example.test/{}", "a".repeat(16 * 1024));
    let (window, pane) = wrapped_url_pane(&mut app, 600, 30, long.as_bytes());
    assert!(app.cell_target_at(window, pane, 1, 2).is_none());
}

/// A wrapped OSC 8 label keeps its declared destination rather than its visible text.
#[test]
fn wrapped_osc8_label_keeps_declared_destination() {
    let uri = "https://example.com/declared";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let output = format!("\x1b]8;;{uri}\x1b\\{}\x1b]8;;\x1b\\", "label-".repeat(6));
    let (window, pane) = wrapped_url_pane(&mut app, 10, 8, output.as_bytes());
    for row in 0..3 {
        let target = app.cell_target_at(window, pane, row, 1).expect("OSC 8 fragment");
        assert_eq!(target.display, uri, "row {row}");
        assert!(target.explicit_hyperlink);
    }
}

/// A wrapped local file URI routes to the filesystem probe and keeps every fragment span.
#[test]
fn wrapped_file_uri_routes_to_path_probe() {
    let uri = if cfg!(windows) { "file:///C:/work/notes.txt" } else { "file:///tmp/notes.txt" };
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = wrapped_url_pane(&mut app, 12, 8, uri.as_bytes());
    let target = app.cell_target_at(window, pane, 1, 2).expect("file URI fragment");
    let ResolvedCellTarget::Path(key) = target.target else { panic!("file URI needs a probe") };
    assert_eq!(key.candidates[0].resolved_path.file_name().unwrap(), "notes.txt");
    assert!(key.candidates[0].spans.len() >= 2);
}

/// Rewriting a continuation row yields a fresh value instead of the previously resolved destination.
#[test]
fn mutated_continuation_never_reuses_stale_destination() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = wrapped_url_pane(&mut app, 21, 10, ISSUE_URI.as_bytes());
    assert_eq!(app.cell_target_at(window, pane, 0, 2).unwrap().display, ISSUE_URI);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[2;1H\x1b[2K"));
    let refreshed = app.cell_target_at(window, pane, 0, 2);
    assert!(!matches!(
        refreshed.as_ref().map(|snapshot| &snapshot.target),
        Some(ResolvedCellTarget::Uri(destination)) if destination == ISSUE_URI
    ));
}

// Emit `lines` as application hard rows (CRLF) into one pane `cols` wide and `rows` tall.
fn hardwrap_pane_sized(app: &mut App, cols: u16, rows: u16, lines: &[String]) -> (WindowId, u64) {
    let window = app.__test_seed_child_window(&["hard wrap"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, lines.join("\r\n").as_bytes()));
    (window, pane)
}

fn hardwrap_pane(app: &mut App, cols: u16, lines: &[String]) -> (WindowId, u64) {
    hardwrap_pane_sized(app, cols, 10, lines)
}

// Fill each non-final row exactly, leaving the closing wrapper on the final row.
fn hardwrap_lines(
    cols: u16,
    head: &str,
    uri: &str,
    indents: &[usize],
    closer: char,
) -> Vec<String> {
    let cols = usize::from(cols);
    assert!(head.is_ascii() && uri.is_ascii());
    assert!(head.len() < cols, "head must leave room for the first fragment");
    let (first, mut rest) = uri.split_at(cols - head.len());
    let mut lines = vec![format!("{head}{first}")];
    for (index, indent) in indents.iter().enumerate() {
        let pad = " ".repeat(*indent);
        if index + 1 == indents.len() {
            assert!(indent + rest.len() < cols, "final fragment and closer must fit");
            lines.push(format!("{pad}{rest}{closer}"));
        } else {
            assert!(rest.len() > cols - indent, "uri too short for {} rows", indents.len() + 1);
            let (chunk, tail) = rest.split_at(cols - indent);
            lines.push(format!("{pad}{chunk}"));
            rest = tail;
        }
    }
    lines
}

const HARDWRAP_HEAD: &str = "* Done: #1349 (";
const LONG_URI: &str = "https://example.com/aaaaaaaaaa/bbbbbbbbbb/cccccccccc/dddddddddd";

/// The reported shape resolves whole from either fragment, with spans excluding indent and wrappers.
#[test]
fn hardwrapped_parenthesized_url_resolves_complete_destination() {
    let lines = hardwrap_lines(39, HARDWRAP_HEAD, ISSUE_URI, &[2], ')');
    assert_eq!(lines[0].chars().count(), 39, "first fragment must reach the right edge");
    let start = HARDWRAP_HEAD.len() as u16;
    let tail = (ISSUE_URI.len() - (39 - HARDWRAP_HEAD.len())) as u16;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 39, &lines);
    assert!(
        !app.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(1)
            .unwrap()
            .soft_wrapped_from_previous(),
        "fixture must be an application hard break, not a terminal wrap"
    );
    for (row, col) in (start..39).map(|col| (0, col)).chain((2..2 + tail).map(|col| (1, col))) {
        let target = app.cell_target_at(window, pane, row, col).expect("hard-wrap fragment");
        assert_eq!(target.display, ISSUE_URI, "row {row}");
        assert!(
            matches!(&target.target, ResolvedCellTarget::Uri(destination) if destination == ISSUE_URI)
        );
        let cells = target.hovered(true).expect("highlight").cells;
        assert_eq!(cells.spans().len(), 2, "row {row}");
        assert_eq!((cells.spans()[0].start_col, cells.spans()[0].end_col), (start, 39));
        assert_eq!((cells.spans()[1].start_col, cells.spans()[1].end_col), (2, 2 + tail));
    }
    // The opener, the indent, and the closer are never part of the destination footprint.
    for (row, col) in [(0, start - 1), (1, 0), (1, 2 + tail)] {
        assert!(app.cell_target_at(window, pane, row, col).is_none(), "row {row} col {col}");
    }
}

/// Hard-row fragments share actual preview state, and a later rewrite cannot retain the old target.
#[test]
fn hardwrapped_preview_tracks_complete_fresh_destination() {
    let lines = hardwrap_lines(39, HARDWRAP_HEAD, ISSUE_URI, &[2], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.terminal.clickable_local_targets = false;
    app.config.terminal.clickable_bare_names = false;
    let (window, pane) = hardwrap_pane(&mut app, 39, &lines);
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    for (row, col) in [(0, 20), (1, 5)] {
        let target = app.cell_target_at(window, pane, row, col);
        app.apply_target_hover(window, target);
        let state = &app.windows[&window];
        assert_eq!(state.link_preview.as_ref().expect("modifier preview").uri, ISSUE_URI);
        let hover = state.hovered_url.as_ref().expect("underline input");
        assert_eq!(hover.url, ISSUE_URI);
        assert_eq!(hover.cells.spans().len(), 2);
    }
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[2;1H\x1b[2K"));
    let target = app.cell_target_at(window, pane, 0, 20);
    assert!(target.is_none());
    app.apply_target_hover(window, target);
    assert!(app.windows[&window].link_preview.is_none());
    assert!(app.windows[&window].hovered_url.is_none());
}

/// Percent-escapes, query text, and a hyphen sitting exactly at the cut survive joining byte-for-byte.
#[test]
fn hardwrapped_url_preserves_percent_query_and_hyphen_at_cut() {
    let uri = "https://example.com/a-b/c?x=%2Fy&z=1";
    let head = "x (";
    let cols = (head.len() + "https://example.com/a".len()) as u16;
    let lines = hardwrap_lines(cols, head, uri, &[2], ')');
    assert!(lines[0].ends_with("/a"), "cut must fall immediately before the hyphen");
    assert!(lines[1].starts_with("  -b/"), "continuation must begin with the hyphen");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, cols, &lines);
    assert_eq!(app.cell_target_at(window, pane, 1, 4).expect("continuation").display, uri);
}

/// Unicode prose before the opener shifts columns without rejecting the candidate.
#[test]
fn hardwrapped_url_allows_unicode_prose_prefix() {
    let cols = 39u16;
    // Two wide CJK cells occupy four columns; the ASCII surrogate preserves all six prefix columns.
    let mut lines = hardwrap_lines(cols, "done (", ISSUE_URI, &[2], ')');
    lines[0] = lines[0].replacen("done (", "完成 (", 1);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, cols, &lines);
    let target = app.cell_target_at(window, pane, 1, 4).expect("unicode-prefixed fragment");
    assert_eq!(target.display, ISSUE_URI);
}

/// Square brackets delimit the same way parentheses do, across three rows.
#[test]
fn hardwrapped_url_supports_square_brackets_and_three_rows() {
    let lines = hardwrap_lines(30, "x [", LONG_URI, &[2, 2], ']');
    assert_eq!(lines.len(), 3);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
    for row in 0..3 {
        let target = app.cell_target_at(window, pane, row, 5).expect("bracket fragment");
        assert_eq!(target.display, LONG_URI, "row {row}");
        assert_eq!(target.hovered(true).unwrap().cells.spans().len(), 3, "row {row}");
    }
}

/// HTTP supports the inclusive eight-row and eight-space indentation limits without changing bytes.
#[test]
fn hardwrapped_http_accepts_inclusive_bounds() {
    let uri = format!("http://example.test/{}", "a".repeat(169));
    let lines = hardwrap_lines(32, "x [", &uri, &[8; 7], ']');
    assert_eq!(lines.len(), 8);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 32, &lines);
    for row in 0..8 {
        let target = app.cell_target_at(window, pane, row, 10).expect("bounded HTTP fragment");
        assert_eq!(target.display, uri);
        assert_eq!(target.hovered(true).unwrap().cells.spans().len(), 8);
    }
}

/// A cut inside the authority is inert: the syntactically valid prefix must never activate.
#[test]
fn hardwrap_cut_before_path_delimiter_stays_inert() {
    let uri = "https://example.com/page";
    let head = "* see this one here (";
    let cols = (head.len() + "https://example.com".len()) as u16;
    let lines = hardwrap_lines(cols, head, uri, &[2], ')');
    assert!(lines[0].ends_with("https://example.com"), "cut must precede the path delimiter");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, cols, &lines);
    assert!(
        app.cell_target_at(window, pane, 0, head.len() as u16 + 5).is_none(),
        "truncated authority must be inert"
    );
}

/// A slash inside a query is not the path delimiter required before a hard cut.
#[test]
fn hardwrap_requires_path_slash_before_query_or_fragment() {
    for uri in ["https://example.com?next=/abcde", "https://example.com#name/abcde"] {
        let lines = hardwrap_lines(29, "x (", uri, &[2], ')');
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane(&mut app, 29, &lines);
        assert!(app.cell_target_at(window, pane, 0, 5).is_none(), "{uri}");
    }
}

/// A recorded terminal wrap keeps its own chain, including a scheme or authority split.
#[test]
fn hardwrap_never_shadows_a_softwrapped_chain() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = wrapped_url_pane(&mut app, 12, 10, format!("x ({ISSUE_URI})").as_bytes());
    assert!(app.windows[&window].panes[&pane]
        .parser
        .lock()
        .grid()
        .row_at_abs(1)
        .unwrap()
        .soft_wrapped_from_previous());
    assert_eq!(
        app.cell_target_at(window, pane, 1, 2).expect("soft wrap owns it").display,
        ISSUE_URI
    );
}

/// An incomplete candidate suppresses only its own fragments, never unrelated prose or URLs.
#[test]
fn hardwrap_incomplete_does_not_consume_unrelated_targets() {
    let other = "https://other.test/ok";
    let mut lines = hardwrap_lines(39, HARDWRAP_HEAD, ISSUE_URI, &[2], ')');
    lines[1] = lines[1].replace(')', "");
    lines.push(format!("see {other} here"));
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 39, &lines);
    assert!(app.cell_target_at(window, pane, 0, HARDWRAP_HEAD.len() as u16 + 5).is_none());
    // The independent URL after the malformed continuation keeps its own resolution.
    assert_eq!(app.cell_target_at(window, pane, 2, 8).expect("independent URL").display, other);
}

/// Continuation rows must share one indent; an unequal one cannot be joined or truncated.
#[test]
fn hardwrap_unequal_indent_stays_inert() {
    let lines = hardwrap_lines(30, "x (", LONG_URI, &[2, 4], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
    assert!(app.cell_target_at(window, pane, 0, 5).is_none());
    let parser = app.windows[&window].panes[&pane].parser.lock();
    assert!(
        matches!(
            hardwrap_uri_at_cell(parser.grid(), 0, AbsoluteCell { row: 2, col: 5 }),
            HardwrapUri::NotApplicable
        ),
        "a differently indented row is not owned by the prior wrapper"
    );
}

/// A second scheme separator in the wrapper is never merged into one destination.
#[test]
fn hardwrap_second_scheme_is_never_joined() {
    let joined = format!("{LONG_URI}https://b.test/y");
    let lines = hardwrap_lines(30, "x (", &joined, &[2, 2], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
    let found = app.cell_target_at(window, pane, 1, 5);
    assert!(!matches!(
        found.as_ref().map(|snapshot| &snapshot.target),
        Some(ResolvedCellTarget::Uri(destination)) if destination == &joined
    ));
}

/// Hard rows without an explicit opener keep their existing row-local behavior and never join.
#[test]
fn hardwrap_without_wrapper_never_joins() {
    let head = "* Done: #1349 ";
    let (first, rest) = ISSUE_URI.split_at(39 - head.len());
    let lines = vec![format!("{head}{first}"), format!("  {rest}")];
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 39, &lines);
    let found = app.cell_target_at(window, pane, 0, 20);
    assert!(!matches!(
        found.as_ref().map(|snapshot| &snapshot.target),
        Some(ResolvedCellTarget::Uri(destination)) if destination == ISSUE_URI
    ));
}

/// A wrapper closed on its own row is ordinary text that existing single-row detection owns.
#[test]
fn hardwrap_closer_on_first_row_keeps_existing_detection() {
    let uri = "https://a.test/b";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 39, &[format!("x ({uri}) more")]);
    assert_eq!(app.cell_target_at(window, pane, 0, 6).expect("single row URI").display, uri);
}

/// OSC 8 decides before any wrapper scan and keeps its declared destination.
#[test]
fn hardwrap_scan_leaves_osc8_authoritative() {
    let declared = "https://example.com/declared";
    let label = "label-".repeat(6);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let output = format!("x (\x1b]8;;{declared}\x1b\\{label}\x1b]8;;\x1b\\)");
    let (window, pane) = hardwrap_pane(&mut app, 39, &[output]);
    let target = app.cell_target_at(window, pane, 0, 6).expect("OSC 8 fragment");
    assert_eq!(target.display, declared);
    assert!(target.explicit_hyperlink);
}

/// File URIs keep soft-wrap and filesystem handling; hard rows never infer a joined path.
#[test]
fn hardwrap_file_uri_is_never_joined() {
    let uri = "file:///tmp/aaaaaaaa/bbbbbbbb/cccccccc/dddddddd";
    let lines = hardwrap_lines(30, "x (", uri, &[2], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
    let found = app.cell_target_at(window, pane, 1, 4);
    assert!(found.as_ref().is_none_or(|target| target.display != uri));
}

/// Row, byte, and viewport bounds all refuse rather than truncating.
#[test]
fn hardwrap_bounds_and_offscreen_stay_inert() {
    // More rows than the bound: head carries a path delimiter so the authority is never the reason.
    let head = "x (https://example.test/";
    let long = format!("https://example.test/{}", "abcdefghij/".repeat(114));
    let lines = hardwrap_lines(40, "x (", &long, &[2; 33], ')');
    assert!(lines.len() > MAX_WRAPPED_PATH_ROWS, "fixture must exceed the row bound");
    assert!(lines[0].starts_with(head), "first fragment must already hold a path delimiter");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 40, 40, &lines);
    assert!(app.cell_target_at(window, pane, 0, 10).is_none());

    // Joined text beyond the 4 KiB bound, within the row bound.
    let huge = format!("https://example.test/{}", "a".repeat(5000));
    let lines = hardwrap_lines(900, "x (", &huge, &[2; 5], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane(&mut app, 900, &lines);
    assert!(app.cell_target_at(window, pane, 0, 10).is_none());

    // Closer below the visible viewport.
    let lines = hardwrap_lines(30, "x (", LONG_URI, &[2, 2], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 30, 2, &lines);
    app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().viewport_top_abs = Some(0);
    {
        let parser = app.windows[&window].panes[&pane].parser.lock();
        assert_eq!(GpuRenderer::resolved_view_top_abs_legacy(parser.grid(), Some(0)), 0);
        assert!(parser.grid().row_at_abs(2).unwrap().iter().any(|cell| cell.ch == ')'));
    }
    assert!(app.cell_target_at(window, pane, 0, 5).is_none());

    // Opener scrolled above the viewport: the visible continuation cannot prove its head.
    let lines = hardwrap_lines(30, "x (", LONG_URI, &[2, 2], ')');
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 30, 2, &lines);
    let view_top = {
        let parser = app.windows[&window].panes[&pane].parser.lock();
        GpuRenderer::resolved_view_top_abs_legacy(parser.grid(), None)
    };
    assert!(view_top > 0, "opener must have scrolled above the viewport");
    assert!(app.cell_target_at(window, pane, 1, 5).is_none());
}

/// An internal closer followed by URL text cannot authorize a shorter reconstructed destination.
#[test]
fn hardwrap_closer_requires_a_prose_boundary() {
    for (opener, closer) in [('(', ')'), ('[', ']')] {
        let head = format!("x {opener}");
        for suffix in ["cc", "/tail", "?query", "%2F", ".html", "界", "\u{301}"] {
            let uri = "https://example.com/aaaaaaabb";
            let mut lines = hardwrap_lines(30, &head, uri, &[2], closer);
            assert_eq!(lines[1], format!("  bb{closer}"));
            lines[1] = format!("  bb{closer}{suffix}{closer}");
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
            for (row, col) in [(0, 5), (1, 2)] {
                assert!(app.cell_target_at(window, pane, row, col).is_none(), "{suffix} at {row}");
            }
        }
    }
}

/// Closing wrappers followed by prose punctuation retain the full URI from every fragment.
#[test]
fn hardwrap_closer_accepts_prose_punctuation() {
    let uri = "https://example.com/aaaaaaabb";
    for suffix in ["", " ", ".", ", next", "; next", ": next", "!", "?"] {
        let mut lines = hardwrap_lines(30, "x (", uri, &[2], ')');
        lines[1].push_str(suffix);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane(&mut app, 30, &lines);
        for (row, col) in [(0, 5), (1, 2)] {
            assert_eq!(app.cell_target_at(window, pane, row, col).expect("URI").display, uri);
        }
    }
}

/// zsh repaints a wrapped command line with CR + EL0 and recolors the command word; the soft wrap
/// survives, so a home path split across the two rows resolves whole from either row.
#[test]
fn zsh_redrawn_wrapped_home_path_resolves_from_both_rows() {
    let root = native_test_root().join(format!(
        "sonicterm-zsh-wrap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(".claude.json"), "inert fixture").unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.home_dir = Some(root.clone());
    let window = app.__test_seed_child_window(&["zsh"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(40, 4);
    // Bytes zsh sends at 40 columns: the wrapping space, CR + EL0, the repainted continuation
    // row, then syntax highlighting rewriting `echo` on the row above.
    let typed = "> echo 同步时会将客户端设置写入 ~/.claud \r\x1b[Ke\re.json，并将权限\
                 \x1b[1;3H\x1b[34mecho\x1b[39m\x1b[2;14H";
    assert!(app.__test_advance_child_pane_parser(window, pane, typed.as_bytes()));
    for (row, col) in [(0, 37), (1, 2)] {
        let snapshot = app.cell_target_at(window, pane, row, col).expect("wrapped home path");
        let ResolvedCellTarget::Path(key) = snapshot.target else { panic!("path") };
        let selection = probe_candidates(&key.candidates, classify_local_target)
            .unwrap_or_else(|reason| panic!("row {row} col {col}: {reason}"));
        assert_eq!(selection.candidate.resolved_path, root.join(".claude.json"));
        assert_eq!(selection.candidate.spans.len(), 2, "row {row} col {col}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A multiplexer positions each pane row with a cursor move, so a path that wraps at a pane's right
/// edge has no recorded wrap; on the alternate screen it still resolves whole from either row, at
/// the grid's edge and at a split border.
#[test]
fn multiplexer_wrapped_paths_resolve_from_either_row() {
    let path = native_path("tmp/abcdefghijklmnopqrstuvwxyz/x.txt");
    let split_at = 20;
    let (head, tail) = path.split_at(split_at);
    // The tail fits inside the 20-column pane so the split border stays drawn on both rows.
    let full = format!("\x1b[?1049h\x1b[1;1H{head}\x1b[2;1H{tail}");
    let mut split = format!("\x1b[?1049h{}", pane_border(21, 4));
    split.push_str(&format!("\x1b[1;1H{head}\x1b[2;1H{tail}"));
    for (cols, fed) in [(20, &full), (41, &split)] {
        for (row, col) in [(0, 3), (1, 2)] {
            let target = target_after(cols, 4, fed, row, col)
                .unwrap_or_else(|| panic!("cols {cols} row {row}: nothing resolved"));
            let ResolvedCellTarget::Path(key) = target.target else { panic!("path") };
            assert!(
                key.candidates.iter().any(|candidate| candidate.display() == path),
                "cols {cols} row {row}"
            );
            assert!(
                key.candidates.iter().all(|candidate| candidate.display() != head),
                "cols {cols} row {row}: the cut-off head must not be offered"
            );
        }
    }
}

/// `clickable_path_max_chars` drops candidates longer than the cap, counted in Unicode scalars.
#[test]
fn clickable_path_max_chars_limits_candidates() {
    let path = native_path("tmp/同步/report.txt");
    let path_chars = path.chars().count();
    let output = format!("{path} done");
    for (cap, offered) in [(path_chars, true), (path_chars - 1, false)] {
        let mut config = Config::default();
        config.terminal.clickable_path_max_chars = cap;
        let mut app = App::new(Theme::default(), config, Keymap::default());
        let window = app.__test_seed_child_window(&["cap"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        let offered_path = app.cell_target_at(window, pane, 0, 2).is_some_and(|target| {
            matches!(&target.target, ResolvedCellTarget::Path(key)
                if key.candidates.iter().any(|candidate| candidate.display() == path))
        });
        assert_eq!(offered_path, offered, "cap {cap}");
    }
}

/// The open worker repeats the hover-time selection over its contenders, so a contender that
/// appeared, became blocked, or tied since hover stops the open instead of opening a longer path.
#[test]
fn open_request_repeats_selection_over_contenders() {
    let short = probe_candidate("a.txt", "/work/a.txt", 0);
    let chosen = probe_candidate("a.txt b", "/work/a.txt b", 0);
    let request = PathOpenRequest {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        path: chosen.resolved_path.clone(),
        expected_decision: PathOpenDecision::Openable(PathKind::File),
        contenders: vec![chosen.clone(), short.clone()],
    };
    let classify = |short_decision: PathOpenDecision| {
        move |path: &Path| {
            if path == Path::new("/work/a.txt") {
                short_decision
            } else {
                PathOpenDecision::Openable(PathKind::File)
            }
        }
    };
    assert!(open_request_still_selected(&request, classify(PathOpenDecision::Missing)));
    assert!(!open_request_still_selected(
        &request,
        classify(PathOpenDecision::Openable(PathKind::File))
    ));
    assert!(!open_request_still_selected(&request, classify(PathOpenDecision::Blocked)));
    // An equal-length contender that starts earlier now wins the tie.
    let mut tied = request.clone();
    tied.contenders.push(probe_candidate("x.txt b", "/work/x.txt b", 0));
    tied.contenders[0].spans[0].start_col = 1;
    tied.contenders[0].spans[0].end_col = 8;
    assert!(!open_request_still_selected(&tied, classify(PathOpenDecision::Missing)));
}

/// A 1023-character URL soft-wrapped across 13 rows resolves whole from any row and underlines
/// every row, within the row and fragment caps.
#[test]
fn long_wrapped_url_resolves_and_underlines_every_row() {
    let prefix = "http://example.com/path?data=";
    let url = format!("{prefix}{}", "a".repeat(1023 - prefix.len()));
    for (row, col) in [(0, 5), (6, 40), (12, 3)] {
        let target = target_after(82, 16, &url, row, col).unwrap();
        assert_eq!(target.target, ResolvedCellTarget::Uri(url.clone()), "row {row}");
        assert_eq!(target.hover_cells.unwrap().spans().len(), 13, "row {row}");
    }
}

/// tmux redraws a wrapped line row by row with CR LF on the alternate screen, recording no soft
/// wrap; the pane-edge join still resolves the whole URL and underlines every row.
#[test]
fn tmux_redrawn_long_url_resolves_on_the_alternate_screen() {
    let prefix = "http://example.com/path?data=";
    let url = format!("{prefix}{}", "a".repeat(1023 - prefix.len()));
    let rows = url.as_bytes().chunks(82).map(|row| std::str::from_utf8(row).unwrap());
    let output = format!("\x1b[?1049h\x1b[H{}", rows.collect::<Vec<_>>().join("\r\n"));
    for (row, col) in [(0, 5), (6, 40), (12, 3)] {
        let target = target_after(82, 16, &output, row, col).unwrap();
        assert_eq!(target.target, ResolvedCellTarget::Uri(url.clone()), "row {row}");
        assert_eq!(target.hover_cells.unwrap().spans().len(), 13, "row {row}");
    }
}

/// An app such as Claude Code wraps a long OSC 8 link itself with a hanging indent and a one-cell
/// right margin; on the alternate screen both rows underline from either fragment, with or
/// without tmux popup borders around the pane.
#[test]
fn hanging_indent_osc8_link_underlines_both_rows() {
    let link = "\x1b]8;;https://fr.wikipedia.org/wiki/Llanfair\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let head = "a".repeat(27);
    let plain = format!(
        "\x1b[?1049h\x1b[1;1H*\x1b[1;3H{link}{head}{close}\x1b[2;3H{link}bbbbbbbbbbbb{close}"
    );
    for (row, col) in [(1, 5), (0, 5)] {
        assert_eq!(
            hover_spans_after(30, 4, &plain, row, col),
            [hover_span(0, 2, 29), hover_span(1, 2, 14)],
            "row {row}"
        );
    }
    let mut popup = String::from("\x1b[?1049h");
    for row in 1..=4 {
        popup.push_str(&format!("\x1b[{row};1H\u{2502}\x1b[{row};32H\u{2502}"));
    }
    popup.push_str(&format!(
        "\x1b[1;2H*\x1b[1;4H{link}{head}{close}\x1b[2;4H{link}bbbbbbbbbbbb{close}"
    ));
    assert_eq!(
        hover_spans_after(32, 4, &popup, 1, 6),
        [hover_span(0, 3, 30), hover_span(1, 3, 15)]
    );
}

/// An equal-length blocked candidate vetoes an earlier actionable one in the same tier.
#[test]
fn equal_length_blocked_candidate_vetoes_earlier_actionable_one() {
    let candidates = vec![
        probe_candidate("Left Name", "/work/Left Name", 0),
        probe_candidate("Name Here", "/work/Name Here", 5),
    ];
    let verdict = probe_candidates(&candidates, |path| {
        if path.ends_with("Name Here") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    });
    assert_eq!(verdict, Err("path-error-blocked"));
}

/// An inferred pane-edge join cannot reach a row whose own recorded wrap came from a row that has
/// scrolled away: hovering the row below refuses the text, as hovering that row does.
#[test]
fn inferred_joins_cannot_reach_an_incomplete_recorded_chain() {
    let path = native_path("tmp/filex");
    let cols = u16::try_from(path.chars().count()).unwrap();
    // The prefix fills row 0, the path wraps into row 1, `more` follows a hard line, and the final
    // line feed scrolls the prefix off the alternate screen.
    let output = format!("\x1b[?1049h\x1b[H{}{path}\r\nmore \n", "p".repeat(usize::from(cols)));
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["incomplete"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, 3);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    {
        let parser = app.windows[&window].panes[&pane].parser.lock();
        assert!(parser.grid().row(0).soft_wrapped_from_previous(), "row 0 keeps its unproven wrap");
    }
    assert!(app.cell_target_at(window, pane, 0, 2).is_none());
    assert!(app.cell_target_at(window, pane, 1, 1).is_none());
}

/// A wrap the terminal recorded from a right pane into the next row's left pane shows where that
/// pane's text went, so no inferred join links it to the right pane's next row, from either row.
#[test]
fn recorded_cross_pane_wraps_block_inferred_joins() {
    let mut output = format!("\x1b[?1049h{}", pane_border(21, 4));
    // The URL overflows the 20-column right pane, so `abcde` wraps into the next row's left pane.
    output.push_str("\x1b[1;22Hhttps://example.com/abcde\x1b[2;22Hmore");
    let joined = ResolvedCellTarget::Uri("https://example.com/more".into());
    for (row, col) in [(0, 25), (1, 22)] {
        let found = target_after(41, 4, &output, row, col);
        assert!(found.is_none_or(|target| target.target != joined), "row {row}");
    }
}

/// A rule or border glyph between two fragments of a link is not blank, so it ends the right margin
/// or the hanging indent and the fragments keep separate underlines.
#[test]
fn osc8_continuation_needs_blank_margins() {
    let link = "\x1b]8;;https://example.com/rule\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let head = "a".repeat(27);
    let after =
        format!("\x1b[?1049h\x1b[1;3H{link}{head}{close}\u{2500}\x1b[2;3H{link}bbbb{close}");
    assert_eq!(hover_spans_after(30, 4, &after, 0, 5), [hover_span(0, 2, 29)]);
    let before =
        format!("\x1b[?1049h\x1b[1;3H{link}{head}{close}\x1b[2;1H\u{2502} {link}bbbb{close}");
    assert_eq!(hover_spans_after(30, 4, &before, 1, 3), [hover_span(1, 2, 6)]);
}

/// Lines written into the trimmed-history grid; history keeps the last 1,000 that scrolled out.
const TRIMMED_HISTORY_LINES: usize = 1_100;

/// Generated line `line`: words, CJK wide pairs, a URL, a path, or a prompt with a coloured tail.
fn trimmed_history_line(line: usize) -> String {
    match line % 5 {
        0 => format!("alpha beta {line} gamma"),
        1 => format!("中文{line}字"),
        2 => format!("see https://example.com/page{line} now"),
        3 => format!("edit ./src/file{line}.rs now"),
        _ => format!("prompt{line}$"),
    }
}

/// A 200x24 grid whose 1,000 history rows are the generated short lines, stored trimmed.
fn grid_with_trimmed_history() -> Grid {
    let coloured = Cell::plain(' ', Color::Default, Color::Indexed(4), CellFlags::empty());
    let mut grid = Grid::new(200, 24);
    for line in 0..TRIMMED_HISTORY_LINES {
        for character in trimmed_history_line(line).chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        if line % 5 == 4 {
            grid.erase_line_to_end_with(coloured.clone());
        }
        grid.carriage_return();
        grid.linefeed();
    }
    grid
}

/// Path and URL detection on trimmed history rows resolve the generated targets with their exact
/// cell spans, through the single-row lookups and the logical-line scan, as on flat rows.
#[test]
fn path_and_url_detection_read_trimmed_history_rows() {
    let grid = grid_with_trimmed_history();
    assert_eq!(grid.scrollback_len(), 1_000);
    assert!(grid.scrollback_trimmed_rows() >= 900, "precondition: history must be trimmed");
    let first_history_line = TRIMMED_HISTORY_LINES - 23 - 1_000;
    let mut checked = [0usize; 2];
    for row in 0..1_000usize {
        let line = first_history_line + row;
        let history_row = grid.row_at_abs(row as u64).expect("history row");
        assert!(history_row.is_trimmed(), "row {row} is stored trimmed");
        match line % 5 {
            2 => {
                let url = format!("https://example.com/page{line}");
                let found = target_at_row_cell(history_row, 10, PathStyle::Posix).expect("URL");
                assert_eq!(found.matched.target, DetectedTarget::Uri(url.clone()));
                assert_eq!((found.start_col, usize::from(found.end_col)), (4, 4 + url.len()));
                checked[0] += 1;
            }
            3 => {
                let path = format!("./src/file{line}.rs");
                let found = target_at_row_cell(history_row, 9, PathStyle::Posix).expect("path");
                assert_eq!(found.matched.target, DetectedTarget::PathCandidate(path.clone()));
                assert_eq!((found.start_col, usize::from(found.end_col)), (5, 5 + path.len()));
                let candidates =
                    row_target_candidates_at_cell(history_row, 9, PathStyle::Posix, false);
                assert!(candidates.iter().any(|candidate| candidate.start_col == 5
                    && candidate.matched.target == DetectedTarget::PathCandidate(path.clone())));
                let pointed = AbsoluteCell { row: row as u64, col: 9 };
                let scan =
                    logical_path_scan_at_cell(&grid, row as u64, pointed, PathStyle::Posix, false)
                        .expect("logical scan of a history row");
                assert!(scan.candidates.iter().any(
                    |candidate| candidate.target == DetectedTarget::PathCandidate(path.clone())
                ));
                checked[1] += 1;
            }
            _ => {
                assert!(
                    target_at_row_cell(history_row, 2, PathStyle::Posix).is_none(),
                    "row {row}"
                );
            }
        }
    }
    assert_eq!(checked, [200, 200], "every URL and path row was checked");
}

/// The absolute row whose text starts with `prefix`, searched over history and the live screen.
fn absolute_row_starting_with(grid: &Grid, prefix: &str) -> u64 {
    let stored_rows = grid.scrollback_len() as u64 + u64::from(grid.rows);
    (0..stored_rows)
        .find(|row| {
            grid.row_at_abs(*row).is_some_and(|line| {
                line.iter().map(|cell| cell.ch).collect::<String>().starts_with(prefix)
            })
        })
        .expect("a row with the prefix")
}

/// A child-window pane of `cols` × `rows` cells that has parsed `output`.
fn pane_after(cols: u16, rows: u16, output: &str) -> (App, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["frame scope"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    (app, window, pane)
}

/// Detection reads only the displayed frame: from every cell of every scrolled-back view over
/// recorded wraps, an OSC 8 link and a hard-wrapped bracketed URL that cross both frame edges, and
/// from every cell of an alternate screen whose pane borders reach its bottom row, no row outside
/// `[view_top, view_top + rows)` is read, not even the wrap flag or border glyph of a neighbour.
#[test]
fn detection_reads_no_row_outside_the_displayed_frame() {
    let link = "\x1b]8;;https://example.com/linked\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let mut output = String::new();
    for index in 0..12 {
        match index % 3 {
            // 30 characters on 20 columns: a recorded wrap into the next row.
            0 => output.push_str("/tmp/a/b/c/d/e/f/g/h/i/j/k.txt"),
            // An OSC 8 label that wraps with the row.
            1 => output.push_str(&format!("{link}linked-label-that-wraps-on{close}")),
            // A bracketed URL a program wrapped itself, continued on a hard line.
            _ => output.push_str("(https://ex.co/aaaaaaaaaa\r\n/bbbb) tail"),
        }
        output.push_str("\r\n");
    }
    let (app, window, pane) = pane_after(20, 4, &output);
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let history_rows = guard.grid().scrollback_len() as u64;
    assert!(history_rows > 8, "the fixture scrolls rows into history");
    for view_top in 0..=history_rows {
        for row in 0..4 {
            for col in 0..20 {
                let (_, reads) = frame_rows::read_observer::observe(|| {
                    app.cell_target_from_parser(window, pane, row, col, &guard, Some(view_top))
                });
                let outside = reads.iter().filter(|(_, in_frame)| !in_frame).collect::<Vec<_>>();
                assert!(outside.is_empty(), "view {view_top} cell ({row}, {col}) read {outside:?}");
            }
        }
    }
    drop(guard);

    // On the alternate screen a split border runs to the bottom row and text fills both panes.
    let screen = format!(
        "\x1b[?1049h{}\x1b[1;1Hhttps://ex.co/aaaaaaaa\x1b[2;1Hmore/path\x1b[4;1H/tmp/left/side.txt\x1b[4;23H/tmp/right.txt",
        pane_border(21, 4)
    );
    let (app, window, pane) = pane_after(41, 4, &screen);
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    for row in 0..4 {
        for col in 0..41 {
            let (_, reads) = frame_rows::read_observer::observe(|| {
                app.cell_target_from_parser(window, pane, row, col, &guard, None)
            });
            let outside = reads.iter().filter(|(_, in_frame)| !in_frame).collect::<Vec<_>>();
            assert!(outside.is_empty(), "alternate cell ({row}, {col}) read {outside:?}");
        }
    }
}

/// A target on the bottom row of a scrolled-back view may continue on the row below, which is not
/// displayed, so it is refused without reading that row, whether or not the row below records a
/// wrap. The same row away from the edge, and on the live view's bottom row, keeps its target.
#[test]
fn a_bottom_edge_target_of_a_scrolled_back_view_is_refused_without_reading_below() {
    for (label, line, pointed_col) in [
        // The row below is a recorded continuation of this path.
        ("wrapped", "/tmp/abcdefghijklmnopqrstuvwxyz.txt", 3_u16),
        // The row below is unrelated text.
        ("complete", "/tmp/notes.txt", 3),
    ] {
        let output = format!("one\r\ntwo\r\n{line}\r\nnext\r\nfour\r\nfive\r\nsix\r\nseven");
        let (app, window, pane) = pane_after(20, 3, &output);
        let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
        let guard = parser.lock();
        let path_row = absolute_row_starting_with(guard.grid(), &line[..10]);
        assert!(path_row + 2 < guard.grid().scrollback_len() as u64, "{label}: in history");
        // Bottom edge: the path is the last of three displayed rows.
        let (target, reads) = frame_rows::read_observer::observe(|| {
            app.cell_target_from_parser(window, pane, 2, pointed_col, &guard, Some(path_row - 2))
        });
        assert!(target.is_none(), "{label}: a bottom-edge target is refused");
        assert!(reads.iter().all(|(_, in_frame)| *in_frame), "{label}: {reads:?}");
        // One row up the path is the middle row, so the row below is displayed and read.
        if label == "complete" {
            let target = app
                .cell_target_from_parser(window, pane, 1, pointed_col, &guard, Some(path_row - 1))
                .expect("the same path away from the edge");
            assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
        }
    }
    // On the live view no row exists below the bottom row, so the bottom-row target stays.
    let (app, window, pane) = pane_after(20, 3, "one\r\ntwo\r\n/tmp/notes.txt");
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let target =
        app.cell_target_from_parser(window, pane, 2, 3, &guard, None).expect("live bottom row");
    assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
}

/// An App that counts hover work, with an injected probe mailbox the test drains itself, and one
/// child window whose pane is `cols` × `rows` cells.
fn counted_hover_app(cols: u16, rows: u16) -> (App, WindowId, u64, PathProbeMailbox) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let (mailbox, _wake) = PathProbeMailbox::new();
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers { probe: mailbox.clone(), open });
    let window = app.__test_seed_child_window(&["hover cache"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    (app, window, pane, mailbox)
}

/// One hover lookup of `pane`'s cell (`row`, `col`) as a frame runs it from its held parser, at
/// the pane's own viewport or at `viewport`, then the apply that always follows.
fn frame_hover_at(
    app: &mut App,
    window: WindowId,
    pane: u64,
    (row, col): (u16, u16),
    viewport: Option<u64>,
) {
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let viewport =
        viewport.or_else(|| app.windows[&window].panes[&pane].resolved_viewport(guard.grid()));
    let target = app.hover_target_from_parser(window, pane, row, col, &guard, viewport);
    drop(guard);
    app.apply_target_hover(window, target);
}

/// [`frame_hover_at`] at the pane's own viewport.
fn frame_hover(app: &mut App, window: WindowId, pane: u64, cell: (u16, u16)) {
    frame_hover_at(app, window, pane, cell, None);
}

/// The App's hover counters as `(lookups, detections, cache_hits)`.
fn hover_counts(app: &App) -> (u64, u64, u64) {
    let hover = app.frame_counters.as_ref().expect("counters are on").hover;
    (hover.lookups, hover.detections, hover.cache_hits)
}

/// Probe requests hover submitted since the last drain; only the POSIX-path tests submit probes.
#[cfg(unix)]
fn drained_probes(mailbox: &PathProbeMailbox) -> usize {
    std::iter::from_fn(|| mailbox.take_next()).count()
}

/// Hold or release the platform's open modifier in `window`.
fn set_open_modifier(app: &mut App, window: WindowId, held: bool) {
    let modifier = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    app.windows.get_mut(&window).unwrap().modifiers =
        if held { modifier } else { winit::keyboard::ModifiersState::empty() };
}

/// With the pointer still and the rows detection read unchanged while another row streams, every
/// frame after the first reuses the cached result: zero further detections and zero further probe
/// submissions, counted through the injected mailbox.
#[cfg(unix)]
#[test]
fn a_still_pointer_over_unchanged_rows_detects_once_while_other_rows_stream() {
    let (mut app, window, pane, mailbox) = counted_hover_app(40, 6);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"/tmp/sonicterm-cache/notes.txt"));
    for frame in 0..20 {
        frame_hover(&mut app, window, pane, (0, 3));
        let streamed = format!("\x1b[5;1H{frame:>4} streamed");
        assert!(app.__test_advance_child_pane_parser(window, pane, streamed.as_bytes()));
    }
    assert_eq!(hover_counts(&app), (20, 1, 19));
    assert_eq!(drained_probes(&mailbox), 1, "only the first frame submits a probe");
    assert_eq!(app.frame_counters.as_ref().unwrap().hover.probes_submitted, 1);
}

/// A change to one row detection read causes exactly one detection on the next lookup, and the
/// lookup after it reuses that result.
#[cfg(unix)]
#[test]
fn one_dependency_row_change_detects_once_then_reuses() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 6);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"/tmp/sonicterm-cache/notes.txt"));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1));
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[1;36Hx"));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the changed row detects again");
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (4, 2, 2), "and the next lookup reuses it");
}

/// Same-cell motion and a modifier change hit the cache, through both the frame path and the
/// event-time lookup, while the apply that follows still flips the underline's `active` state.
#[test]
fn same_cell_motion_and_modifier_changes_hit_the_cache_and_still_apply() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    let active = |app: &App| app.windows[&window].hovered_url.as_ref().unwrap().active();
    frame_hover(&mut app, window, pane, (0, 3));
    assert!(!active(&app));
    set_open_modifier(&mut app, window, true);
    let lookup = app.hover_lookup(window, pane, 0, 3);
    app.apply_target_lookup(window, lookup);
    assert!(active(&app), "the modifier activates the cached target");
    set_open_modifier(&mut app, window, false);
    frame_hover(&mut app, window, pane, (0, 3));
    assert!(!active(&app), "releasing it deactivates the cached target");
    assert_eq!(hover_counts(&app), (3, 1, 2));
}

/// A probe that completes for an unchanged key is applied by the frame it requests with no new
/// detection and no duplicate probe, and a same-value repaint of the target's row detects once and
/// yields an equal key, so the authorization stands.
#[cfg(unix)]
#[test]
fn probe_completion_and_same_value_repaint_keep_the_cached_authorization() {
    let root = native_test_root().join(format!(
        "sonicterm-hover-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("notes.txt");
    std::fs::write(&path, b"notes").unwrap();
    let (mut app, window, pane, mailbox) = counted_hover_app(200, 4);
    let text = path.to_str().unwrap().to_owned();
    assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    let request = mailbox.take_next().expect("the first frame submits a probe");
    let outcome = probe_candidates(&request.key.candidates, classify_local_target);
    let key = request.key.clone();
    app.handle_path_probe_finished(PathProbeResult {
        failure: outcome.as_ref().err().copied(),
        selection: outcome.ok(),
        request,
    });
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "the completion's frame reuses the detection");
    assert_eq!(drained_probes(&mailbox), 0, "and submits no duplicate probe");
    assert!(app.windows[&window].path_probe.authorized(&key, true));
    let repaint = format!("\x1b[1;1H{text}");
    assert!(app.__test_advance_child_pane_parser(window, pane, repaint.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "a same-value repaint detects once");
    let fresh = match app.cell_target_at(window, pane, 0, 3).unwrap().target {
        ResolvedCellTarget::Path(fresh) => fresh,
        other => panic!("a path target, not {other:?}"),
    };
    assert_eq!(fresh, key, "the recomputed key is equal");
    assert!(app.windows[&window].path_probe.authorized(&key, true), "authorization stands");
    assert_eq!(drained_probes(&mailbox), 0);
    std::fs::remove_dir_all(root).unwrap();
}

/// Every input of a detection invalidates the cache on its own: each change below is followed by
/// one lookup of the same cell, which must detect again. A theme change is not an input, so the
/// lookup after it hits.
#[test]
fn each_detection_input_invalidates_the_cache() {
    type Change = fn(&mut App, WindowId, u64);
    let changes: [(&str, Change); 10] = [
        ("resize", |app, window, pane| {
            app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(41, 4);
        }),
        ("alt-screen enter", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[?1049h"));
        }),
        ("cursor leave", |app, window, _| app.clear_target_hover(window)),
        ("pane hover invalidation", |app, window, _| {
            app.windows.get_mut(&window).unwrap().invalidate_path_hover();
        }),
        ("clickable_local_targets", |app, _, _| {
            app.config.terminal.clickable_local_targets ^= true;
        }),
        ("clickable_bare_names", |app, _, _| app.config.terminal.clickable_bare_names ^= true),
        ("clickable_path_max_chars", |app, _, _| app.config.terminal.clickable_path_max_chars = 9),
        ("cwd revision", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(
                window,
                pane,
                b"\x1b]7;file:///tmp\x1b\\"
            ));
        }),
        ("dependency row write", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[1;30Hz"));
        }),
        ("history clear", |app, window, pane| {
            app.windows[&window].panes[&pane].parser.lock().grid_mut().clear_scrollback();
        }),
    ];
    for (label, change) in changes {
        let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
        // Two rows of history, so the history clear has rows to drop.
        let output = b"one\r\ntwo\r\nhttps://example.com/\r\nfour\r\nfive";
        assert!(app.__test_advance_child_pane_parser(window, pane, output));
        frame_hover(&mut app, window, pane, (0, 3));
        frame_hover(&mut app, window, pane, (0, 3));
        assert_eq!(hover_counts(&app), (2, 1, 1), "{label}: warm");
        change(&mut app, window, pane);
        frame_hover(&mut app, window, pane, (0, 3));
        assert_eq!(hover_counts(&app).1, 2, "{label}: detects again");
    }
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    frame_hover(&mut app, window, pane, (0, 3));
    app.theme = Theme::default();
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "a theme change keeps the cached result");
}

/// Leaving the alternate screen invalidates the cache: the primary screen's rows are back on
/// display, so the next lookup detects again and resolves the primary screen's URL.
#[test]
fn alt_screen_exit_invalidates_the_cache() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let output = b"https://primary.example/\x1b[?1049h\x1b[1;1Hhttps://alternate.example/";
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "warm on the alternate screen");
    assert_eq!(
        app.windows[&window].hovered_url.as_ref().unwrap().url,
        "https://alternate.example/"
    );
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[?1049l"));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the exit detects again");
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://primary.example/");
}

/// With history full and the view scrolled back, a line of output evicts the oldest history row:
/// the view keeps its text, but every absolute row index shifts, so the cache must not compare
/// stamps at the old indices. The eviction invalidates it and the next lookup detects again.
#[test]
fn history_eviction_while_scrolled_back_invalidates_the_cache() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    parser.lock().grid_mut().set_scrollback_limit(10);
    let lines: String = (0..20).map(|line| format!("https://example.com/{line:02}\r\n")).collect();
    assert!(app.__test_advance_child_pane_parser(window, pane, lines.as_bytes()));
    let evicted_before = parser.lock().grid().scrollback_evicted();
    assert!(evicted_before > 0, "the fixture fills history");
    app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().pin_viewport_top(Some(2));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "warm while scrolled back");
    let hovered = app.windows[&window].hovered_url.as_ref().unwrap().url.clone();
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/new\r\n"));
    assert_eq!(parser.lock().grid().scrollback_evicted(), evicted_before + 1, "one row evicted");
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the eviction detects again");
    assert_eq!(
        app.windows[&window].hovered_url.as_ref().unwrap().url,
        hovered,
        "the anchored view still shows, and resolves, the same row"
    );
}

/// The child-window frame path, `refresh_target_hover_from_parsers` with the held parser both
/// redraw adapters pass it, reuses the cache while nothing changes. Moving another tab out of the
/// window completes a topology change, which clears the cache, so the next frame detects again.
#[test]
fn the_child_frame_path_uses_the_cache_and_a_tab_transfer_invalidates_it() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let window = app.__test_seed_child_window(&["moved", "pointed"]);
    let target = app.__test_seed_child_window(&["target"]);
    // A 400 × 80 px viewport of 10 × 20 px cells is a 40 × 4 grid with no renderer.
    let viewport = sonicterm_ui::pane::Rect::new(0.0, 0.0, 400.0, 80.0);
    for id in [window, target] {
        assert!(app.__test_set_child_pane_viewport(id, viewport, 10.0, 20.0));
    }
    let child = &app.windows[&window];
    assert_eq!(child.tabs.active_index(), 1, "the last seeded tab is active");
    let pane = child.tab_states[1].active_pane;
    for pane_state in app.windows[&window].panes.values() {
        pane_state.parser.lock().grid_mut().resize(40, 4);
    }
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    // The pointer rests on row 0, column 3 of the pointed pane.
    app.windows.get_mut(&window).unwrap().cursor_pos = (35.0, 10.0);
    let frame = |app: &mut App| {
        let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
        let guard = parser.lock();
        app.refresh_target_hover_from_parsers(window, [(pane, &*guard)]);
    };
    frame(&mut app);
    frame(&mut app);
    assert_eq!(hover_counts(&app), (2, 1, 1), "the frame path reuses the cache");
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://example.com/");
    app.transfer_tab(Some(window), 0, Some(target), 1).expect("the inactive tab moves");
    assert_eq!(app.windows[&window].tab_states[0].active_pane, pane, "the pointed tab stays");
    assert!(app.windows[&window].hover_detection.is_empty(), "the transfer cleared the cache");
    frame(&mut app);
    frame(&mut app);
    assert_eq!(hover_counts(&app), (4, 2, 2), "detects once after the transfer, then reuses");
}

/// Authorization is revoked through the cache: a probed, authorized path whose row is rewritten
/// to a different path is detected again on the next frame, the old key is no longer authorized,
/// and exactly one probe is submitted, for the new path.
#[cfg(unix)]
#[test]
fn rewriting_an_authorized_path_revokes_it_through_the_cache() {
    let root = native_test_root().join(format!(
        "sonicterm-hover-revoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let (first_path, second_path) = (root.join("first.txt"), root.join("other.txt"));
    for path in [&first_path, &second_path] {
        std::fs::write(path, b"notes").unwrap();
    }
    let (first_text, second_text) =
        (first_path.to_str().unwrap().to_owned(), second_path.to_str().unwrap().to_owned());
    let (mut app, window, pane, mailbox) = counted_hover_app(200, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, first_text.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    let request = mailbox.take_next().expect("the first frame submits a probe");
    let outcome = probe_candidates(&request.key.candidates, classify_local_target);
    let first_key = request.key.clone();
    app.handle_path_probe_finished(PathProbeResult {
        failure: outcome.as_ref().err().copied(),
        selection: outcome.ok(),
        request,
    });
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "the completion's frame reuses the detection");
    assert!(app.windows[&window].path_probe.authorized(&first_key, true));
    let submitted_before = app.frame_counters.as_ref().unwrap().hover.probes_submitted;

    let rewrite = format!("\x1b[1;1H{second_text}\x1b[K");
    assert!(app.__test_advance_child_pane_parser(window, pane, rewrite.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the rewritten row detects again");
    assert!(
        !app.windows[&window].path_probe.authorized(&first_key, true),
        "the old path's key is no longer authorized"
    );
    let second_request = mailbox.take_next().expect("the new path submits a probe");
    assert_ne!(second_request.key, first_key);
    assert!(
        second_request.key.candidates.iter().any(|candidate| candidate.display() == second_text),
        "the probe is for the new path"
    );
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(drained_probes(&mailbox), 0, "exactly one new probe");
    assert_eq!(app.frame_counters.as_ref().unwrap().hover.probes_submitted, submitted_before + 1);
    std::fs::remove_dir_all(root).unwrap();
}

/// The cache holds one entry per window: another cell (a font or DPI change moves the pointer to
/// one), another pane, another view top or another window each detect, and each window keeps its
/// own entry, so returning to a window's cell hits.
#[test]
fn the_cache_is_one_entry_per_window_keyed_by_cell_pane_and_view() {
    let (mut app, first, first_pane, _mailbox) = counted_hover_app(40, 4);
    let second = app.__test_seed_child_window(&["second", "tab"]);
    let panes = app.__test_child_pane_ids(second).unwrap();
    let output: &[u8] = b"one\r\ntwo\r\nhttps://example.com/\r\nfour\r\nfive\r\nsix";
    assert!(app.__test_advance_child_pane_parser(first, first_pane, output));
    for pane in &panes {
        app.windows[&second].panes[pane].parser.lock().grid_mut().resize(40, 4);
        assert!(app.__test_advance_child_pane_parser(second, *pane, output));
    }
    frame_hover(&mut app, first, first_pane, (0, 3));
    frame_hover(&mut app, second, panes[0], (0, 3));
    assert_eq!(hover_counts(&app), (2, 2, 0), "each window detects for itself");
    frame_hover(&mut app, first, first_pane, (0, 3));
    frame_hover(&mut app, second, panes[0], (0, 3));
    assert_eq!(hover_counts(&app), (4, 2, 2), "and keeps its own entry");
    frame_hover(&mut app, first, first_pane, (0, 4));
    assert_eq!(hover_counts(&app).1, 3, "another cell detects");
    if let Some(other_pane) = panes.get(1) {
        frame_hover(&mut app, second, *other_pane, (0, 3));
        assert_eq!(hover_counts(&app).1, 4, "another pane detects");
    }
    let before = hover_counts(&app).1;
    frame_hover_at(&mut app, first, first_pane, (0, 3), Some(0));
    assert_eq!(hover_counts(&app).1, before + 1, "another view top detects");
}

/// An OSC 8 link rewritten with the same label and a different destination is detected again and
/// resolves to the new destination: the rewrite restamps the row, so no hyperlink id has to join
/// the key.
#[test]
fn an_osc8_link_with_the_same_label_and_a_new_destination_detects_again() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let link = |uri: &str| format!("\x1b[1;1H\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        link("https://a.example/").as_bytes()
    ));
    frame_hover(&mut app, window, pane, (0, 2));
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://a.example/");
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        link("https://b.example/").as_bytes()
    ));
    frame_hover(&mut app, window, pane, (0, 2));
    assert_eq!(hover_counts(&app), (2, 2, 0));
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://b.example/");
}

/// Span mapping by binary search returns what the linear scans it replaced return, on a recorded-
/// wrap line of exactly 16 KiB (the cap) whose spaced tokens give many candidates per cell: each
/// mapping helper asserts its result against the linear scan under tests, at every pointed cell.
#[test]
fn span_mapping_matches_the_linear_scan_on_a_full_sixteen_kib_line() {
    let (cols, rows) = (512_u16, 32_u16);
    let mut grid = Grid::new(cols, rows);
    let unit = "./ab cd ef gh ij kl mn op ";
    let text = unit.repeat(usize::from(cols) * usize::from(rows) / unit.len() + 1);
    for character in text.chars().take(usize::from(cols) * usize::from(rows)) {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    let mut most_candidates = 0;
    for col in (0..cols).step_by(7) {
        let pointed = AbsoluteCell { row: 16, col };
        if let Some(scan) = logical_path_scan_at_cell(&grid, 0, pointed, PathStyle::Posix, true) {
            assert!(scan
                .candidates
                .iter()
                .all(|candidate| candidate.spans.iter().any(|span| span.contains(pointed))));
            most_candidates = most_candidates.max(scan.candidates.len());
        }
    }
    assert!(most_candidates >= 8, "the line offers many candidates per cell: {most_candidates}");
}

// The hard-wrap pre-pass as it was before each row was read once: every wrapper collected its own
// row and searched a growing suffix. It is kept only as the equivalence oracle and the cost
// baseline for the shared-view pre-pass.
fn reference_hardwrap_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let first =
        pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    for row_number in (first..=pointed.row).rev() {
        let Some(row) = frame.row(row_number) else {
            // When: row_at_abs has no row_number, that evicted row cannot open a visible candidate.
            continue;
        };
        // The frame's last row may wrap into a row below it that is not displayed, so on a
        // scrolled-back view that boundary is unknown and the logical scanner, which fails closed
        // there, owns it.
        let next_row_number = row_number + 1;
        let wraps_below = if frame.contains(next_row_number) {
            frame.row(next_row_number).is_some_and(|next| next.soft_wrapped_from_previous())
        } else {
            // When: the next row is outside the frame, only a scrolled-back view has one at all.
            frame.rows_below()
        };
        if row.soft_wrapped_from_previous() || wraps_below {
            // When: soft_wrapped_from_previous holds, or may hold, at either boundary, the logical scanner owns it.
            continue;
        }
        let mut result = HardwrapUri::NotApplicable;
        hardwrap_work::add(row.len());
        for (index, cell) in row.iter().enumerate() {
            let closer = match cell.ch {
                '(' => ')',
                '[' => ']',
                _ => {
                    // When: cell.ch opens no supported wrapper, this column cannot delimit a URI.
                    continue;
                }
            };
            let start = index + 1;
            if !reference_opens_http_scheme(row, start) {
                // When: opens_http_scheme finds neither https:// nor http:// at start, this wrapper opens ordinary prose.
                continue;
            }
            let found = reference_hardwrap_uri_candidate(frame, pointed, row_number, start, closer);
            // When: matches! excludes NotApplicable for found, retain this wrapper's claim on pointed.
            if !matches!(found, HardwrapUri::NotApplicable) {
                if !matches!(result, HardwrapUri::NotApplicable) {
                    // When: matches! excludes NotApplicable for result, another wrapper already owns pointed.
                    return HardwrapUri::Incomplete;
                }
                result = found;
            }
        }
        if !matches!(result, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for result, the nearest owning wrapper decides pointed.
            return result;
        }
    }
    HardwrapUri::NotApplicable
}

/// Whether `row`'s cells from column `start` spell `https://` or `http://`, read into a fixed
/// buffer so the pre-pass allocates nothing for the many wrappers that open ordinary prose.
fn reference_opens_http_scheme(row: &Row, start: usize) -> bool {
    let mut scheme = ['\0'; 8];
    for (offset, slot) in scheme.iter_mut().enumerate() {
        let Some(cell) = row.get(start + offset) else {
            // When: the row ends before eight cells, the remaining slots stay empty.
            break;
        };
        *slot = cell.ch;
    }
    let spells =
        |prefix: &str| prefix.chars().zip(scheme).all(|(expected, found)| expected == found);
    spells("https://") || spells("http://")
}

fn reference_hardwrap_uri_candidate(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    first_row: u64,
    first_col: usize,
    closer: char,
) -> HardwrapUri {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    let mut joined = String::new();
    let mut indent = None;
    let mut authority_end = None;
    let refusal = |spans: &[AbsoluteCellSpan]| {
        // When: a span contains pointed and spans_join_across_pane_border is false, suppress that fragment's truncated prefix.
        if spans.iter().any(|span| span.contains(pointed))
            && !spans_join_across_pane_border(frame, spans)
        {
            HardwrapUri::Incomplete
        } else {
            HardwrapUri::NotApplicable
        }
    };
    for offset in 0..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        // When: row_number reaches view_end, the chain leaves the viewport and cannot be proven complete.
        if row_number >= frame.view_end() {
            return refusal(&spans);
        }
        let Some(row) = frame.row(row_number) else {
            // When: row_at_abs has no row_number, the chain cannot be continued or proven.
            return refusal(&spans);
        };
        if row.soft_wrapped_from_previous() {
            // When: soft_wrapped_from_previous holds, the wrap-aware scanner owns this boundary.
            return refusal(&spans);
        }
        let cells = row.iter().collect::<Vec<_>>();
        hardwrap_work::add(cells.len());
        let start = if offset == 0 {
            first_col
        } else {
            // When: offset is past zero, start skips the continuation indent rather than first_col.
            cells.iter().position(|cell| cell.ch != ' ').unwrap_or(cells.len())
        };
        let close = cells
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, cell)| cell.ch == closer)
            .map(|(col, _)| col);
        if offset == 0 && close.is_some() {
            // When: close exists at offset zero, single-row detection stays authoritative.
            return HardwrapUri::NotApplicable;
        }
        let end = close.unwrap_or(usize::from(frame.grid().cols));
        let Some(body) = cells.get(start..end).filter(|body| !body.is_empty()) else {
            // When: cells hold no non-empty body between start and end, this row adds no fragment.
            return refusal(&spans);
        };
        if body.iter().any(|cell| {
            unsafe_path_cell(cell)
                || cell.hyperlink().is_some()
                || !cell.ch.is_ascii()
                || cell.ch.is_whitespace()
                || cell.ch.is_control()
                || matches!(cell.ch, '(' | ')' | '[' | ']')
        }) {
            // When: any body cell is unsafe, hyperlinked, non-ascii, blank, control, or a wrapper,
            // the row carries prose rather than a continuation of this candidate.
            return refusal(&spans);
        }
        if offset > 0 {
            // When: offset is past zero, a continuation row must open no scheme and respect the indent.
            let prefix = body.iter().take(8).map(|cell| cell.ch).collect::<String>();
            if prefix.starts_with("https://") || prefix.starts_with("http://") {
                // When: prefix opens its own scheme, that independent URI keeps its destination.
                return refusal(&spans);
            }
            if start > MAX_HARDWRAP_INDENT {
                // When: start passes MAX_HARDWRAP_INDENT, the row is too deep to be one continued line.
                return refusal(&spans);
            }
        }
        if offset > 0 && *indent.get_or_insert(start) != start {
            // When: start differs from the recorded indent, the preceding fragment does not own this row.
            return refusal(&spans);
        }
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: start as u16,
            end_col: end as u16,
        });
        if joined.len() + body.len() > MAX_LOGICAL_PATH_BYTES {
            // When: joined plus body passes MAX_LOGICAL_PATH_BYTES, stop before admitting more text.
            return refusal(&spans);
        }
        joined.extend(body.iter().map(|cell| cell.ch));
        if offset == 0 {
            // When: offset is zero, joined must prove a complete authority before the margin cut.
            let scheme_len = if joined.starts_with("https://") { 8 } else { 7 };
            authority_end = joined[scheme_len..]
                .find(['/', '?', '#'])
                .filter(|index| *index > 0 && joined.as_bytes()[scheme_len + index] == b'/')
                .map(|index| scheme_len + index);
            if authority_end.is_none() {
                // When: authority_end is absent, no path slash followed the host, so invent no suffix.
                return refusal(&spans);
            }
        }
        if joined.matches("://").count() != 1 {
            // When: joined holds other than one :// separator, two destinations were concatenated.
            return refusal(&spans);
        }
        if close.is_some() {
            // When: close exists, the wrapper terminates and joined must validate as one whole URI.
            if unsafe_path_cell(&cells[end])
                || cells[end].hyperlink().is_some()
                || cells[end + 1..].iter().take_while(|cell| !cell.ch.is_whitespace()).any(|cell| {
                    unsafe_path_cell(cell) || !matches!(cell.ch, '.' | ',' | ';' | ':' | '!' | '?')
                })
            {
                // When: cells after end continue the token, the apparent closer may be internal URI text.
                return refusal(&spans);
            }
            let matches = sonicterm_cfg::url_scan::find_urls(&joined);
            let Some(found) = matches.first().filter(|found| {
                matches.len() == 1
                    && found.start == 0
                    && found.end == joined.len()
                    && found.url == joined
                    && authority_end.is_some()
            }) else {
                // When: find_urls does not return exactly joined as one whole match, reject the prefix.
                return refusal(&spans);
            };
            if !spans.iter().any(|span| span.contains(pointed)) {
                // When: no span contains pointed, this proven chain does not own the pointer.
                return HardwrapUri::NotApplicable;
            }
            if spans_join_across_pane_border(frame, &spans) {
                // When: spans_join_across_pane_border holds, the chain runs into the next pane's text.
                return HardwrapUri::NotApplicable;
            }
            return HardwrapUri::Complete(LogicalTargetCandidate {
                target: DetectedTarget::Uri(found.url.clone()),
                spans,
            });
        }
    }
    refusal(&spans)
}

/// A deterministic xorshift generator, so the equivalence corpus is the same on every run.
struct CorpusRng(u64);

impl CorpusRng {
    fn next_value(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_value() % bound as u64) as usize
    }
}

/// Fragments a random prose row is built from: wrappers, schemes, blanks, wide and combining-free
/// non-ASCII text, a box-drawing border and an OSC 8 link.
const CORPUS_FRAGMENTS: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "(https://",
    "[http://",
    "ex.co/",
    "/",
    "a",
    "bb",
    " ",
    "  ",
    ".",
    "?",
    "é",
    "\u{754c}",
    "\u{2502}",
    "\x1b]8;;https://h.test/\x1b\\ln\x1b]8;;\x1b\\",
];

/// One random prose row of at most `cols` cells' worth of fragments.
fn corpus_prose_line(rng: &mut CorpusRng, cols: u16) -> String {
    let fragment_count = rng.below(usize::from(cols) / 2);
    (0..fragment_count).map(|_| CORPUS_FRAGMENTS[rng.below(CORPUS_FRAGMENTS.len())]).collect()
}

/// A hard-wrapped bracketed URL: a first row filled to the edge, one or two indented continuation
/// rows and a closer, sometimes with an unequal indent or one character mutated into a break.
fn corpus_chain_lines(rng: &mut CorpusRng, cols: u16) -> Vec<String> {
    let width = usize::from(cols);
    let (opener, closer) = if rng.below(2) == 0 { ('(', ')') } else { ('[', ']') };
    let head = ["", "x ", "see "][rng.below(3)];
    let scheme = ["https://ex.co/", "http://ex.co/", "https://ex.co"][rng.below(3)];
    let mut first = format!("{head}{opener}{scheme}");
    while first.len() < width {
        first.push('a');
    }
    let mut lines = vec![first];
    let continuation_count = 1 + rng.below(2);
    let indent = rng.below(3);
    for index in 0..continuation_count {
        let row_indent = if rng.below(8) == 0 { indent + 1 } else { indent };
        let mut line = " ".repeat(row_indent);
        if index + 1 < continuation_count {
            while line.len() < width {
                line.push('b');
            }
        } else {
            let body_len = 1 + rng.below(width - row_indent - 2);
            line.push_str(&"c".repeat(body_len));
            line.push(closer);
            line.push_str(["", " done", ".", "x", ")"][rng.below(5)]);
            line.truncate(width);
        }
        lines.push(line);
    }
    if rng.below(4) == 0 {
        // Mutate one character of one row into a break or a wrapper.
        let line_index = rng.below(lines.len());
        let mut chars = lines[line_index].chars().collect::<Vec<_>>();
        let position = rng.below(chars.len());
        chars[position] = ['(', ' ', ')', 'é', '/', ']'][rng.below(6)];
        lines[line_index] = chars.into_iter().collect();
    }
    lines
}

/// The shared-view pre-pass decides every cell exactly as the per-wrapper pre-pass did: the same
/// refusal (`Incomplete`), the same ownership and the same spans, over primary and alternate
/// screens, live and scrolled-back frames, split borders, hyperlinks and wide cells. The corpus
/// must reach both `Complete` and `Incomplete`, or the agreement would prove little.
#[test]
fn hardwrap_pre_pass_matches_the_per_wrapper_reference() {
    let mut rng = CorpusRng(0x5eed_1649_cafe_f00d);
    let (mut complete_count, mut incomplete_count) = (0usize, 0usize);
    for grid_index in 0..160 {
        let cols = [20u16, 24, 30][rng.below(3)];
        let rows = 8u16;
        let mut lines = Vec::new();
        while lines.len() < 12 {
            // When: the coin picks a chain, emit a hard-wrapped URL; otherwise one prose row.
            if rng.below(2) == 0 {
                lines.extend(corpus_chain_lines(&mut rng, cols));
            } else {
                lines.push(corpus_prose_line(&mut rng, cols));
            }
        }
        let alt_screen = grid_index % 3 == 0;
        let output = if alt_screen {
            let mut placed = String::from("\x1b[?1049h");
            for (row, line) in lines.iter().take(usize::from(rows)).enumerate() {
                placed.push_str(&format!("\x1b[{};1H{line}", row + 1));
            }
            if grid_index % 2 == 0 {
                placed.push_str(&pane_border(cols / 2, rows));
            }
            placed
        } else {
            lines.join("\r\n")
        };
        let mut parser = sonicterm_vt::vt::Parser::new(Grid::new(cols, rows));
        parser.advance(output.as_bytes());
        let grid = parser.grid();
        let live_top = grid.scrollback_evicted() + grid.scrollback_len() as u64;
        let scrolled_top = live_top.saturating_sub(1 + rng.below(3) as u64);
        for view_top in [live_top, scrolled_top] {
            for row in view_top..view_top + u64::from(rows) {
                for col in 0..cols {
                    let pointed = AbsoluteCell { row, col };
                    let found = hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed);
                    let expected =
                        reference_hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed);
                    assert_eq!(
                        found, expected,
                        "grid {grid_index}, view_top {view_top}, cell {row}:{col}, output {output:?}"
                    );
                    match expected {
                        HardwrapUri::Complete(_) => complete_count += 1,
                        HardwrapUri::Incomplete => incomplete_count += 1,
                        HardwrapUri::NotApplicable => {}
                    }
                }
            }
        }
    }
    assert!(complete_count > 50, "the corpus reached Complete only {complete_count} times");
    assert!(incomplete_count > 50, "the corpus reached Incomplete only {incomplete_count} times");
}

/// Cells walked by `pre_pass` for one primary-screen row holding `pattern` repeated to `cols`
/// columns, pointed at its first cell.
fn hardwrap_cells_walked(
    pattern: &str,
    cols: u16,
    pre_pass: fn(&FrameRows<'_>, AbsoluteCell) -> HardwrapUri,
) -> u64 {
    let row_text = pattern.chars().cycle().take(usize::from(cols)).collect::<String>();
    let mut parser = sonicterm_vt::vt::Parser::new(Grid::new(cols, 2));
    parser.advance(row_text.as_bytes());
    let grid = parser.grid();
    let pointed = AbsoluteCell { row: 0, col: 0 };
    let (_, walked) = hardwrap_work::measure(|| pre_pass(&FrameRows::new(grid, 0), pointed));
    walked
}

/// Assert that doubling the row width at most about doubles the pre-pass's work on `pattern`,
/// while the per-wrapper reference grows about fourfold, which proves the measure can tell them apart.
fn assert_hardwrap_work_is_linear(pattern: &str) {
    let narrow = hardwrap_cells_walked(pattern, 400, hardwrap_uri_in);
    let wide = hardwrap_cells_walked(pattern, 800, hardwrap_uri_in);
    assert!(narrow > 0, "{pattern:?}: the measure counted no work");
    assert!(wide * 2 <= narrow * 5, "{pattern:?}: {narrow} cells at 400 columns, {wide} at 800");
    let reference_narrow = hardwrap_cells_walked(pattern, 400, reference_hardwrap_uri_in);
    let reference_wide = hardwrap_cells_walked(pattern, 800, reference_hardwrap_uri_in);
    assert!(
        reference_wide >= reference_narrow * 3,
        "{pattern:?}: the reference walked {reference_narrow} then {reference_wide} cells"
    );
}

/// A row of repeated complete `(https://a)` wrappers costs the pre-pass work linear in its width:
/// each wrapper's same-row closer is not found by re-collecting the row per wrapper.
#[test]
fn repeated_complete_wrappers_cost_linear_work() {
    assert_hardwrap_work_is_linear("(https://a)");
}

/// A row of repeated unmatched `(https://` openers costs work linear in its width: no opener
/// searches the growing suffix after it for a closer that never comes.
#[test]
fn repeated_unmatched_openers_cost_linear_work() {
    assert_hardwrap_work_is_linear("(https://a/b ");
}
