//! Numbered box lines whose rows must prove a Claude Code box, and the layouts it allows, before
//! a URL broken across them opens.

use super::super::path_target_tests::*;
use super::path_target_hardwrap_markdown_quoted_tests::{
    box_line, decide_in_frame, gutter, target_at, url_spans,
};
use super::*;

/// A numbered list whose every item opens with `- ` reads as markdown and as an all-signed diff
/// alike: a broken URL whose continuation opens with `-` would join with or without that `-`, so it
/// stays inert rather than opening either guess.
#[test]
fn all_signed_box_leaves_a_hyphen_continuation_inert() {
    let lines = [
        "     86 - item one".to_string(),
        "     87 - curl \"https://example.test/markdown".to_string(),
        "         -preview.zip\"".to_string(),
        "     88 - item three".to_string(),
    ];
    for (row, col) in [(1, 25), (2, 12)] {
        let display = target_at(&lines, 60, row, col);
        assert!(
            display.as_deref().is_none_or(|display| !display.starts_with("https://example.test")),
            "row {row}: {display:?}"
        );
    }
}

/// Indented tool output whose row opens with a number is no box: a complete URL on it stays the
/// row's own, never joined with the status text below it.
#[test]
fn numeric_tool_output_keeps_its_own_url() {
    let lines = [
        "  ⎿  Status URL".to_string(),
        "     200 https://example.test/ok".to_string(),
        "         fetched successfully".to_string(),
    ];
    // The pane shows exactly these rows: `target_at` would add a next numbered line below them.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, 60, 10, &lines);
    let display = app.cell_target_at(window, pane, 1, 20).map(|target| target.display);
    assert_eq!(display.as_deref(), Some("https://example.test/ok"));
}

/// A one-line file's Update shows a single line number, removed then added: that replaced line
/// proves the diff box, so the added row's wrapped URL joins without its repeated sign.
#[test]
fn replaced_line_alone_proves_the_box() {
    let lines = [
        "      1 - get \"https://example.test/old\"".to_string(),
        "      1 + get \"https://example.test/aaaa".to_string(),
        "        +bbbb\"".to_string(),
    ];
    let decided = decide_in_frame(&lines, (60, 3), 0, AbsoluteCell { row: 1, col: 20 });
    let HardwrapUri::Complete(candidate) = decided else { panic!("replaced line: {decided:?}") };
    assert_eq!(candidate.target, DetectedTarget::Uri("https://example.test/aaaabbbb".into()));
}

/// Resolve the target at `(row, col)` of exactly `lines` in a `cols` by `pane_rows` pane, with no
/// numbered line added below them.
fn exact_target_at(
    lines: &[String],
    (cols, pane_rows): (u16, u16),
    row: u16,
    col: u16,
) -> Option<String> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (window, pane) = hardwrap_pane_sized(&mut app, cols, pane_rows, lines);
    app.cell_target_at(window, pane, row, col).map(|target| target.display)
}

/// A box whose every row leaves the column after the gutter blank may be indented markdown or a
/// diff's context: the continuation one column further in either began with a source blank or
/// continues the URL, so the box never opens the joined guess.
#[test]
fn unsigned_indented_box_never_joins_a_guessed_continuation() {
    let lines = [
        "      1  see https://example.test/aaaa".to_string(),
        "         next".to_string(),
        "      2  other".to_string(),
    ];
    let display = exact_target_at(&lines, (45, 8), 0, 20);
    assert_ne!(display.as_deref(), Some("https://example.test/aaaanext"));
}

/// A box showing a single line number is not proven, but a markdown `](`…`)` pair wrapped in it
/// still joins: the delimiters show the rows hold one source line, as a padded markdown target.
#[test]
fn one_line_box_joins_a_markdown_pair() {
    let lines =
        ["      1 [doc](https://example.test/aaaa".to_string(), "        bbbb)".to_string()];
    assert_eq!(
        exact_target_at(&lines, (46, 8), 0, 20).as_deref(),
        Some("https://example.test/aaaabbbb")
    );
}

