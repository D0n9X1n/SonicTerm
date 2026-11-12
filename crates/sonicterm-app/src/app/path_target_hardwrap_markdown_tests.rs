//! Padded hard-wrapped markdown link target cases.

use super::super::path_target_tests::*;
use super::*;

/// The link target the screenshot shape carries, cut after `Usag`.
const DOC_URI: &str =
    "https://github.com/D0n9X1n/bufferlines.nvim/wiki/Installation-and-Usage-zh-CN";

/// Pad each line with blanks to `content_cols` display cells, then add `border` when one is given,
/// so every row ends the same distance from the pane edge as a padded box draws it.
fn padded_rows(lines: &[&str], content_cols: usize, border: Option<&str>) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let width = unicode_width::UnicodeWidthStr::width(*line);
            assert!(width <= content_cols, "line wider than the box: {line}");
            format!("{line}{}{}", " ".repeat(content_cols - width), border.unwrap_or(""))
        })
        .collect()
}

/// Claude Code's numbered markdown row: the target is cut before the pane edge, padding follows
/// it, and the continuation hangs at the `[` column; every fragment resolves the whole target.
#[test]
fn padded_markdown_target_joins_across_hanging_indent() {
    let cut = DOC_URI.find("Usag").unwrap() + "Usag".len();
    let first = format!("    60 [安装]({}", &DOC_URI[..cut]);
    let first_cols = unicode_width::UnicodeWidthStr::width(first.as_str());
    let tail_len = (DOC_URI.len() - cut) as u16;
    // The target opens after `    60 [安装](`: seven gutter cells, `[`, two wide cells, `]`, `(`.
    let start = 14u16;
    for (tail, border, grid_cols) in
        [(")。", None, 100u16), (") 。", None, 100), (")", None, 100), (")。其他", Some("│x"), 100)]
    {
        let second = format!("       {}{tail}", &DOC_URI[cut..]);
        let lines = padded_rows(&[&first, &second], first_cols + 8, border);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, grid_cols, 10, &lines);
        let pointers = (start..first_cols as u16)
            .map(|col| (0, col))
            .chain((7..7 + tail_len).map(|col| (1, col)));
        for (row, col) in pointers {
            let target = app.cell_target_at(window, pane, row, col).expect("padded fragment");
            assert_eq!(target.display, DOC_URI, "tail {tail:?} row {row} col {col}");
            let cells = target.hovered(true).expect("highlight").cells;
            let spans =
                cells.spans().iter().map(|span| (span.start_col, span.end_col)).collect::<Vec<_>>();
            assert_eq!(spans, [(start, first_cols as u16), (7, 7 + tail_len)], "tail {tail:?}");
        }
        // The closer and the cells after it never join the destination footprint.
        assert!(app.cell_target_at(window, pane, 1, 7 + tail_len).is_none(), "tail {tail:?}");
    }
}

/// A target split over three rows joins only when the middle row ends at the first row's column.
#[test]
fn padded_markdown_target_joins_three_rows() {
    let uri = "https://example.com/aaaaaaaaaa/bbbbbbbbbb/cccccccccc/dddddddddd/eeeeeeeeee";
    let first = format!("  1 [doc]({}", &uri[..30]);
    let width = first.len();
    let middle_len = width - 4;
    let middle = format!("    {}", &uri[30..30 + middle_len]);
    let last = format!("    {}).", &uri[30 + middle_len..]);
    let lines = padded_rows(&[&first, &middle, &last], width + 6, None);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &lines);
    for (row, col) in [(0, 12), (1, 4), (2, 5)] {
        let target = app.cell_target_at(window, pane, row, col).expect("three-row fragment");
        assert_eq!(target.display, uri, "row {row}");
        assert_eq!(target.hovered(true).unwrap().cells.spans().len(), 3, "row {row}");
    }
}

