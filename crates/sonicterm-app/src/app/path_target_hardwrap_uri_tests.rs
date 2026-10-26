//! Preserved hardwrap uri path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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