/// The cut comes from every displayed row of the box: a wide row far above the URL sets it, so a
/// URL row that stops short ended at a source blank and never runs into the next row.
#[test]
fn box_cut_comes_from_distant_rows() {
    let mut lines = vec![format!("      1  {}", "x".repeat(30))];
    lines.extend((2..=33).map(|number| format!("{number:>7}  short")));
    lines.push("     34  see https://example.test/aaaa ".to_string());
    lines.push("         next".to_string());
    lines.push("     35  done".to_string());
    assert_eq!(
        exact_target_at(&lines, (46, 50), 33, 20).as_deref(),
        Some("https://example.test/aaaa")
    );
}

/// Numbered tool output whose consecutive rows share a width is no box: a row that reaches that
/// width and ends shows the width is no cut, so each URL stays its row's own.
#[test]
fn consecutive_status_rows_keep_their_own_urls() {
    let lines = [
        "  ⎿  Status URLs".to_string(),
        "     200 https://example.test/old".to_string(),
        "         fetched successfully".to_string(),
        "     201 https://example.test/new".to_string(),
    ];
    assert_eq!(
        exact_target_at(&lines, (60, 8), 1, 20).as_deref(),
        Some("https://example.test/old")
    );
}

/// An Update view that only adds lines shows context rows beside added rows, which proves a diff:
/// a URL wrapped on an added row and one wrapped on a context row both join, without the signs.
#[test]
fn added_lines_beside_context_rows_join() {
    let uri = "https://example.test/aaaaaaaaaa/bbbbbbbbbb/cccccccccc";
    for (url_sign, other_sign) in [('+', ' '), (' ', '+')] {
        let mut lines = box_line(&gutter(85, ' '), ' ', "  try {", 40, 8);
        lines.extend(box_line(&gutter(86, other_sign), other_sign, "    x = 1", 40, 8));
        let url_row = lines.len();
        let source = format!("    get \"{uri}\" -o out");
        lines.extend(box_line(&gutter(87, url_sign), url_sign, &source, 40, 8));
        lines.extend(box_line(&gutter(88, ' '), ' ', "  }", 40, 8));
        let start = lines[url_row].find("https").unwrap();
        let spans = url_spans(url_row as u16, start, uri.len(), 40, 8);
        let decided = decide_in_frame(
            &lines,
            (60, lines.len() as u16),
            0,
            AbsoluteCell { row: url_row as u64, col: start as u16 + 2 },
        );
        let HardwrapUri::Complete(candidate) = decided else {
            panic!("sign {url_sign:?}: {decided:?}")
        };
        assert_eq!(candidate.target, DetectedTarget::Uri(uri.into()), "sign {url_sign:?}");
        let found = candidate
            .spans
            .iter()
            .map(|span| (span.row, span.start_col, span.end_col))
            .collect::<Vec<_>>();
        let expected =
            spans.iter().map(|&(row, start, end)| (row as u64, start, end)).collect::<Vec<_>>();
        assert_eq!(found, expected, "sign {url_sign:?}");
    }
}

/// Signed rows beside a context row prove a diff even when a neighbouring line exactly fills the
/// box's width: the added row's wrapped URL joins rather than opening its first row's prefix.
#[test]
fn diff_box_with_an_exact_width_line_still_joins() {
    let lines = [
        "     87 +get \"https://example.test/aaaa".to_string(),
        "        +bbbb\"".to_string(),
        "     88  result = download_and_parse();".to_string(),
    ];
    assert_eq!(lines[0].len(), lines[2].len(), "the next line fills the box's width");
    let decided = decide_in_frame(&lines, (60, 10), 0, AbsoluteCell { row: 0, col: 24 });
    let HardwrapUri::Complete(candidate) = decided else { panic!("{decided:?}") };
    assert_eq!(candidate.target, DetectedTarget::Uri("https://example.test/aaaabbbb".into()));
}

