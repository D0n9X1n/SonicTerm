//! Pins where the cursor goes after a kitty image: the parser ends its prefix after a command that
//! moves the cursor, and the host's move goes `c` right and `r - 1` down, wraps at the right edge,
//! scrolls at the bottom margin (storing what history holds), stays below the margins when it
//! started there, and sizes a missing side from the image's pixels and the cell size.

use super::*;
use crate::vt::vt_test_support::*;

/// A parser on a private pool and a `cols` x `rows` grid, so admission is this test's alone.
fn new_parser(cols: u16, rows: u16) -> (Parser, Arc<CaptureStagingPool>) {
    let pool = CaptureStagingPool::new();
    (Parser::new_with_staging_pool(Grid::new(cols, rows), None, pool.clone()), pool)
}

/// The cursor as `(row, col)`.
fn cursor(parser: &Parser) -> (u16, u16) {
    (parser.grid().cursor.row, parser.grid().cursor.col)
}

/// The text of visible row `row`, trailing blanks trimmed.
fn row_text(parser: &Parser, row: u16) -> String {
    let grid = parser.grid();
    let line = grid.row_at_abs(grid.scrollback_len() as u64 + u64::from(row)).expect("row");
    line.iter().map(|cell| cell.ch).collect::<String>().trim_end().to_owned()
}

/// The parser ends its prefix right after a command that moves the cursor, so the host moves the
/// cursor before the text after it is parsed; a store, `C=1` or a virtual placement does not.
#[test]
fn the_prefix_ends_after_a_command_that_moves_the_cursor() {
    let stream = |control: &str| format!("ab\x1b_G{control},f=100,q=2,c=8,r=3;AAAA\x1b\\X");
    let text_at = |control: &str| stream(control).find('X').expect("text");
    for control in ["a=T", "a=T,i=4"] {
        let (mut parser, _pool) = new_parser(40, 10);
        let (consumed, _events, _replies) = parser.advance_with_replies(stream(control).as_bytes());
        assert_eq!(consumed, text_at(control), "{control} stops before the text");
    }
    for control in ["a=t,i=4", "a=T,C=1", "a=T,U=1,i=3"] {
        let (mut parser, _pool) = new_parser(40, 10);
        let bytes = stream(control);
        let (consumed, _events, _replies) = parser.advance_with_replies(bytes.as_bytes());
        assert_eq!(consumed, bytes.len(), "{control} does not stop");
    }
}

/// Only a displayed, non-virtual placement without `C=1` moves the cursor.
#[test]
fn only_a_direct_placement_moves_the_cursor() {
    let moves = |control: &[u8]| KittyCommand::parse(control).expect("parses").moves_cursor();
    assert!(moves(b"a=T") && moves(b"a=p,i=5"));
    assert!(!moves(b"a=T,C=1") && !moves(b"a=T,U=1") && !moves(b"a=t") && !moves(b"a=q"));
}

/// The host's move after a placement at column 2 goes 8 right and 2 down, so the next line's text
/// starts on the row after the image instead of on it.
#[test]
fn the_move_puts_the_next_line_below_the_image() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.advance(b"ab");
    parser.move_cursor_past_kitty_image(8, 3, (64, 32));
    assert_eq!(cursor(&parser), (2, 10), "c=8 right, r-1=2 down");
    parser.advance(b"\r\nX");
    assert_eq!(row_text(&parser, 3), "X", "text after the image lands below it");
}

/// A move past the right edge wraps to the start of the next row.
#[test]
fn a_move_past_the_right_edge_wraps() {
    let (mut parser, _pool) = new_parser(10, 10);
    parser.advance(b"abcd");
    parser.move_cursor_past_kitty_image(6, 2, (64, 32));
    assert_eq!(cursor(&parser), (2, 0), "col 4 + 6 = 10 wraps to row 1 + 1");
}

/// An image placed near the bottom scrolls the screen so the cursor ends on its last row; the
/// event's scroll mark is taken before the move, so the host moves the image up by the rows
/// scrolled and it covers rows 7..=9 of a 10-row screen.
#[test]
fn a_placement_at_the_bottom_scrolls_and_the_image_follows() {
    let (mut parser, _pool) = new_parser(40, 10);
    let media = only_media(parser.advance(b"\x1b[9;1H\x1b_Ga=T,f=100,q=2,c=8,r=3;AAAA\x1b\\"));
    assert_eq!((media.row, media.scroll_mark), (8, 0), "anchored before the scroll");
    parser.move_cursor_past_kitty_image(8, 3, (64, 32));
    let scrolled = parser.grid().lines_scrolled();
    assert_eq!(scrolled, 1, "rows 8..=10 need one row more than the screen has");
    assert_eq!(i64::from(media.row) - (scrolled - media.scroll_mark) as i64, 7);
    assert_eq!(cursor(&parser), (9, 8), "the cursor ends on the image's last row");
}

