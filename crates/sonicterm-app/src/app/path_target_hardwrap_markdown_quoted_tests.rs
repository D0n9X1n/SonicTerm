//! Numbered box lines that hold a URL mid-row: quoted arguments, code comments and prose.

use super::super::path_target_tests::*;
use super::*;

/// A long URL in a quoted command argument, as Claude Code's diff view shows it.
const QUOTED_URI: &str = "https://github.com/iamcco/markdown-preview.nvim/releases/download/v$version/markdown-preview-win.zip";

/// The diff gutter the view draws: blanks, a two-digit line number, a blank, then the sign column.
fn gutter(number: u32, sign: char) -> String {
    format!("    {number:>2} {sign}")
}

/// Lay one source line out as the box does: `head` (its gutter) and `text` cut every `cut_cols`
/// columns, each continuation hanging at `hang` with the line's sign repeated just before it.
fn box_line(head: &str, sign: char, text: &str, cut_cols: usize, hang: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let first_len = (cut_cols - head.len()).min(text.len());
    rows.push(format!("{head}{}", &text[..first_len]));
    let mut rest = &text[first_len..];
    while !rest.is_empty() {
        let take = (cut_cols - hang).min(rest.len());
        rows.push(format!("{}{sign}{}", " ".repeat(hang - 1), &rest[..take]));
        rest = &rest[take..];
    }
    rows
}

/// The spans a URL of `uri_len` bytes covers when it starts at `(row, col)` of a box line cut at
/// `cut_cols` whose continuations hang at `hang`.
fn url_spans(
    row: u16,
    col: usize,
    uri_len: usize,
    cut_cols: usize,
    hang: usize,
) -> Vec<(u16, u16, u16)> {
    let mut spans = Vec::new();
    let (mut row, mut col, mut left) = (row, col, uri_len);
    while left > 0 {
        let take = (cut_cols - col).min(left);
        spans.push((row, col as u16, (col + take) as u16));
        left -= take;
        row += 1;
        col = hang;
    }
    spans
}

/// Every pointer inside `spans` resolves to `expected` with exactly those spans highlighted.
fn assert_joined(lines: &[String], cols: u16, spans: &[(u16, u16, u16)], expected: &str) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, cols, 16, lines);
    for &(row, start, end) in spans {
        for col in start..end {
            let target = app
                .cell_target_at(window, pane, row, col)
                .unwrap_or_else(|| panic!("no target at row {row} col {col}"));
            assert_eq!(target.display, expected, "row {row} col {col}");
            let cells = target.hovered(true).expect("highlight").cells;
            let found = cells
                .spans()
                .iter()
                .map(|span| (span.row, span.start_col, span.end_col))
                .collect::<Vec<_>>();
            assert_eq!(found, spans, "row {row} col {col}");
        }
    }
}

/// The screenshot: a context row whose quoted URL the box cuts mid-word, among removed and added
/// comment rows cut at the same width. Every fragment resolves the whole URL, without the quotes,
/// the gutter or the signs the comment rows repeat.
#[test]
fn quoted_url_on_context_row_joins_at_the_box_cut() {
    const CUT: usize = 79;
    const HANG: usize = 9;
    let mut lines = Vec::new();
    lines.extend(box_line(&gutter(85, ' '), ' ', "          try {", CUT, HANG));
    lines.extend(box_line(
        &gutter(86, ' '),
        ' ',
        "            $zip = Join-Path $temp 'preview.zip'",
        CUT,
        HANG,
    ));
    let url_row = lines.len();
    let source =
        format!("            Invoke-WebRequest -UseBasicParsing \"{QUOTED_URI}\" -OutFile $zip");
    lines.extend(box_line(&gutter(87, ' '), ' ', &source, CUT, HANG));
    let removed = "            # .NET, not Expand-Archive: started from pwsh, Windows PowerShell can load pwsh's incompatible Archive module.";
    lines.extend(box_line(&gutter(88, '-'), '-', removed, CUT, HANG));
    let added = "            # Unzip with .NET: started from pwsh, Windows PowerShell can load pwsh's incompatible Archive module.";
    lines.extend(box_line(&gutter(88, '+'), '+', added, CUT, HANG));

    let start = lines[url_row].find("https").unwrap();
    assert_eq!(lines[url_row].len(), CUT, "the URL row must reach the cut");
    let row = url_row as u16;
    let spans = url_spans(row, start, QUOTED_URI.len(), CUT, HANG);
    let (end_row, _, end_col) = *spans.last().unwrap();
    assert_joined(&lines, CUT as u16 + 6, &spans, QUOTED_URI);
    // The closing quote, the gutter and the opening quote are not part of the destination.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, CUT as u16 + 6, 16, &lines);
    for (row, col) in [(end_row, end_col), (row + 1, 7), (row, start as u16 - 1)] {
        assert!(app.cell_target_at(window, pane, row, col).is_none(), "row {row} col {col}");
    }
}

/// On an added or removed row each continuation repeats the sign, which is gutter, not URL text,
/// whether it sits in the head's sign column or one column right of it; single and double quotes
/// both end the URL.
#[test]
fn quoted_url_on_signed_row_skips_the_repeated_sign() {
    for hang in [8, 9] {
        for quote in ['"', '\''] {
            for sign in ['+', '-'] {
                let source = format!("  get {quote}{QUOTED_URI}{quote} -o out");
                let lines = box_line(&gutter(87, sign), sign, &source, 70, hang);
                let start = lines[0].find("https").unwrap();
                let spans = url_spans(0, start, QUOTED_URI.len(), 70, hang);
                assert_joined(&lines, 80, &spans, QUOTED_URI);
            }
        }
    }
}

