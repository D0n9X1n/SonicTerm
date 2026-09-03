use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme, url_scan::DetectedTarget};
use sonicterm_grid::grid::{Cell, CellFlags, Color, Row};
use sonicterm_types::HyperlinkId;

fn ascii_row(text: &str) -> Row {
    Row::from_flat(
        text.chars()
            .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
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

/// Trusted pane context selects a trimmed source span only while the literal punctuation file is missing.
#[cfg(target_os = "macos")]
#[test]
fn app_probe_prefers_literal_then_trimmed_revealable_path() {
    let root = std::env::temp_dir().join(format!(
        "sonicterm-prose-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let source_dir = root.join("lua/config");
    std::fs::create_dir_all(&source_dir).unwrap();
    let trimmed_path = source_dir.join("lsp.lua");
    let literal_path = source_dir.join("lsp.lua,");
    std::fs::write(&trimmed_path, b"return {}").unwrap();

    let text = "lua/config/lsp.lua,";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["prose"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let output = format!("\x1b]7;file://{}\x1b\\{text}", root.display());
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let snapshot = app.cell_target_at(window, pane, 0, 2).expect("relative path candidate set");
    let ResolvedCellTarget::Path(key) = snapshot.target else {
        panic!("relative path must require a filesystem probe")
    };

    let trimmed = select_openable_candidate(&key.candidates, classify_local_target)
        .expect("missing literal permits the existing source file");
    assert_eq!(trimmed.candidate.display(), "lua/config/lsp.lua");
    assert_eq!(trimmed.candidate.end_col, u16::try_from(text.len() - 1).unwrap());
    assert_eq!(trimmed.decision, PathOpenDecision::Revealable(PathKind::File));

    std::fs::write(&literal_path, b"punctuation filename").unwrap();
    let literal = select_openable_candidate(&key.candidates, classify_local_target)
        .expect("existing punctuation filename has literal priority");
    assert_eq!(literal.candidate.display(), text);
    assert_eq!(literal.candidate.end_col, u16::try_from(text.len()).unwrap());
    assert_eq!(literal.decision, PathOpenDecision::Openable(PathKind::File));

    std::fs::remove_dir_all(root).unwrap();
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
                        && usize::from(probe.start_col) == name_start_col
                        && usize::from(probe.end_col) == name_start_col + "ff ff".chars().count()
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
                        && probe.start_col == 0
                        && usize::from(probe.end_col) == candidate.chars().count()
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
                            DetectedTarget::Uri(_) => false,
                        }
                }),
                "missing full row span at {col} in {text:?}: {candidates:?}"
            );
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
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells.push(Cell::plain('界', Color::Default, Color::Default, CellFlags::WIDE));
    cells.push(Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT));
    cells.extend(
        "/file"
            .chars()
            .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())),
    );
    let row = Row::from_flat(cells);

    assert!(target_at_row_cell(&row, 1, PathStyle::Posix).is_none());
    assert!(target_at_row_cell(&row, 8, PathStyle::Posix).is_none());
}

/// Combining extras reject a whole token instead of exposing an ASCII fragment.
#[test]
fn combining_extras_reject_the_entire_surrounding_path_token() {
    let mut cells = "./cafe/file"
        .chars()
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
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
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
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
fn macos_policy_separates_revealable_sources_from_launchers() {
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
        assert!(
            macos_file_policy(Path::new(path), b"ordinary", false).is_blocked(),
            "unexpected actionable launcher {path}"
        );
    }
    for path in [
        "/tmp/run.sh",
        "/tmp/run.py",
        "/tmp/run.rb",
        "/tmp/run.pl",
        "/tmp/run.zsh",
        "/tmp/run.lua",
        "/tmp/run.js",
    ] {
        assert_eq!(
            macos_file_policy(Path::new(path), b"ordinary", false),
            PathOpenDecision::Revealable(PathKind::File),
            "source suffix must be reveal-only: {path}"
        );
        assert!(macos_file_policy(Path::new(path), b"#!/bin/sh", false).is_blocked());
        assert!(macos_file_policy(Path::new(path), b"ordinary", true).is_blocked());
    }
    for prefix in [
        b"#!/bin/sh".as_slice(),
        b"\x7fELF\x02\x01\x01\0".as_slice(),
        b"MZ\x90\0\0\0\0\0".as_slice(),
        b"\xcf\xfa\xed\xfe\0\0\0\0".as_slice(),
        b"\xfe\xed\xfa\xcf\0\0\0\0".as_slice(),
        b"\xca\xfe\xba\xbe\0\0\0\0".as_slice(),
        b"\xbe\xba\xfe\xca\0\0\0\0".as_slice(),
    ] {
        assert!(
            macos_file_policy(Path::new("/tmp/tool"), prefix, false).is_blocked(),
            "unexpected actionable executable prefix {prefix:?}"
        );
    }
    assert!(macos_file_policy(Path::new("/tmp/tool"), b"ordinary", true).is_blocked());
    assert_eq!(
        macos_file_policy(Path::new("/tmp/readme.txt"), b"notes", false),
        PathOpenDecision::Openable(PathKind::File)
    );
}

