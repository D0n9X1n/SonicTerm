//! Preserved hardwrap paths path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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
