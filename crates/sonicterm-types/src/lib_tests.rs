//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{Cell, CellFlags, Color};

#[test]
fn exports_core_cell_contracts() {
    let cell = Cell::plain('A', Color::Rgb(1, 2, 3), Color::Default, CellFlags::BOLD);
    assert_eq!(cell.ch, 'A');
    assert_eq!(cell.fg, Color::Rgb(1, 2, 3));
    assert!(cell.flags.contains(CellFlags::BOLD));
}