/// Activation-time macOS revalidation requires the exact reveal action and kind to remain stable.
#[cfg(target_os = "macos")]
#[test]
fn macos_reveal_revalidation_rejects_new_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "sonicterm-reveal-revalidate-{}-{}-source.lua",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"ordinary").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let decision = PathOpenDecision::Revealable(PathKind::File);
    let spec =
        macos_validated_open_spec(&path, decision).expect("stable source remains revealable");
    assert_eq!(spec.args[0], "-R");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(macos_validated_open_spec(&path, decision).is_err());
    std::fs::remove_file(path).unwrap();
}

/// macOS production classification blocks an extensionless executable before LaunchServices.
#[cfg(target_os = "macos")]
#[test]
fn macos_classification_rejects_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
        "sonicterm-macos-executable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"ordinary").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(classify_macos_target(&path), PathOpenDecision::Blocked);
    std::fs::remove_file(path).unwrap();
}

/// Windows launcher, PATHEXT, ADS, and trailing-dot names are blocked before ShellExecuteExW.
#[test]
fn windows_open_policy_blocks_launcher_classes() {
    for path in [
        r"C:\tmp\run.exe",
        r"C:\tmp\run.ps1",
        r"C:\tmp\link.lnk",
        r"C:\tmp\file.txt:payload",
        r"C:\tmp\name. ",
    ] {
        assert!(
            windows_path_policy(Path::new(path), Some(".EXE;.COM;.BAT;.CMD")).is_blocked(),
            "unexpected openable {path}"
        );
    }
    assert_eq!(
        windows_path_policy(Path::new(r"C:\tmp\readme.txt"), Some(".EXE;.COM;.BAT;.CMD")),
        PathOpenDecision::Openable(PathKind::File)
    );
}

/// Linux blocks desktop launchers and executable content without relying on unreliable mode bits.
#[test]
fn linux_open_policy_sniffs_unsafe_content() {
    assert!(linux_file_policy(Path::new("/tmp/tool.desktop"), b"[Desktop Entry]").is_blocked());
    assert!(linux_file_policy(Path::new("/tmp/tool.AppImage"), b"ordinary").is_blocked());
    assert!(linux_file_policy(Path::new("/tmp/tool"), b"\x7fELF").is_blocked());
    assert!(linux_file_policy(Path::new("/tmp/tool"), b"#!/bin/sh").is_blocked());
    assert!(linux_file_policy(Path::new("/tmp/tool"), b"MZ\0\0").is_blocked());
    assert_eq!(
        linux_file_policy(Path::new("/tmp/readme.txt"), b"notes"),
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

/// Real filesystem classification accepts files/directories and rejects symlinks and sockets.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn filesystem_classification_rejects_identity_indirection_and_special_entries() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let root = std::env::temp_dir().join(format!(
        "sonicterm-path-kinds-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("notes.txt");
    let directory = root.join("folder");
    let link = root.join("link");
    let socket = root.join("socket");
    std::fs::write(&file, b"notes").unwrap();
    std::fs::create_dir(&directory).unwrap();
    symlink(&file, &link).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();

    assert_eq!(classify_local_target(&file), PathOpenDecision::Openable(PathKind::File));
    assert_eq!(classify_local_target(&directory), PathOpenDecision::Openable(PathKind::Directory));
    assert_eq!(classify_local_target(&link), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&socket), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&root.join("missing")), PathOpenDecision::Missing);

    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// The Linux open adapter receives an already-opened file identity, not its pathname.