/// In a box the rows do not prove, a markdown pair still joins only when both layouts read the
/// same URL: a continuation opening with `-` in an all-signed box may be a sign or text.
#[test]
fn unproven_signed_markdown_pair_needs_both_readings() {
    let lines = [
        "     87 - [doc](https://example.test/aaaa".to_string(),
        "        -preview.zip)".to_string(),
        "     88 - More installation instructions.".to_string(),
    ];
    let decided = decide_in_frame(&lines, (60, 10), 0, AbsoluteCell { row: 0, col: 24 });
    assert_eq!(decided, HardwrapUri::Incomplete);
}

/// Numbered tool output that holds two URLs on consecutive rows proves no box: each URL stays its
/// row's own, never refused as a broken one.
#[test]
fn unproven_rows_keep_independent_urls() {
    let lines = [
        "     302 https://example.test/redirect".to_string(),
        "         https://new.test/".to_string(),
    ];
    for (row, col, expected) in
        [(0, 15, "https://example.test/redirect"), (1, 12, "https://new.test/")]
    {
        let display = exact_target_at(&lines, (60, 8), row, col);
        assert_eq!(display.as_deref(), Some(expected), "row {row}");
    }
}

/// A signed row shaped like a markdown bullet beside an indented row proves no diff: in a
/// markdown view the continuation's `-` is text, so the URL is never opened without it.
#[test]
fn bullet_rows_beside_indented_rows_prove_no_diff() {
    let lines = [
        "     87 - curl \"https://example.test/markdown".to_string(),
        "        -preview.zip\"".to_string(),
        "     88   More installation instructions now.".to_string(),
    ];
    for (row, col) in [(0u64, 24u16), (1, 12)] {
        let decided = decide_in_frame(&lines, (60, 10), 0, AbsoluteCell { row, col });
        let wrong = DetectedTarget::Uri("https://example.test/markdownpreview.zip".into());
        assert!(
            !matches!(&decided, HardwrapUri::Complete(candidate) if candidate.target == wrong),
            "row {row}: {decided:?}"
        );
    }
}

/// A wrapped markdown link with a title after its URL is owned by its `](`: it never opens the
/// first row's prefix.
#[test]
fn titled_markdown_link_never_opens_a_prefix() {
    let lines = [
        "      1 [doc](https://example.test/aaaa".to_string(),
        "        bbbb \"Documentation\")".to_string(),
    ];
    let display = exact_target_at(&lines, (60, 10), 0, 24);
    assert_ne!(display.as_deref(), Some("https://example.test/aaaa"));
}

/// Numbered tool output whose next row starts at the box's text column proves no box, and its
/// URL, opened by no markdown `](`, stays its row's own.
#[test]
fn unproven_rows_keep_a_url_above_plain_text() {
    let lines = [
        "  ⎿  Status URL".to_string(),
        "     200 https://example.test/ok".to_string(),
        "     fetched successfully".to_string(),
    ];
    assert_eq!(
        exact_target_at(&lines, (60, 10), 1, 20).as_deref(),
        Some("https://example.test/ok")
    );
}

/// A wrapped markdown link inside emphasis ends at the `)` that closes its destination: the `**`
/// after it is markdown, never part of the URL.
#[test]
fn emphasised_markdown_link_ends_at_its_closer() {
    let lines = [
        "      1 **[SSH setup](https://docs.github.com/en/authentication/connectin".to_string(),
        "        g-to-github-with-ssh)**".to_string(),
        "      2 Next steps.".to_string(),
    ];
    let expected = "https://docs.github.com/en/authentication/connecting-to-github-with-ssh";
    for (row, col) in [(0, 30), (1, 12)] {
        assert_eq!(exact_target_at(&lines, (80, 10), row, col).as_deref(), Some(expected));
    }
}