/// A bare URL in a code comment that the box cuts several times joins across every row, and the
/// prose after it on the last row is not part of it.
#[test]
fn bare_url_in_comment_joins_across_rows() {
    let uri = "https://example.com/docs/aaaaaaaaaa/bbbbbbbbbb/cccccccccc/dddddddddd/eeeeeeeeee";
    let source = format!("  // see {uri} for details");
    let lines = box_line(&gutter(12, ' '), ' ', &source, 40, 9);
    let start = lines[0].find("https").unwrap();
    let spans = url_spans(0, start, uri.len(), 40, 9);
    assert!(spans.len() >= 3, "fixture must wrap the URL over at least three rows");
    assert_joined(&lines, 60, &spans, uri);
}

/// A URL that starts on a continuation row of a long prose line, and wraps into the next one, is
/// found in the rebuilt line like any other.
#[test]
fn url_starting_on_a_continuation_row_joins() {
    let uri = "https://example.com/guide/aaaaaaaaaaaaaaaaaaaa";
    let source = format!("  This long line wraps well before the link at {uri}.");
    let lines = box_line(&gutter(3, ' '), ' ', &source, 40, 9);
    let (url_row, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| Some((index, line.find("https")?)))
        .unwrap();
    assert!(url_row > 0, "fixture must start the URL on a continuation row");
    let spans = url_spans(url_row as u16, start, uri.len(), 40, 9);
    assert_joined(&lines, 60, &spans, uri);
}

/// A closing quote alone on the continuation row ends a URL that filled the row above exactly.
#[test]
fn closer_alone_on_continuation_keeps_the_row_url() {
    let uri = "https://example.com/aaaa/bbbb";
    let source = format!("  get \"{uri}\" -o out");
    let head = gutter(7, ' ');
    let cut = head.len() + "  get \"".len() + uri.len();
    let lines = box_line(&head, ' ', &source, cut, 9);
    assert!(lines[1].trim_start().starts_with('"'), "fixture must put the closer on the next row");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &lines);
    let col = lines[0].find("https").unwrap() as u16 + 3;
    let display = app.cell_target_at(window, pane, 0, col).map(|target| target.display);
    assert_eq!(display.as_deref(), Some(uri));
}

/// A URL that ends at the cut keeps its own row when the next row is a new numbered line, a blank
/// row, or a continuation that starts right of the box's hang column, whose source began with a
/// blank.
#[test]
fn box_url_refusals_keep_the_row_url() {
    let prefix = "https://example.com/aaaa/bbbb";
    let first = format!("{}  get \"{prefix}", gutter(7, ' '));
    let cut = first.len();
    let cases: [(&str, String); 3] = [
        ("next line", format!("{}  cccc\" -o out", gutter(8, ' '))),
        ("blank row", String::new()),
        ("blank at the hang", "          cccc\" -o out".to_string()),
    ];
    for (label, second) in cases {
        // A later line's continuation fixes the box's hang column at 9.
        let lines = [
            first.clone(),
            second,
            format!("{}  {}", gutter(9, ' '), "x".repeat(cut - 10)),
            format!("         {}", "y".repeat(10)),
        ];
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &lines);
        let col = first.find("https").unwrap() as u16 + 3;
        let display = app.cell_target_at(window, pane, 0, col).map(|target| target.display);
        assert_eq!(display.as_deref(), Some(prefix), "{label}");
    }
}

/// Rows captured from Claude Code's Update view in a 168-column pane: the box cuts at column 161
/// and hangs every continuation at the content column, two columns after the line number. The
/// same geometry with the cut moved inside the URL joins it, and `$zip` stays one word.
#[test]
fn captured_update_view_rows_join_at_their_cut() {
    let invoke = "      4      Invoke-WebRequest -UseBasicParsing \"https://github.com/iamcco/markdown-preview.nvim/releases/download/v$version/markdown-preview-win.zip\" -OutFile $";
    let captured = [
        "      2    try {".to_string(),
        "      3      $zip = Join-Path $temp \"preview.zip\"".to_string(),
        invoke.to_string(),
        "         zip".to_string(),
        "      5 -    # first comment line that is long enough to wrap inside the diff box of the terminal user interface for sure".to_string(),
        "      5 +    # second comment line that is long enough to wrap inside the diff box of the terminal user interface for sure".to_string(),
        "      6    }".to_string(),
    ];
    assert_eq!(invoke.len(), 161, "the captured row reaches the box's cut");
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 168, 12, &captured);
    let col = invoke.find("https").unwrap() as u16 + 4;
    let display = app.cell_target_at(window, pane, 2, col).map(|target| target.display);
    assert_eq!(display.as_deref(), Some(QUOTED_URI));

    // The same box with the cut inside the URL: the URL row reaches column 161, the rest hangs at 9.
    let head = "      4      Invoke-WebRequest -UseBasicParsing \"";
    let source = format!("{}{QUOTED_URI}\" -OutFile $zip", &head[7..]);
    let lines = box_line(&head[..7], ' ', &source, 161 - QUOTED_URI.len() + 40, 9);
    let start = lines[0].find("https").unwrap();
    let spans = url_spans(0, start, QUOTED_URI.len(), lines[0].len(), 9);
    assert_joined(&lines, 168, &spans, QUOTED_URI);
}