/// An image taller than the screen scrolls by all of its rows, so the text after it still starts
/// below it, and an earlier image stays on its text's history row.
#[test]
fn an_image_taller_than_the_screen_keeps_text_and_earlier_images_in_place() {
    let (mut parser, _pool) = new_parser(40, 10);
    let earlier =
        only_media(parser.advance(b"\x1b[5;1Hmark\x1b_Ga=T,C=1,f=100,q=2,c=2,r=1;AAAA\x1b\\"));
    let tall = only_media(parser.advance(b"\x1b[H\x1b_Ga=T,f=100,q=2,c=2,r=30;AAAA\x1b\\"));
    parser.move_cursor_past_kitty_image(2, 30, (64, 32));
    parser.advance(b"\r\nX");
    let grid = parser.grid();
    let tall_bottom = i64::from(tall.row) - (grid.lines_scrolled() - tall.scroll_mark) as i64 + 29;
    assert_eq!(row_text(&parser, 9), "X");
    assert!(tall_bottom < 9, "the tall image ends on row {tall_bottom}, above the text");
    let image_abs = grid.scrollback_len() as i64 + i64::from(earlier.row)
        - (grid.lines_scrolled() - earlier.scroll_mark) as i64;
    let text_abs = (0..grid.scrollback_len() as u64 + u64::from(grid.rows))
        .find(|abs| {
            grid.row_at_abs(*abs).is_some_and(|line| {
                line.iter().map(|cell| cell.ch).collect::<String>().starts_with("mark")
            })
        })
        .expect("the text is still in history");
    assert_eq!(image_abs, text_abs as i64, "the earlier image stays on its text's row");
}

/// A placement whose cursor is below the DECSTBM margins does not scroll the region or jump into
/// it; the cursor moves down within the screen like a line feed outside the margins.
#[test]
fn a_placement_below_the_margins_keeps_the_cursor_below_them() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.advance(b"\x1b[2;5r\x1b[8;1H");
    parser.move_cursor_past_kitty_image(2, 1, (64, 32));
    assert_eq!(cursor(&parser), (7, 2), "the cursor stays on row 7, below the margins");
    assert_eq!(parser.grid().lines_scrolled(), 0);
}

/// Without `c` and `r`, the cells come from the image's pixel size and the cell size; one given
/// side keeps the aspect ratio; with no cell size reported, the cursor stays; a side is capped at
/// 65,535 cells.
#[test]
fn natural_size_uses_the_pixel_size_and_cell_size() {
    let (mut parser, _pool) = new_parser(40, 20);
    parser.move_cursor_past_kitty_image(0, 0, (100, 50));
    assert_eq!(cursor(&parser), (0, 0), "no cell size, no move");
    for (cols, rows, expected) in [(0, 0, (2, 10)), (0, 4, (3, 16)), (5, 0, (1, 5))] {
        let (mut parser, _pool) = new_parser(40, 20);
        parser.set_cell_pixel_size(10.0, 20.0);
        parser.move_cursor_past_kitty_image(cols, rows, (100, 50));
        // 100x50 px on 10x20 px cells is 10 x 3 cells; r=4 is 80 px tall, so 160 px or 16 cells
        // wide; c=5 is 50 px wide, so 25 px or 2 cells tall. The cursor ends on the last row.
        assert_eq!(cursor(&parser), expected, "c={cols} r={rows}");
    }
    assert_eq!(placement_cells(u32::MAX, 0, (1, 2048), Some((10.0, 20.0))), Some((65_535, 65_535)));
}

/// `C=1` survives the control text the host reads back, and other commands' text is unchanged.
#[test]
fn no_cursor_move_round_trips_through_the_control_text() {
    let command = KittyCommand::parse(b"a=T,C=1,i=2").expect("parses");
    assert!(command.no_cursor_move);
    assert!(command.control_text().ends_with(",C=1"));
    // The text always names q, so the reparsed command differs only in quiet_set; compare text.
    let reparsed = KittyCommand::parse(command.control_text().as_bytes()).expect("reparses");
    assert!(reparsed.no_cursor_move);
    assert_eq!(reparsed.control_text(), command.control_text());
    assert!(!KittyCommand::parse(b"a=T").expect("parses").control_text().contains("C="));
}