/// A quoted URL whose path ends in a balanced `)` keeps it: the closing quote, not prose
/// punctuation trimming, ends the URL.
#[test]
fn quoted_url_keeps_its_own_closing_parenthesis() {
    let lines = [
        "     87  const documentation_url = \"https://en.wikipedia.org/wiki/Function".to_string(),
        "         _(mathematics)\";".to_string(),
        "     88 -old();".to_string(),
        "     88 +new();".to_string(),
    ];
    let expected = "https://en.wikipedia.org/wiki/Function_(mathematics)";
    for (row, col) in [(0, 45), (1, 12)] {
        assert_eq!(exact_target_at(&lines, (80, 10), row, col).as_deref(), Some(expected));
    }
}

/// `nl` output whose line numbers widen keeps each row's URL: a narrower number above is no
/// gutter the next row hangs under, since that row writes its own number into it.
#[test]
fn widening_line_numbers_keep_their_urls() {
    let lines = [
        "9      https://example.test/guide".to_string(),
        "10     https://example.test/api".to_string(),
    ];
    assert_eq!(
        exact_target_at(&lines, (80, 10), 1, 18).as_deref(),
        Some("https://example.test/api")
    );
    assert_eq!(
        exact_target_at(&lines, (80, 10), 0, 18).as_deref(),
        Some("https://example.test/guide")
    );
}

/// A quoted URL that holds an apostrophe cannot open, since no URL may hold one; it stays inert
/// rather than opening the part before the apostrophe.
#[test]
fn quoted_url_with_an_apostrophe_never_opens_a_prefix() {
    let lines = [
        "     87  const documentation_url = \"https://en.wikipedia.org/wiki/The_Hit".to_string(),
        "         chhiker's_Guide_to_the_Galaxy\";".to_string(),
        "     88 -old();".to_string(),
        "     88 +new();".to_string(),
    ];
    let display = exact_target_at(&lines, (80, 10), 0, 45);
    assert_ne!(display.as_deref(), Some("https://en.wikipedia.org/wiki/The_Hitchhiker"));
}

/// A linked badge holds an image URL and a link destination in one token: the pointer on the
/// destination opens it, not the image.
#[test]
fn linked_badge_destination_opens_whole() {
    let lines = [
        "      1 [![CI](https://img.shields.io/badge/CI-passing-green)](https://github.com/rust-lang/rust/actions/workflow".to_string(),
        "        s/ci.yml)".to_string(),
        "      2 Next steps.".to_string(),
    ];
    let expected = "https://github.com/rust-lang/rust/actions/workflows/ci.yml";
    for (row, col) in [(0, 70), (1, 10)] {
        assert_eq!(exact_target_at(&lines, (120, 10), row, col).as_deref(), Some(expected));
    }
}

/// A URL opened by a prose `(` runs to the matching `)`, so its own balanced parentheses stay,
/// and sentence punctuation before the closing `)` is not the URL's.
#[test]
fn prose_parenthesis_urls_keep_their_own_parentheses() {
    let cases = [
        (
            "     87  (https://en.wikipedia.org/wiki/Function",
            "         _(mathematics)) for details",
            "https://en.wikipedia.org/wiki/Function_(mathematics)",
        ),
        (
            "     87  (https://example.test/aaaaaaaaaaaaaaaaaaaa",
            "         bbbb.) and more",
            "https://example.test/aaaaaaaaaaaaaaaaaaaabbbb",
        ),
    ];
    for (first, second, expected) in cases {
        let lines = [
            first.to_string(),
            second.to_string(),
            "     88 -old();".to_string(),
            "     88 +new();".to_string(),
        ];
        assert_eq!(
            exact_target_at(&lines, (80, 10), 0, 25).as_deref(),
            Some(expected),
            "{expected}"
        );
    }
}

/// A URL in prose parentheses that ends in `!` or `?` may own that mark or leave it to the
/// sentence, so it stays inert rather than opening either reading.
#[test]
fn prose_parenthesis_url_ending_in_a_mark_stays_inert() {
    let lines = [
        "     87  // Background for the quiz (https://en.wikipedia.org/wiki/Jeopar".to_string(),
        "         dy!) and more.".to_string(),
        "     88 -old();".to_string(),
        "     88 +new();".to_string(),
    ];
    let display = exact_target_at(&lines, (80, 10), 0, 41);
    assert_ne!(display.as_deref(), Some("https://en.wikipedia.org/wiki/Jeopardy"));
}

