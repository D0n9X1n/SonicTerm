//! Preserved logical path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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
