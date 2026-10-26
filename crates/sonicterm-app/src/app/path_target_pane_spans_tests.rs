//! Preserved pane spans path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

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

/// Programs write primary-screen lines in order. A URL that ends before the edge resolves alone;
/// one that starts its row and ends exactly at the edge, followed by a single token at its column,
/// is joined, the documented ambiguity of the hard-wrap rule.
#[test]
fn primary_screen_urls_at_the_edge_resolve() {
    let short = target_after(21, 4, "https://example.com/\r\nnext", 0, 5).unwrap();
    assert_eq!(short.target, ResolvedCellTarget::Uri("https://example.com/".to_string()));
    let edge = target_after(20, 4, "https://example.com/\r\nnext", 0, 5).unwrap();
    assert_eq!(edge.target, ResolvedCellTarget::Uri("https://example.com/next".to_string()));
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