/// On a `+` diff row the sign repeated before the continuation is gutter, not URL text: the target
/// joins without it, and a continuation that holds only the closer keeps the first row's URL.
#[test]
fn padded_markdown_target_skips_repeated_diff_sign() {
    let uri = "https://github.com/ryanoasis/nerd-fonts/wiki";
    let first = format!("    59 +see [Nerd Font]({}", &uri[..38]);
    let second = format!("       +{}) and more", &uri[38..]);
    let lines = padded_rows(&[&first, &second], first.len() + 7, None);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &lines);
    for (row, col) in [(0, 30), (1, 9)] {
        let target = app.cell_target_at(window, pane, row, col).expect("diff fragment");
        assert_eq!(target.display, uri, "row {row}");
    }
    // The repeated sign itself is not part of the target.
    assert!(app.cell_target_at(window, pane, 1, 7).is_none());

    // The whole URL fits on the first row and the continuation holds only the closer: the
    // first row's own URL stays clickable, never a URL with the sign appended.
    let whole = "https://github.com/ryanoasis/nerd-fonts";
    let first = format!("    59 +see [Nerd Font]({whole}");
    let lines = padded_rows(&[&first, "       +)。其他插件"], first.len() + 7, None);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &lines);
    assert_eq!(app.cell_target_at(window, pane, 0, 30).expect("row URL").display, whole);
}

/// Shapes the padded rule must not join keep today's single-row result: the first row's own URL.
#[test]
fn padded_markdown_refusals_keep_single_row_detection() {
    let prefix = "https://example.com/aaaa/bbbb";
    let cases: [(&str, String, String, &str); 10] = [
        // A plain prose parenthesis is not a markdown target, so word wrapping may have cut it.
        ("plain paren", format!("    1 see ({prefix}"), "      cccc)".into(), prefix),
        // Without a numbered gutter the row is not the padded box, even when a later row looks aligned.
        ("no gutter", format!("See [x]({prefix}"), "    cccc) next item".into(), prefix),
        // A first fragment ending in sentence punctuation is where a word wrapper breaks.
        ("comma", format!("  1 [a]({prefix},"), "    cccc)".into(), prefix),
        // A new numbered gutter row is a new line, not a hanging continuation.
        ("gutter row", format!("    60 [a]({prefix}"), "    61)".into(), prefix),
        // A continuation not at the content column is not owned by the target.
        ("unaligned", format!("  1 [a]({prefix}"), "     cccc)".into(), prefix),
        // A continuation that holds a space carries prose, not the rest of one target.
        ("space", format!("  1 [a]({prefix}"), "    cc cc)".into(), prefix),
        // A continuation that opens its own scheme is an independent URL.
        ("second scheme", format!("  1 [a]({prefix}"), "    https://b.test/x)".into(), prefix),
        // A sign that differs from the opener row's is a different diff line.
        ("other sign", format!("    59 +[a]({prefix}"), "       -cccc)".into(), prefix),
        // Without a sign on the opener row, a sign before the content column is URL text, not gutter.
        ("unexpected sign", format!("    59 [a]({prefix}"), "      +cccc)".into(), prefix),
        // A host split across rows would let the continuation choose the destination's host.
        (
            "split authority",
            "  1 [a](https://good.test".into(),
            "    @evil.test/cccc)".into(),
            "https://good.test",
        ),
    ];
    for (label, first, second, row_url) in cases {
        let lines = padded_rows(&[&first, &second], first.len() + 6, None);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &lines);
        let col = first.find("https").unwrap() as u16 + 2;
        // The first row keeps exactly the URL it shows on its own.
        let display = app.cell_target_at(window, pane, 0, col).map(|target| target.display);
        assert_eq!(display.as_deref(), Some(row_url), "{label}");
        // The continuation never resolves to a destination built from the first row's prefix.
        assert!(
            app.cell_target_at(window, pane, 1, 8)
                .is_none_or(|target| !target.display.starts_with(row_url)),
            "{label} continuation"
        );
    }
}

/// A combining mark on the `(` the join would remove makes that delimiter unsafe, so nothing joins.
#[test]
fn padded_markdown_refuses_unsafe_opener() {
    let first = "  1 [a](\u{301}https://example.com/aaaa/bbbb";
    let lines = padded_rows(&[first, "    cccc)"], 40, None);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &lines);
    for (row, col) in [(0, 12), (1, 5)] {
        let display = app.cell_target_at(window, pane, row, col).map(|target| target.display);
        assert!(display.as_deref().is_none_or(|display| !display.contains("cccc")), "{display:?}");
    }
}

