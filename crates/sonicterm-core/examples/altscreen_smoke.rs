//! Smoke test for DEC ?1049 alt-screen handling.

use sonicterm_grid::grid::{CellFlags, Grid};
use sonicterm_vt::vt::Parser;

fn row_str(parser: &Parser, r: u16) -> String {
    parser
        .grid()
        .row(r)
        .iter()
        .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn main() {
    let mut p = Parser::new(Grid::new(40, 6));
    p.advance(b"primary line one\r\n");
    p.advance(b"primary line two");
    assert_eq!(row_str(&p, 0), "primary line one");
    assert_eq!(row_str(&p, 1), "primary line two");

    p.advance(b"\x1b[?1049h");
    assert!(p.grid().is_alt(), "expected alt-screen active after ?1049h");
    for r in 0..p.grid().rows {
        assert_eq!(row_str(&p, r), "");
    }
    p.advance(b"\x1b[H");
    p.advance(b"ALT CONTENT HERE");
    assert_eq!(row_str(&p, 0), "ALT CONTENT HERE");

    p.advance(b"\x1b[?1049l");
    assert!(!p.grid().is_alt(), "expected primary restored after ?1049l");
    assert_eq!(row_str(&p, 0), "primary line one");
    assert_eq!(row_str(&p, 1), "primary line two");

    println!("[altscreen-smoke] OK");
}
