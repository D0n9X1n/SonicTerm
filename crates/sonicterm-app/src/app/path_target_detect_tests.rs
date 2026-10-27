//! Preserved detect path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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