/// A soft wrap into a later continuation means that row is not the box's hard-broken text.
#[test]
fn padded_markdown_refuses_soft_wrap_after_first_continuation() {
    let first = "  1 [x](https://example.test/aaaaaaaa";
    let last = first.len() - 1;
    let middle = format!("    {}   ", "b".repeat(last + 1 - 4));
    // The middle row fills the grid, so four more blanks wrap and place `cc)` at the content column.
    let output = format!("{first}\r\n{middle}    cc)");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 40, 10, &[output]);
    assert!(
        app.registry.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(2)
            .unwrap()
            .soft_wrapped_from_previous(),
        "fixture must soft-wrap into the third row"
    );
    assert_eq!(
        app.registry.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(2)
            .unwrap()
            .iter()
            .position(|cell| cell.ch == 'c'),
        Some(4),
        "the wrapped closer row must start its body at the content column"
    );
    let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
    assert!(display.as_deref().is_none_or(|display| !display.contains("bbb")), "{display:?}");
}

/// Build a padded chain in a `cols`-wide pane whose rows after the first fill to the opener row's
/// cut, ending with `last_body` and the closer.
fn padded_chain(cols: usize, middle_rows: usize, last_body: &str) -> (Vec<String>, String) {
    let head = "  1 [x](https://example.test/";
    let first = format!("{head}{}", "a".repeat(cols - 1 - head.len()));
    let middle = format!("    {}", "b".repeat(first.len() - 4));
    let mut uri = first[8..].to_string();
    let mut lines = vec![first];
    for _ in 0..middle_rows {
        uri.push_str(&middle[4..]);
        lines.push(middle.clone());
    }
    uri.push_str(last_body);
    lines.push(format!("    {last_body})"));
    (lines, uri)
}

/// A chain of exactly `MAX_WRAPPED_PATH_ROWS` rows joins; one more row keeps the first row alone.
#[test]
fn padded_markdown_enforces_row_bound() {
    for (total_rows, joins) in [(MAX_WRAPPED_PATH_ROWS, true), (MAX_WRAPPED_PATH_ROWS + 1, false)] {
        let (lines, uri) = padded_chain(50, total_rows - 2, "zz");
        let lines = padded_rows(&lines.iter().map(String::as_str).collect::<Vec<_>>(), 49, None);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 60, 40, &lines);
        let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
        assert_eq!(
            display.as_deref() == Some(uri.as_str()),
            joins,
            "rows {total_rows}: {display:?}"
        );
        assert!(joins || display.as_deref().is_none_or(|display| !display.contains("zz")));
    }
}

/// A joined target of exactly `MAX_LOGICAL_PATH_BYTES` joins; one byte more keeps the first row alone.
#[test]
fn padded_markdown_enforces_byte_bound() {
    for (extra, joins) in [(0usize, true), (1, false)] {
        let (probe, _) = padded_chain(140, 29, "");
        let fixed = probe[0].len() - 8 + 29 * (probe[1].len() - 4);
        let last_body = "z".repeat(MAX_LOGICAL_PATH_BYTES - fixed + extra);
        let (lines, uri) = padded_chain(140, 29, &last_body);
        assert_eq!(uri.len(), MAX_LOGICAL_PATH_BYTES + extra);
        let lines = padded_rows(&lines.iter().map(String::as_str).collect::<Vec<_>>(), 139, None);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 150, 40, &lines);
        let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
        assert_eq!(display.as_deref() == Some(uri.as_str()), joins, "extra {extra}");
    }
}

/// The edge-reaching bracketed rule accepts any non-ASCII text right after its closer, since no
/// URL character is non-ASCII, while URL text after the closer still makes the boundary ambiguous.
#[test]
fn hardwrapped_closer_followed_by_non_ascii_resolves() {
    for (suffix, joins) in [("。", true), ("了解更多", true), ("x", false)] {
        let mut lines = hardwrap_lines(39, HARDWRAP_HEAD, ISSUE_URI, &[2], ')');
        lines[1].push_str(suffix);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane(&mut app, 39, &lines);
        let target = app.cell_target_at(window, pane, 1, 3);
        assert_eq!(
            target.map(|target| target.display),
            joins.then(|| ISSUE_URI.to_string()),
            "suffix {suffix:?}"
        );
    }
}

