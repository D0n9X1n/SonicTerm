use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

fn row_text(parser: &Parser, row: u16) -> String {
    parser.grid().row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn carriage_return_progress_updates_same_line_without_scrollback() {
    let mut parser = parser(24, 3);

    parser.advance(b"Downloading 10%\rDownloading 20%\r\x1b[KDownloading 100%");

    assert_eq!(row_text(&parser, 0), "Downloading 100%        ");
    assert_eq!(row_text(&parser, 1), "                        ");
    assert_eq!(parser.grid().scrollback_len(), 0);
}

#[test]
fn decawm_off_clips_long_progress_lines_instead_of_wrapping() {
    let mut parser = parser(12, 3);

    parser.advance(
        b"\x1b[?7lBottle node ############ Downloading\r\x1b[KBottle just #### Downloading",
    );

    assert_eq!(row_text(&parser, 0), "Bottle justg");
    assert_eq!(row_text(&parser, 1), "            ");
    assert_eq!(parser.grid().scrollback_len(), 0);
    assert!(!parser.grid().autowrap());
}

#[test]
fn decawm_on_restores_normal_wrapping() {
    let mut parser = parser(5, 3);

    parser.advance(b"\x1b[?7labcdef\x1b[?7hZ");

    assert_eq!(row_text(&parser, 0), "abcdZ");
    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 5);
    assert!(parser.grid().pending_wrap());
    assert!(parser.grid().autowrap());
}

#[test]
fn exact_width_write_sets_pending_wrap_without_immediate_scroll() {
    let mut parser = parser(4, 2);

    parser.advance(b"abcd");

    assert_eq!(row_text(&parser, 0), "abcd");
    assert_eq!(row_text(&parser, 1), "    ");
    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 4);
    assert!(parser.grid().pending_wrap());
    assert_eq!(parser.grid().scrollback_len(), 0);
}

#[test]
fn next_printable_after_pending_wrap_wraps_once() {
    let mut parser = parser(4, 2);

    parser.advance(b"abcdZ");

    assert_eq!(row_text(&parser, 0), "abcd");
    assert_eq!(row_text(&parser, 1), "Z   ");
    assert_eq!(parser.grid().cursor.row, 1);
    assert_eq!(parser.grid().cursor.col, 1);
    assert!(!parser.grid().pending_wrap());
}

#[test]
fn carriage_return_clears_pending_wrap() {
    let mut parser = parser(4, 2);

    parser.advance(b"abcd\rxy");

    assert_eq!(row_text(&parser, 0), "xycd");
    assert_eq!(row_text(&parser, 1), "    ");
    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 2);
    assert!(!parser.grid().pending_wrap());
}

#[test]
fn erase_line_clears_pending_wrap() {
    let mut parser = parser(4, 2);

    parser.advance(b"abcd\x1b[KZ");

    assert_eq!(row_text(&parser, 0), "abcd");
    assert_eq!(row_text(&parser, 1), "Z   ");
    assert!(!parser.grid().pending_wrap());
}

#[test]
fn pending_wrap_scrolls_inside_scroll_region() {
    let mut parser = parser(4, 4);

    parser.advance(b"\x1b[1;1Htop \x1b[2;1H1111\x1b[3;1H2222\x1b[4;1Hlast");
    parser.advance(b"\x1b[2;3r\x1b[3;1HABCDZ");

    assert_eq!(row_text(&parser, 0), "top ");
    assert_eq!(row_text(&parser, 1), "ABCD");
    assert_eq!(row_text(&parser, 2), "Z   ");
    assert_eq!(row_text(&parser, 3), "last");
}

#[test]
fn wide_glyph_at_last_column_wraps_before_printing() {
    let mut parser = parser(4, 2);

    parser.advance("abc中".as_bytes());

    assert_eq!(row_text(&parser, 0), "abc ");
    assert_eq!(parser.grid().row(1)[0].ch, '中');
    assert!(parser.grid().row(1)[0].flags.contains(sonicterm_grid::grid::CellFlags::WIDE));
    assert!(parser.grid().row(1)[1].flags.contains(sonicterm_grid::grid::CellFlags::WIDE_CONT));
}

#[test]
fn combining_mark_after_pending_wrap_stays_on_previous_cell() {
    let mut parser = parser(4, 2);

    parser.advance("abcd\u{0301}".as_bytes());

    assert_eq!(row_text(&parser, 0), "abcd");
    assert_eq!(row_text(&parser, 1), "    ");
    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 4);
    assert!(parser.grid().pending_wrap());
}

#[test]
fn alternate_screen_preserves_autowrap_mode() {
    let mut parser = parser(5, 2);

    parser.advance(b"\x1b[?7l\x1b[?1049habcdef\x1b[?1049l");

    assert!(!parser.grid().autowrap());
}
