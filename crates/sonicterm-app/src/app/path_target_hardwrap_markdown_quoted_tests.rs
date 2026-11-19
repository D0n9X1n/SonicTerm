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
    // The pane holds every line, so no row scrolls into history.
    let pane_rows = lines.len() as u16 + 4;
    let (window, pane) = hardwrap_pane_sized(&mut app, cols, pane_rows, lines);
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

/// Resolve the target at `(row, col)` of `lines` written into a `cols`-wide pane.
fn target_at(lines: &[String], cols: u16, row: u16, col: u16) -> Option<String> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, cols, 40, lines);
    app.cell_target_at(window, pane, row, col).map(|target| target.display)
}

/// A URL the box broke across rows opens whole or not at all: a continuation that opens its own
/// scheme, an unsafe cell anywhere in the token (a combining mark, an OSC 8 link or an orphan wide
/// continuation, also after trimmed punctuation or on a non-ASCII cell), or a token longer than
/// `MAX_LOGICAL_PATH_BYTES` leaves every fragment inert instead of opening the first row's prefix.
#[test]
fn broken_url_with_unreadable_fragment_never_opens_a_prefix() {
    let first = format!("{}  get \"https://example.test/aaaa", gutter(1, ' '));
    let cases = [
        ("second scheme", "         https://b.test/x\"".to_string()),
        ("combining on the first fragment cell", "         b\u{301}bbb\"".to_string()),
        ("combining after trimmed punctuation", "         bbbb)b\u{301}ccc\"".to_string()),
        ("combining on non-ASCII", "         bbbbé\u{301}\"".to_string()),
        (
            "hyperlinked fragment",
            "         \x1b]8;;https://declared.test/\x1b\\bbbb\x1b]8;;\x1b\\\"".to_string(),
        ),
    ];
    for (label, second) in cases {
        let lines = [first.clone(), second];
        for (row, col) in [(0, first.len() as u16 - 3), (1, 10)] {
            let display = target_at(&lines, 60, row, col);
            assert!(
                display
                    .as_deref()
                    .is_none_or(|display| !display.starts_with("https://example.test")),
                "{label} row {row}: {display:?}"
            );
        }
    }
    // The same rows without the unreadable cell join, so each refusal comes from that cell.
    let plain = [first.clone(), "         bbbb)bccc\"".to_string()];
    assert_eq!(
        target_at(&plain, 60, 0, first.len() as u16 - 3).as_deref(),
        Some("https://example.test/aaaabbbb)bccc")
    );
}

/// A URL one byte past `MAX_LOGICAL_PATH_BYTES` never opens its first rows' prefix.
#[test]
fn broken_url_past_the_byte_cap_stays_inert() {
    let uri = format!("https://example.test/{}", "a".repeat(MAX_LOGICAL_PATH_BYTES + 1 - 21));
    assert_eq!(uri.len(), MAX_LOGICAL_PATH_BYTES + 1);
    let lines = box_line(&gutter(1, ' '), ' ', &format!("  \"{uri}\" x"), 150, 9);
    assert!(target_at(&lines, 160, 0, 20).is_none());
    assert!(target_at(&lines, 160, 1, 20).is_none());
}

/// A leading `-` or `+` on a context line's continuation is URL text, not a diff sign.
#[test]
fn context_continuation_opening_with_a_hyphen_joins() {
    let first = format!("{}  get \"https://example.test/markdown", gutter(87, ' '));
    let lines = [first.clone(), "         -preview.zip\"".to_string()];
    assert_eq!(
        target_at(&lines, 60, 0, first.len() as u16 - 3).as_deref(),
        Some("https://example.test/markdown-preview.zip")
    );
}

/// A continuation whose text starts with digits and a blank is not a new numbered line: the URL
/// it starts still joins the next continuation.
#[test]
fn continuation_starting_with_digits_keeps_its_head() {
    let lines = [
        format!("  1 {}", "x".repeat(28)),
        "    123 \"https://a.test/aaaaaaaa".to_string(),
        "    bbbb\"".to_string(),
    ];
    assert_eq!(target_at(&lines, 40, 1, 12).as_deref(), Some("https://a.test/aaaaaaaabbbb"));
}

/// The row cap bounds the joined URL, not the source line: a two-row URL that starts deep in a
/// long wrapped line still joins.
#[test]
fn two_row_url_deep_in_a_long_line_joins() {
    let uri = "https://example.test/aaaaaaaaaabbbbbbbbbb";
    let prose = "word ".repeat(31 * 7);
    let lines = box_line(&gutter(1, ' '), ' ', &format!("  {prose}\"{uri}\" x"), 40, 9);
    let (row, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| Some((index, line.find("https")?)))
        .unwrap();
    assert!(row >= MAX_WRAPPED_PATH_ROWS - 1, "fixture must start the URL deep in the line");
    let spans = url_spans(row as u16, start, uri.len(), 40, 9);
    assert!(spans.len() == 2, "fixture must split the URL over two rows: {spans:?}");
    assert_joined(&lines, 60, &spans, uri);
}

/// A closing quote inside the frame ends a URL even when the view is scrolled back and the line's
/// next row is not displayed.
#[test]
fn quoted_url_closed_above_an_undisplayed_row_joins() {
    let lines = [
        format!("{}  get \"https://example.test/aaaa", gutter(1, ' ')),
        "         bbbb\"".to_string(),
        "         more".to_string(),
    ];
    let mut grid = Grid::new(60, 2);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    // Three rows into a two-row grid: the view shows rows 0 and 1, the third row is below it.
    let frame = FrameRows::new(&grid, 0);
    assert!(frame.rows_below(), "fixture must hide a row below the view");
    let decided = numbered_box_uri_in(&frame, AbsoluteCell { row: 0, col: 20 });
    let HardwrapUri::Complete(candidate) = decided else { panic!("closed URL: {decided:?}") };
    assert_eq!(candidate.target, DetectedTarget::Uri("https://example.test/aaaabbbb".into()));
}