/// A closer at the grid's edge whose row soft-wraps into URL text is not the target's end.
#[test]
fn padded_markdown_refuses_closer_that_wraps_on() {
    let output = format!("  1 [x](https://example.test/a\r\n    {})tail)", "b".repeat(35));
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 40, 10, &[output]);
    assert!(
        app.registry.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(2)
            .unwrap()
            .soft_wrapped_from_previous(),
        "fixture must soft-wrap after the closer"
    );
    let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
    assert!(display.as_deref().is_none_or(|display| !display.contains("bbb")), "{display:?}");
}

/// The text after the closer is read only up to the pane's border, so another pane's text there
/// never decides whether this pane's target ends. Both rows reach the border with no padding.
#[test]
fn padded_markdown_closer_tail_stops_at_pane_border() {
    let first = "  1 [x](https://example.test/aaaa/bbbb";
    let first = format!("{first}{}", "c".repeat(40 - first.len()));
    let tail = "d".repeat(35);
    let uri = format!("{}{tail}", &first[8..]);
    let second = format!("    {tail})");
    assert_eq!((first.len(), second.len()), (40, 40), "both rows must end at the border");
    for neighbour in ["X", " "] {
        let lines =
            [&first, &second].iter().map(|line| format!("{line}│{neighbour}")).collect::<Vec<_>>();
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &lines);
        let target = app.cell_target_at(window, pane, 1, 5).expect("continuation target");
        assert_eq!(target.display, uri, "neighbour {neighbour:?}");
    }
}

/// Text another hyperlink owns is never joined into a padded target's reconstructed destination.
#[test]
fn padded_markdown_never_joins_hyperlinked_fragment() {
    let first = "  1 [x](https://example.test/aaaa/bbbb";
    let output = format!(
        "{first}{}\r\n    \x1b]8;;https://declared.test/\x1b\\cccc\x1b]8;;\x1b\\)",
        " ".repeat(6)
    );
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &[output]);
    assert_eq!(
        app.cell_target_at(window, pane, 1, 5).expect("OSC 8 fragment").display,
        "https://declared.test/"
    );
    let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
    assert!(display.as_deref().is_none_or(|display| !display.contains("cccc")), "{display:?}");
}

/// A padded chain whose closer is below the displayed frame, or whose opener is above it, is never
/// reconstructed, and the rule reads no row outside the frame.
#[test]
fn padded_markdown_stays_inside_the_frame() {
    let uri = "https://example.test/aaaa/bbbb";
    let lines = padded_rows(
        &[&format!("  1 [x]({}", &uri[..24]), &format!("    {})", &uri[24..])],
        34,
        None,
    );
    // Two hard rows written into a one-row grid: the opener scrolls into history.
    let mut grid = Grid::new(60, 1);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    // A one-row view shows either the opener (closer below) or the closer (opener above).
    for (view_top, row, col) in [(0u64, 0u64, 12u16), (1, 1, 5)] {
        let frame = FrameRows::new(&grid, view_top);
        let decided = padded_markdown_uri_in(&frame, AbsoluteCell { row, col });
        assert_eq!(decided, HardwrapUri::NotApplicable, "view_top {view_top}");
    }
}

/// A bracketed closer whose non-ASCII tail runs to the grid edge and soft-wraps into URL text is
/// not proven final, so the edge rule stays inert rather than opening the shorter destination.
#[test]
fn hardwrapped_closer_tail_that_wraps_on_stays_inert() {
    let output = format!("x (https://example.com/aaaaaaa\r\n  bb){}tail)", "é".repeat(25));
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 30, 10, &[output]);
    assert!(
        app.registry.windows[&window].panes[&pane]
            .parser
            .lock()
            .grid()
            .row_at_abs(2)
            .unwrap()
            .soft_wrapped_from_previous(),
        "fixture must soft-wrap after the tail"
    );
    for (row, col) in [(0, 5), (1, 2)] {
        assert!(
            app.cell_target_at(window, pane, row, col)
                .is_none_or(|target| target.display != "https://example.com/aaaaaaabb"),
            "row {row}"
        );
    }
}

