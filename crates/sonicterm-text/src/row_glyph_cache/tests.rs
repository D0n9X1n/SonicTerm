
use super::*;
#[test]
fn row_hash_cells_accepts_owned_cells() {
    let cells = vec![
        Cell::plain('a', Color::Default, Color::Default, Default::default()),
        Cell::plain('b', Color::Default, Color::Default, Default::default()),
    ];
    let hash = row_hash_cells(0, 0, cells, 1, 10.0, 20.0, 1.0, None);
    assert_ne!(hash, 0);
}