/// Hovering every cell of a large box stays bounded: each decision reads at most a fixed window of
/// rows around the pointer, whatever the number of rows the box holds.
#[test]
fn box_decision_reads_a_bounded_window() {
    let lines = (0..200)
        .map(|index| format!("{} {}", gutter(index % 90 + 10, ' '), "z".repeat(60)))
        .collect::<Vec<_>>();
    let mut grid = Grid::new(80, 200);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let frame = FrameRows::new(&grid, 0);
    let (_, cells) =
        hardwrap_work::measure(|| numbered_box_uri_in(&frame, AbsoluteCell { row: 100, col: 20 }));
    let window_rows = 6 * (MAX_WRAPPED_PATH_ROWS as u64 - 1) + 1;
    assert!(cells <= window_rows * 80, "read {cells} cells");
}

/// Hovering a delimiter that opens a numbered row's text resolves nothing and never panics.
#[test]
fn delimiter_at_the_text_start_resolves_nothing() {
    for line in ["  1 \"hello\"", "  1 {", "  1 (x)"] {
        assert!(target_at(&[line.to_string()], 60, 0, 4).is_none(), "{line:?}");
    }
}

/// A URL that fits both limits joins even when it starts past the rows `MAX_LOGICAL_PATH_BYTES`
/// of prose would fill: the rebuild reads the line around the pointer, not from its head.
#[test]
fn url_after_long_prose_on_a_wide_row_joins() {
    let uri = format!("https://example.test/{}", "a".repeat(980));
    let lines = box_line("  1 ", ' ', &format!("{}\"{uri}\"", "word ".repeat(3200)), 1000, 4);
    let (row, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| Some((index, line.find("https")?)))
        .unwrap();
    let spans = url_spans(row as u16, start, uri.len(), 1000, 4);
    assert!(spans.len() >= 2, "fixture must wrap the URL: {spans:?}");
    assert_joined(&lines, 1010, &spans, &uri);
}

/// A two-row URL whose numbered head is far above it, but still displayed, joins.
#[test]
fn two_row_url_far_below_its_head_joins() {
    let uri = "https://example.test/aaaaaaaaabbbbbbbbcccccccc";
    let lines = box_line("  1 ", ' ', &format!("{} \"{uri}\"", "x".repeat(2276)), 40, 4);
    let (row, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| Some((index, line.find("https")?)))
        .unwrap();
    assert!(row > 2 * MAX_WRAPPED_PATH_ROWS - 2, "fixture must put the URL far below its head");
    let spans = url_spans(row as u16, start, uri.len(), 40, 4);
    assert_eq!(spans.len(), 2, "fixture must split the URL over two rows");
    assert_joined(&lines, 60, &spans, uri);
}

/// When the head is above the displayed frame, a URL whose rows the frame shows still joins, and
/// one that may start above the frame stays inert.
#[test]
fn url_in_a_line_whose_head_scrolled_away() {
    let uri = "https://example.test/aaaaaaaaabbbbbbbbcccccccc";
    let lines = box_line("  1 ", ' ', &format!("{} \"{uri}\" x", "x".repeat(400)), 40, 4);
    let mut lines = lines;
    lines.push(format!("  2 {}", "y".repeat(10)));
    let (row, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| Some((index, line.find("https")?)))
        .unwrap();
    let mut grid = Grid::new(60, 6);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    // The frame shows the URL's rows and the next numbered row, not the line's head.
    let view_top = row as u64 - 2;
    let frame = FrameRows::new(&grid, view_top);
    assert!(view_top > 0 && frame.contains(row as u64 + 1));
    let decided =
        numbered_box_uri_in(&frame, AbsoluteCell { row: row as u64, col: start as u16 + 2 });
    let HardwrapUri::Complete(candidate) = decided else { panic!("headless line: {decided:?}") };
    assert_eq!(candidate.target, DetectedTarget::Uri(uri.into()));
    // On the frame's top row the prose may continue above, so the token there is not proven.
    let top = numbered_box_uri_in(&frame, AbsoluteCell { row: view_top, col: 6 });
    assert!(!matches!(top, HardwrapUri::Complete(_)), "{top:?}");
}

/// A continuation that holds only the repeated sign ends nothing: the URL closed above it opens.
#[test]
fn sign_only_continuation_keeps_the_closed_url() {
    let lines = [
        "  1 +get \"https://example.test/aaaa".to_string(),
        "    +bbbb\" more".to_string(),
        "    +".to_string(),
    ];
    assert_eq!(target_at(&lines, 60, 0, 20).as_deref(), Some("https://example.test/aaaabbbb"));
}

/// A blank cell carrying a combining mark is unreadable text, not padding, and a continuation's
/// sign carrying one is unreadable too: either leaves the broken URL inert instead of opening a
/// joined or truncated destination.
#[test]
fn unreadable_blank_or_sign_keeps_the_url_inert() {
    let cases = [
        ("combining blank", ["  1 get \"https://example.test/aaaa \u{301}", "    bbbb\""]),
        ("combining sign", ["  1 +get \"https://example.test/aaaa", "    +\u{301}bbbb\""]),
    ];
    for (label, lines) in cases {
        let lines = lines.map(str::to_string);
        assert_eq!(target_at(&lines, 60, 0, 20), None, "{label}");
    }
}