/// Every structural cell the padded join removes (`]`, `(`, the closer and the repeated sign) must
/// be plain: a combining mark or OSC 8 ownership on any of them prevents the join. A wide cell
/// cannot hold these ASCII characters, so the wide-flag case does not arise from real output.
#[test]
fn padded_markdown_structural_cells_must_be_plain() {
    let combining = |character: char| format!("{character}\u{301}");
    let hyperlinked =
        |character: char| format!("\x1b]8;;https://declared.test/\x1b\\{character}\x1b]8;;\x1b\\");
    let plain = |character: char| character.to_string();
    let mutations: [(&str, &dyn Fn(char) -> String); 2] =
        [("combining", &combining), ("hyperlink", &hyperlinked)];
    // Each structural cell is marked alone: the opener row's sign, `]`, `(`, the closer and the
    // continuation's repeated sign.
    for marked in ["first sign", "]", "(", ")", "second sign"] {
        for (label, mutate) in mutations {
            let cell = |position: &str, character: char| {
                if position == marked {
                    mutate(character)
                } else {
                    plain(character)
                }
            };
            let first = format!(
                "    59 {}see [Nerd Font{}{}https://github.com/ryanoasis/nerd-fon{}",
                cell("first sign", '+'),
                cell("]", ']'),
                cell("(", '('),
                " ".repeat(7)
            );
            let second =
                format!("       {}ts/wiki{} and more", cell("second sign", '+'), cell(")", ')'));
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &[first, second]);
            for (row, col) in [(0, 30), (1, 9)] {
                let display =
                    app.cell_target_at(window, pane, row, col).map(|target| target.display);
                assert!(
                    display.as_deref().is_none_or(|display| !display.contains("nerd-fonts/wiki")),
                    "{label} on {marked} at row {row}: {display:?}"
                );
            }
        }
    }
    // Unmutated, the same fixture joins, so each refusal above comes from the marked cell.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let first =
        format!("    59 +see [Nerd Font](https://github.com/ryanoasis/nerd-fon{}", " ".repeat(7));
    let (window, pane) =
        hardwrap_pane_sized(&mut app, 90, 10, &[first, "       +ts/wiki) and more".into()]);
    assert_eq!(
        app.cell_target_at(window, pane, 1, 9).expect("plain fixture").display,
        "https://github.com/ryanoasis/nerd-fonts/wiki"
    );
}

/// Write `lines` as hard rows into a `cols`-wide grid of `rows` rows, the first row on top.
fn hard_rows_grid(cols: u16, rows: u16, lines: &[String]) -> Grid {
    let mut grid = Grid::new(cols, rows);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    grid
}

/// A wide or wide-continuation flag on any structural cell the padded join removes refuses it;
/// the same grid unflagged joins.
#[test]
fn padded_markdown_structural_cells_refuse_wide_flags() {
    let first =
        format!("    59 +see [Nerd Font](https://github.com/ryanoasis/nerd-fon{}", " ".repeat(7));
    let lines = [first.clone(), "       +ts/wiki) and more".to_string()];
    let opener = first.find('(').unwrap();
    // The structural cells: R0's sign, `]`, `(`, the closer and the repeated sign.
    let positions = [(0u16, 7usize), (0, opener - 1), (0, opener), (1, 15), (1, 7)];
    let decide = |grid: &Grid| {
        padded_markdown_uri_in(&FrameRows::new(grid, 0), AbsoluteCell { row: 1, col: 9 })
    };
    assert!(matches!(decide(&hard_rows_grid(90, 10, &lines)), HardwrapUri::Complete(_)));
    for flag in [CellFlags::WIDE, CellFlags::WIDE_CONT] {
        for (row, col) in positions {
            let mut grid = hard_rows_grid(90, 10, &lines);
            grid.row_mut(row).iter_mut().nth(col).unwrap().flags.insert(flag);
            assert_eq!(decide(&grid), HardwrapUri::NotApplicable, "{flag:?} at {row},{col}");
        }
    }
}

