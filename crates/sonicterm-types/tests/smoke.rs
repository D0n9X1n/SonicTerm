use sonicterm_types::{Cell, CellFlags, Color};

#[test]
fn exports_core_cell_contracts() {
    let cell = Cell::plain('A', Color::Rgb(1, 2, 3), Color::Default, CellFlags::BOLD);
    assert_eq!(cell.ch, 'A');
    assert_eq!(cell.fg, Color::Rgb(1, 2, 3));
    assert!(cell.flags.contains(CellFlags::BOLD));
}