#[test]
fn open_target_adapter_owns_an_open_file() {
    use std::io::Write;

    let path = std::env::temp_dir().join(format!(
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

fn probe_candidate(display: &str, path: &str, start_col: u16) -> PathProbeCandidate {
    PathProbeCandidate {
        start_col,
        end_col: start_col + u16::try_from(display.chars().count()).unwrap(),
        target: DetectedTarget::PathCandidate(display.into()),
        resolved_path: PathBuf::from(path),
    }
}

fn probe_key(path: &str, view_top: u64) -> PathProbeKey {
    PathProbeKey {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        viewport_row: 2,
        absolute_row: view_top + 2,
        view_top,
        pointed_col: 4,
        candidates: vec![probe_candidate("./file", path, 4)],
        cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
        cwd_revision: 3,
        row_fingerprint: 11,
        scrollback_evicted: 0,
        alt_screen: false,
    }
}

fn openable_result(request: PathProbeRequest) -> PathProbeResult {
    PathProbeResult {
        selection: Some(PathProbeSelection {
            candidate: request.key.candidates[0].clone(),
            decision: PathOpenDecision::Openable(PathKind::File),
        }),
        request,
    }
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

/// Accepted spans retain authorization when a destination adds only a shorter candidate.
#[test]
fn probe_state_retains_selection_across_irrelevant_candidate_changes() {
    let mut state = PathProbeState::default();
    let mut key = probe_key("/work/My Folder", 20);
    let selected = probe_candidate("My Folder", "/work/My Folder", 4);
    key.candidates = vec![selected.clone()];
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let mut moved = key.clone();
    moved.pointed_col = 8;
    moved.candidates.push(probe_candidate("Folder", "/work/Folder", 7));
    assert!(state.request(moved.clone()).is_none());
    assert!(state.authorized(&moved, true));
}

/// A newly visible equal or longer candidate revokes authorization and schedules a fresh probe.
#[test]
fn probe_state_reprobes_for_new_competing_candidates() {
    for competing in [
        probe_candidate("Name Here", "/work/Name Here", 5),
        probe_candidate("Name Here Extra", "/work/Name Here Extra", 5),
    ] {
        let mut state = PathProbeState::default();
        let selected = probe_candidate("Left Name", "/work/Left Name", 0);
        let mut key = probe_key("/work/Left Name", 20);
        key.pointed_col = 1;
        key.candidates = vec![selected.clone()];
        let result = openable_result(state.request(key.clone()).unwrap());
        assert!(state.accept(&result, Some(&key)));

        let mut moved = key.clone();
        moved.pointed_col = 6;
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
    key.pointed_col = 1;
    key.candidates = vec![selected.clone()];
    let request = state.request(key.clone()).unwrap();

    let mut moved = key.clone();
    moved.pointed_col = 6;
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
    changed.row_fingerprint = changed.row_fingerprint.wrapping_add(1);
    assert!(state.request(changed.clone()).is_some());
    assert!(!state.authorized(&changed, true));
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

    assert!(!state.accept(&PathProbeResult { request: old, selection: None }, Some(&key)));
    assert_eq!(state.decision_for(&key), None);
    assert!(state.accept(&openable_result(current), Some(&key)));
}

/// Longest openable selection wins without falling back past a blocked existing tier.
#[test]
fn candidate_selection_prefers_longest_and_fails_closed() {
    let candidates = vec![
        probe_candidate("OneDrive - Microsoft", "/work/OneDrive - Microsoft", 0),
        probe_candidate("OneDrive", "/work/OneDrive", 0),
    ];
    let selected = select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive - Microsoft") {
            PathOpenDecision::Openable(PathKind::Directory)
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .expect("longest candidate");
    assert_eq!(selected.candidate.display(), "OneDrive - Microsoft");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive - Microsoft") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::Directory)
        }
    })
    .is_none());
}

/// A punctuation-ending literal wins, and only Missing permits its shorter prose alternate.
#[test]
fn candidate_selection_falls_through_only_from_missing_literal() {
    let candidates = vec![
        probe_candidate("src/main.rs,", "/work/src/main.rs,", 0),
        probe_candidate("src/main.rs", "/work/src/main.rs", 0),
    ];
    let literal =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::File))
            .expect("literal punctuation file wins");
    assert_eq!(literal.candidate.display(), "src/main.rs,");

    let trimmed = select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs,") {
            PathOpenDecision::Missing
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .expect("missing literal permits shorter prose path");
    assert_eq!(trimmed.candidate.display(), "src/main.rs");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs,") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .is_none());
}

