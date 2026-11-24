//! Pins `Parser::resize_terminal`: when the window loses rows under a live prompt on the primary
//! screen, rows above the cursor scroll into history so the cursor's row keeps its cells and
//! column and stays visible; nothing is blanked; and every other resize clips as `resize` does.

use super::*;

/// OSC 133 `body`, terminated by BEL.
fn mark(body: &str) -> String {
    format!("\x1b]133;{body}\x07")
}

/// A 20x10 parser with output `o0`..`o4` on rows 0-4, a prompt (A with `options`) on row 5 and
/// input on rows 5-8, the cursor at the end of row 8 (`ln3`).
fn prompt_with_input(options: &str) -> Parser {
    let mut parser = Parser::new(Grid::new(20, 10));
    let a_mark = if options.is_empty() { mark("A") } else { mark(&format!("A;{options}")) };
    let text =
        format!("o0\r\no1\r\no2\r\no3\r\no4\r\n{a_mark}$ {}ln0\r\nln1\r\nln2\r\nln3", mark("B"));
    parser.advance(text.as_bytes());
    parser
}

/// Every retained row's text, history then screen, trailing blanks trimmed.
fn all_rows(parser: &Parser) -> Vec<String> {
    let grid = parser.grid();
    let count = grid.scrollback_len() as u64 + u64::from(grid.rows);
    (0..count)
        .map(|abs| {
            let row = grid.row_at_abs(abs).expect("retained row");
            row.iter().map(|cell| cell.ch).collect::<String>().trim_end().to_owned()
        })
        .collect()
}

/// The cursor as `(row, col)`.
fn cursor(parser: &Parser) -> (u16, u16) {
    (parser.grid().cursor.row, parser.grid().cursor.col)
}

#[test]
fn cursor_mid_multiline_input_keeps_cursor_row_truncates_rows_below() {
    // The cursor on the second of four input rows: rows above it scroll into history so it sits
    // on the new bottom row with its cells and column; input rows below it are cut, as today.
    let mut parser = prompt_with_input("");
    parser.advance(b"\x1b[7;4H");
    assert_eq!(cursor(&parser), (6, 3));
    let outcome = parser.resize_terminal(20, 5);
    assert_eq!(outcome.scrolled_rows, 2);
    assert_eq!(cursor(&parser), (4, 3));
    assert_eq!(crate::vt::vt_test_support::row_text(&parser, 4).trim_end(), "ln1");
    let rows = all_rows(&parser);
    assert_eq!(&rows[..7], &["o0", "o1", "o2", "o3", "o4", "$ ln0", "ln1"]);
    assert!(!rows.iter().any(|row| row == "ln2" || row == "ln3"), "rows below are cut");
}

#[test]
fn height_smaller_than_prompt_keeps_cursor_row() {
    // A one-row window still shows the cursor's row; the prompt's start goes to history.
    let mut parser = prompt_with_input("redraw=last");
    let outcome = parser.resize_terminal(20, 1);
    assert_eq!(outcome.scrolled_rows, 8);
    assert_eq!(cursor(&parser), (0, 3));
    assert_eq!(crate::vt::vt_test_support::row_text(&parser, 0).trim_end(), "ln3");
    assert_eq!(parser.grid().prompts().last().and_then(|region| region.start_row), Some(5));
}

#[test]
fn no_cell_ever_blanked() {
    // Every row up to the cursor survives in order, in history or on screen, for each redraw
    // value that acts; nothing is erased to make room.
    for options in ["", "redraw=1", "redraw=last"] {
        let mut parser = prompt_with_input(options);
        let before = all_rows(&parser);
        parser.resize_terminal(20, 3);
        assert_eq!(all_rows(&parser), before[..9].to_vec(), "{options:?}");
    }
}

#[test]
fn rows_above_go_to_scrollback_in_order() {
    // Earlier history stays first, then the scrolled rows in screen order.
    let mut parser = Parser::new(Grid::new(20, 10));
    let output: String = (0..12).map(|line| format!("o{line}\r\n")).collect();
    parser.advance(format!("{output}{}$ ", mark("A")).as_bytes());
    assert_eq!((parser.grid().scrollback_len(), cursor(&parser)), (3, (9, 2)));
    let outcome = parser.resize_terminal(20, 4);
    assert_eq!(outcome.scrolled_rows, 6);
    let expected: Vec<String> =
        (0..12).map(|line| format!("o{line}")).chain(["$".to_owned()]).collect();
    assert_eq!(all_rows(&parser), expected);
    assert_eq!(cursor(&parser), (3, 2));
}

#[test]
fn no_scrollback_rows_above_discarded_cursor_row_kept() {
    // With scrollback off the scrolled rows are discarded, as the scroll path recycles them, and
    // the cursor's row is still kept; the semantic anchor moves up with its row.
    let mut parser = Parser::new(Grid::new(20, 10));
    parser.grid_mut().set_scrollback_limit(0);
    parser.advance(format!("o0\r\no1\r\no2\r\no3\r\no4\r\no5\r\n{}$ ab", mark("A")).as_bytes());
    assert_eq!(cursor(&parser), (6, 4));
    let outcome = parser.resize_terminal(20, 3);
    assert_eq!(outcome.scrolled_rows, 4);
    assert_eq!(parser.grid().scrollback_len(), 0);
    assert_eq!(all_rows(&parser), ["o4", "o5", "$ ab"]);
    assert_eq!(cursor(&parser), (2, 4));
    assert_eq!(parser.grid().semantic().anchor, Some(2));
}

#[test]
fn no_action_for_output_alt_redraw0_noop_or_clamped_resize() {
    // Output mode, `redraw=0`, a no-op or growing resize, the alternate screen, and a request the
    // bounds clamp to the current height all clip exactly as `resize` does: nothing scrolls.
    let mut output = prompt_with_input("");
    output.advance(mark("C").as_bytes());
    let cases = [
        ("output", output, 5),
        ("redraw=0", prompt_with_input("redraw=0"), 5),
        ("no-op", prompt_with_input(""), 10),
        ("growing", prompt_with_input(""), 12),
    ];
    for (label, mut parser, rows) in cases {
        let outcome = parser.resize_terminal(20, rows);
        assert_eq!(outcome.scrolled_rows, 0, "{label}");
        assert_eq!(parser.grid().scrollback_len(), 0, "{label}");
    }
    let mut alt = Parser::new(Grid::new(20, 10));
    alt.advance(format!("\x1b[?1049h\x1b[9;1H{}$ ", mark("A")).as_bytes());
    assert_eq!(alt.resize_terminal(20, 5).scrolled_rows, 0, "alternate screen");
    assert_eq!(cursor(&alt), (4, 2));
    let mut one_row = Parser::new(Grid::new(20, 1));
    one_row.advance(format!("{}$ ", mark("A")).as_bytes());
    assert_eq!(one_row.resize_terminal(20, 0).scrolled_rows, 0, "a request clamped to 1 row");
    assert_eq!(cursor(&one_row), (0, 2));
}

#[test]
fn general_grid_resize_unchanged() {
    // `Parser::resize` keeps today's clip-and-clamp under a live prompt: the bottom rows are cut
    // and the cursor clamps onto old output.
    let mut parser = prompt_with_input("");
    parser.resize(20, 5);
    assert_eq!(parser.grid().scrollback_len(), 0);
    assert_eq!(cursor(&parser), (4, 3));
    assert_eq!(crate::vt::vt_test_support::row_text(&parser, 4).trim_end(), "o4");
}