/// A markdown link whose label is itself a URL opens the label at the label and the destination at
/// the destination, never one URL running through `](`.
#[test]
fn url_label_and_destination_open_separately() {
    let lines = [
        "     87  [https://doc.rust-lang.org/book/ch01-01-installation.html](https://doc.rust-lang.org".to_string(),
        "         /book/ch01-01-installation.html)".to_string(),
        "     88 -Old instructions.".to_string(),
        "     88 +New instructions.".to_string(),
    ];
    let expected = "https://doc.rust-lang.org/book/ch01-01-installation.html";
    for (row, col) in [(0, 14), (0, 80), (1, 15)] {
        assert_eq!(
            exact_target_at(&lines, (100, 10), row, col).as_deref(),
            Some(expected),
            "row {row} col {col}"
        );
    }
}

/// A URL a backtick or `<` opens runs to the matching backtick or `>`, keeping a `)` it holds.
#[test]
fn backtick_and_autolink_urls_keep_their_own_parenthesis() {
    let expected = "https://en.wikipedia.org/wiki/Function_(mathematics)";
    for (open, close) in [('`', '`'), ('<', '>')] {
        let lines = [
            format!(
                "     87  var documentation = {open}https://en.wikipedia.org/wiki/Function_(mat"
            ),
            format!("         hematics){close}"),
            "     88 -old();".to_string(),
            "     88 +new();".to_string(),
        ];
        assert_eq!(exact_target_at(&lines, (80, 10), 0, 34).as_deref(), Some(expected), "{open}");
    }
}

/// A doubled quote inside a quoted URL escapes a quote, as YAML writes an apostrophe: no URL may
/// hold it, so the URL stays inert rather than opening the part before it.
#[test]
fn doubled_quote_in_a_quoted_url_never_opens_a_prefix() {
    let lines = [
        "     87      documentation_url: 'https://en.wikipedia.org/wiki/The_Hitchh".to_string(),
        "         iker''s_Guide_to_the_Galaxy'".to_string(),
        "     88 -    timeout: 10".to_string(),
        "     88 +    timeout: 20".to_string(),
    ];
    let display = exact_target_at(&lines, (80, 10), 0, 37);
    assert_ne!(display.as_deref(), Some("https://en.wikipedia.org/wiki/The_Hitchhiker"));
}

/// An indented markdown link whose continuation opens with `-` joins: a diff could not show that
/// row below a context line, so the markdown reading stands.
#[test]
fn indented_markdown_link_with_a_hyphen_continuation_joins() {
    let lines = [
        "     87   [SSH setup](https://docs.github.com/en/authentication/connecting".to_string(),
        "        -to-github-with-ssh)".to_string(),
        "     88   Next steps.".to_string(),
    ];
    assert_eq!(
        exact_target_at(&lines, (80, 10), 0, 30).as_deref(),
        Some("https://docs.github.com/en/authentication/connecting-to-github-with-ssh")
    );
}

/// A markdown thematic break (`---`) is no removed line: a box holding one beside a list never
/// drops a continuation's `-` as a diff sign.
#[test]
fn thematic_break_proves_no_diff() {
    let lines = [
        "     85 ---".to_string(),
        "     86 ".to_string(),
        "     87 - <https://docs.github.com/en/authentication/connecting".to_string(),
        "        -to-github-with-ssh>".to_string(),
        "     88   Configure your SSH key.".to_string(),
    ];
    for (row, col) in [(2u16, 25u16), (3, 12)] {
        let display = exact_target_at(&lines, (69, 10), row, col);
        assert_ne!(
            display.as_deref(),
            Some("https://docs.github.com/en/authentication/connectingto-github-with-ssh"),
            "row {row}"
        );
    }
}
