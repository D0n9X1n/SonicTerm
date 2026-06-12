use sonicterm_grid::grid::{Color, Grid};
use sonicterm_vt::vt::Parser;

fn parser(cols: u16, rows: u16) -> Parser {
    Parser::new(Grid::new(cols, rows))
}

fn row_text(parser: &Parser, row: u16) -> String {
    parser.grid().row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn cursor_movement_and_absolute_positioning() {
    let mut parser = parser(8, 4);

    parser.advance(b"abcd\x1b[2;3HXY\x1b[AZ\x1b[2C!");

    assert_eq!(row_text(&parser, 0), "abcdZ  !");
    assert_eq!(row_text(&parser, 1), "  XY    ");
}

#[test]
fn erase_line_modes_use_current_position() {
    let mut parser = parser(8, 3);

    parser.advance(b"abcdefgh\x1b[1;4H\x1b[K");
    assert_eq!(row_text(&parser, 0), "abc     ");

    parser.advance(b"\x1b[1;1Habcdefgh\x1b[1;4H\x1b[1K");
    assert_eq!(row_text(&parser, 0), "    efgh");

    parser.advance(b"\x1b[1;1Habcdefgh\x1b[1;4H\x1b[2K");
    assert_eq!(row_text(&parser, 0), "        ");
}

#[test]
fn erase_screen_modes_preserve_expected_sides() {
    let mut parser = parser(5, 3);

    parser.advance(b"\x1b[1;1Haaaaa\x1b[2;1Hbbbbb\x1b[3;1Hccccc\x1b[2;3H\x1b[J");
    assert_eq!(row_text(&parser, 0), "aaaaa");
    assert_eq!(row_text(&parser, 1), "bb   ");
    assert_eq!(row_text(&parser, 2), "     ");

    parser.advance(b"\x1b[1;1Haaaaa\x1b[2;1Hbbbbb\x1b[3;1Hccccc\x1b[2;3H\x1b[1J");
    assert_eq!(row_text(&parser, 0), "     ");
    assert_eq!(row_text(&parser, 1), "   bb");
    assert_eq!(row_text(&parser, 2), "ccccc");
}

#[test]
fn insert_delete_and_erase_cells_use_bce_fill() {
    let mut parser = parser(8, 2);

    parser.advance(b"abcdefgh\x1b[1;4H\x1b[2P");
    assert_eq!(row_text(&parser, 0), "abcfgh  ");

    parser.advance(b"\x1b[1;1Habcdefgh\x1b[1;4H\x1b[2@");
    assert_eq!(row_text(&parser, 0), "abc    f");

    parser.advance(b"\x1b[1;1Habcdefgh\x1b[1;4H\x1b[3X");
    assert_eq!(row_text(&parser, 0), "abc   gh");
}

#[test]
fn sgr_colors_apply_to_printed_cells() {
    let mut parser = parser(4, 1);

    parser.advance(b"\x1b[31;44mR\x1b[0mN");

    assert_eq!(parser.grid().row(0)[0].fg, Color::Indexed(1));
    assert_eq!(parser.grid().row(0)[0].bg, Color::Indexed(4));
    assert_eq!(parser.grid().row(0)[1].fg, Color::Default);
    assert_eq!(parser.grid().row(0)[1].bg, Color::Default);
}

#[test]
fn scroll_up_and_down_respect_region() {
    let mut parser = parser(4, 4);

    parser.advance(b"\x1b[1;1H1111\x1b[2;1H2222\x1b[3;1H3333\x1b[4;1H4444");
    parser.advance(b"\x1b[2;3r\x1b[S");
    assert_eq!(row_text(&parser, 0), "1111");
    assert_eq!(row_text(&parser, 1), "3333");
    assert_eq!(row_text(&parser, 2), "    ");
    assert_eq!(row_text(&parser, 3), "4444");

    parser.advance(b"\x1b[T");
    assert_eq!(row_text(&parser, 1), "    ");
    assert_eq!(row_text(&parser, 2), "3333");
}
