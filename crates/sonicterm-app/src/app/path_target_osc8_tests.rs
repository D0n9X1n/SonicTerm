//! Preserved osc8 path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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
    // The native grammar decides what is a path, so each platform gets an absolute path it reads.
    let (wrapped_path, notes_path) = if cfg!(windows) {
        ("C:/tmp/abcdefghijklmnopqrstuvwxyz.txt", "C:/tmp/notes.txt")
    } else {
        ("/tmp/abcdefghijklmnopqrstuvwxyz.txt", "/tmp/notes.txt")
    };
    for (label, line, pointed_col) in [
        // The row below is a recorded continuation of this path.
        ("wrapped", wrapped_path, 3_u16),
        // The row below is unrelated text.
        ("complete", notes_path, 3),
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
    let (app, window, pane) = pane_after(20, 3, &format!("one\r\ntwo\r\n{notes_path}"));
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let target =
        app.cell_target_from_parser(window, pane, 2, 3, &guard, None).expect("live bottom row");
    assert!(matches!(target.target, ResolvedCellTarget::Path(_)));
}