/// Openable and revealable results in one tier remain ambiguous and fail closed.
#[test]
fn actionable_candidate_kinds_share_one_ambiguity_count() {
    let candidates = vec![
        probe_candidate("left.lua", "/work/left.lua", 0),
        probe_candidate("right.rs", "/work/right.rs", 0),
    ];

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("left.lua") {
            PathOpenDecision::Revealable(PathKind::File)
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .is_none());
}

/// Revealable probe results authorize the same epoch-keyed activation path as openable results.
#[test]
fn revealable_selection_is_authorized_without_weakening_freshness() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/source.lua", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = PathProbeResult {
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

/// Real filesystem classification selects the complete spaced entry over an existing short prefix.
#[test]
fn filesystem_selection_prefers_complete_spaced_entry() {
    let root = std::env::temp_dir().join(format!(
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
            start_col: 0,
            end_col: 20,
            target: DetectedTarget::BareName("OneDrive - Microsoft".into()),
            resolved_path: complete.clone(),
        },
        PathProbeCandidate {
            start_col: 0,
            end_col: 8,
            target: DetectedTarget::BareName("OneDrive".into()),
            resolved_path: short,
        },
    ];

    let selected = select_openable_candidate(&candidates, classify_local_target)
        .expect("complete spaced directory is openable");
    assert_eq!(selected.candidate.resolved_path, complete);
    std::fs::remove_dir_all(root).unwrap();
}

/// Equal-length positive candidates remain inert rather than choosing by row order.
#[test]
fn equal_length_openable_candidates_are_ambiguous() {
    let candidates = vec![
        probe_candidate("Left Name", "/work/Left Name", 0),
        probe_candidate("Name Here", "/work/Name Here", 5),
    ];
    assert!(select_openable_candidate(&candidates, |_| {
        PathOpenDecision::Openable(PathKind::Directory)
    })
    .is_none());
}

/// Filesystem probes authorize an openable entry and revoke the same entry once missing.
#[test]
fn filesystem_openability_controls_path_authorization() {
    let path = std::env::temp_dir().join(format!(
        "sonicterm-path-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"present").unwrap();

    let mut state = PathProbeState::default();
    let key = probe_key(path.to_str().unwrap(), 20);
    let request = state.request(key.clone()).unwrap();
    let existing = PathProbeResult {
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&existing, Some(&key)));
    assert!(state.authorized(&key, true));

    std::fs::remove_file(&path).unwrap();
    state.invalidate();
    let request = state.request(key.clone()).unwrap();
    let missing = PathProbeResult {
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&missing, Some(&key)));
    assert!(!state.authorized(&key, true));
}

/// The latest-request mailbox retains only the newest request while one wake is pending.
#[test]
fn probe_mailbox_coalesces_to_the_latest_request() {
    let (mailbox, wakes) = PathProbeMailbox::new();
    let mut state = PathProbeState::default();
    let first = state.request(probe_key("/work/first", 20)).unwrap();
    state.invalidate();
    let latest = state.request(probe_key("/work/latest", 20)).unwrap();

    mailbox.submit(first).unwrap();
    mailbox.submit(latest.clone()).unwrap();
    assert_eq!(wakes.len(), 1, "only one worker wake is queued");
    wakes.recv().unwrap();
    assert_eq!(mailbox.take_latest(), Some(latest));
    assert_eq!(mailbox.take_latest(), None);
}