/// A visible closer whose tail runs to the grid edge, on a scrolled-back view whose next row is
/// not displayed, may continue below, so neither rule joins; with no row below, the edge rule does.
#[test]
fn closer_tail_at_edge_above_undisplayed_row_is_not_proven() {
    let tail = "é".repeat(25);
    let bracketed = ["x (https://example.com/aaaaaaa".to_string(), format!("  bb){tail}")];
    let scrolled =
        hard_rows_grid(30, 2, &[bracketed[0].clone(), bracketed[1].clone(), "tail)".into()]);
    let live = hard_rows_grid(30, 2, &bracketed);
    for (row, col) in [(0u64, 5u16), (1, 2)] {
        let pointed = AbsoluteCell { row, col };
        let (decided, reads) = frame_rows::read_observer::observe(|| {
            hardwrap_uri_in(&FrameRows::new(&scrolled, 0), pointed)
        });
        assert!(!matches!(decided, HardwrapUri::Complete(_)), "scrolled row {row}: {decided:?}");
        assert!(reads.iter().all(|(_, in_frame)| *in_frame), "read outside the frame: {reads:?}");
        assert!(
            matches!(hardwrap_uri_at_cell(&live, 0, pointed), HardwrapUri::Complete(_)),
            "live row {row}"
        );
    }
    // The padded rule: a closer row filled to the grid edge with `é` above an undisplayed row.
    let first = "  1 [x](https://example.test/aaaa";
    let padded = [
        format!("{first}{}", " ".repeat(6)),
        format!("    bbbb){}", "é".repeat(40 - 9)),
        "tail)".to_string(),
    ];
    let grid = hard_rows_grid(40, 2, &padded);
    let pointed = AbsoluteCell { row: 1, col: 5 };
    let (decided, reads) = frame_rows::read_observer::observe(|| {
        padded_markdown_uri_in(&FrameRows::new(&grid, 0), pointed)
    });
    assert_eq!(decided, HardwrapUri::NotApplicable);
    assert!(reads.iter().all(|(_, in_frame)| *in_frame), "read outside the frame: {reads:?}");
}

/// A middle row whose text ends anywhere but the opener row's cut was not wrapped at the box width.
/// One that runs past the cut would otherwise lose its overhang from the joined destination, and
/// one that stops short ends at a blank; either way the first row keeps its own URL.
#[test]
fn padded_markdown_middle_row_must_end_at_the_cut() {
    let first = "  1 [doc](https://example.com/aaaa/bbbb";
    for middle_len in [first.len() - 4 + 3, 4] {
        let middle = format!("    {}", "c".repeat(middle_len));
        let lines = padded_rows(&[first, &middle, "    more)."], first.len() + 8, None);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 90, 10, &lines);
        let display = app.cell_target_at(window, pane, 0, 12).map(|target| target.display);
        assert_eq!(
            display.as_deref(),
            Some("https://example.com/aaaa/bbbb"),
            "middle {middle_len}"
        );
    }
}

/// The live twin of the padded undisplayed-tail case: with no row below the closer row, a non-ASCII
/// tail that reaches the grid's edge ends the target, and the padded chain joins.
#[test]
fn padded_markdown_closer_tail_at_edge_joins_on_the_live_view() {
    let first = "  1 [x](https://example.test/aaaa";
    let padded = [format!("{first}{}", " ".repeat(6)), format!("    bbbb){}", "é".repeat(40 - 9))];
    let grid = hard_rows_grid(40, 2, &padded);
    let decided =
        padded_markdown_uri_in(&FrameRows::new(&grid, 0), AbsoluteCell { row: 1, col: 5 });
    let HardwrapUri::Complete(candidate) = decided else {
        panic!("live twin must join: {decided:?}")
    };
    assert_eq!(candidate.target, DetectedTarget::Uri("https://example.test/aaaabbbb".into()));
}
