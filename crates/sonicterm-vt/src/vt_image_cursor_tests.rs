//! Pins where the cursor goes after a Sixel or iTerm2 image: the parser ends its prefix after the
//! image, a Sixel move keeps the image's column and ends on its last cell row, an iTerm2 move ends
//! right of its bottom-right cell and stops in the last column at the right edge, a move at the bottom margin scrolls,
//! and without a cell size the cursor stays still.

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

/// The parser ends its prefix after a Sixel or iTerm2 image and before the text after it, so the
/// host moves the cursor first; the text is parsed only when the host resumes.
#[test]
fn the_prefix_ends_after_a_sixel_or_iterm2_image() {
    for stream in ["ab\x1bPq#1~~\x1b\\X", "ab\x1b]1337;File=inline=1:AAAA\x07X"] {
        let (mut parser, _pool) = new_parser(40, 10);
        let (consumed, events, _replies) = parser.advance_with_replies(stream.as_bytes());
        let text_at = stream.find('X').expect("text");
        assert_eq!(only_media(events).row, 0, "{stream:?} queues its image");
        assert!(consumed <= text_at, "{stream:?} stops before the text");
        assert_eq!(row_text(&parser, 0), "ab", "{stream:?} has not printed the text yet");
    }
}

/// On 10 x 20 px cells a 35 x 50 px Sixel at column 2 covers 4 x 3 cells; the cursor keeps
/// column 2 and ends on row 2, so the next line's text starts below the image.
#[test]
fn a_sixel_move_ends_on_the_image_last_row_in_its_column() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.set_cell_pixel_size(10.0, 20.0);
    parser.advance(b"ab");
    parser.move_cursor_past_sixel_image((35, 50));
    assert_eq!(cursor(&parser), (2, 2));
    parser.advance(b"\r\nX");
    assert_eq!(row_text(&parser, 3), "X", "text after the image lands below it");
}

/// The same image over iTerm2 puts the cursor right of its bottom-right cell: 4 right, 2 down.
#[test]
fn an_iterm2_move_ends_right_of_the_image_bottom_right_cell() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.set_cell_pixel_size(10.0, 20.0);
    parser.advance(b"ab");
    parser.move_cursor_past_iterm2_image((35, 50));
    assert_eq!(cursor(&parser), (2, 6));
}

/// An iTerm2 image reaching the right edge leaves the cursor in the last column of its last row
/// rather than wrapping, as iTerm2 and WezTerm do.
#[test]
fn an_iterm2_move_past_the_right_edge_stops_in_the_last_column() {
    let (mut parser, _pool) = new_parser(10, 10);
    parser.set_cell_pixel_size(10.0, 20.0);
    parser.advance(b"abcd");
    parser.move_cursor_past_iterm2_image((60, 40));
    assert_eq!(cursor(&parser), (1, 9), "col 4 + 6 = 10 stops at column 9 on row 0 + 1");
}

/// A Sixel near the bottom scrolls the screen so the cursor ends on the image's last row, and the
/// image, anchored before the scroll, moves up with it.
#[test]
fn a_sixel_at_the_bottom_scrolls() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.set_cell_pixel_size(10.0, 20.0);
    let media = only_media(parser.advance(b"\x1b[9;1H\x1bPq#1~~\x1b\\"));
    parser.move_cursor_past_sixel_image((20, 60));
    assert_eq!(parser.grid().lines_scrolled() - media.scroll_mark, 1, "rows 8..=10 scroll once");
    assert_eq!(cursor(&parser), (9, 0));
}

/// Without a cell size, or for an image with no pixels, neither move changes the cursor.
#[test]
fn no_cell_size_or_no_pixels_leaves_the_cursor_still() {
    let (mut parser, _pool) = new_parser(40, 10);
    parser.advance(b"ab");
    parser.move_cursor_past_sixel_image((35, 50));
    parser.move_cursor_past_iterm2_image((35, 50));
    assert_eq!(cursor(&parser), (0, 2), "no cell size");
    parser.set_cell_pixel_size(10.0, 20.0);
    parser.move_cursor_past_sixel_image((0, 50));
    parser.move_cursor_past_iterm2_image((35, 0));
    assert_eq!(cursor(&parser), (0, 2), "no pixels");
}
