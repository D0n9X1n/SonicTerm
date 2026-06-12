
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

#[test]
fn resizing_per_pane_to_differing_row_counts_thrashes_the_cache() {
    // Pins the BUG the renderer fix avoids: calling `resize(pane.rows)` once
    // per pane with different row counts changes the cap each call and clears
    // the whole cache, so a peer pane's entries are lost every frame.
    let mut c = RowGlyphCache::new();
    // Two unequal panes: 10 and 30 rows.
    c.resize(10);
    c.insert(0, 0, 1, CachedRow::default()); // pane 0 caches a row
    assert_eq!(c.len(), 1);
    c.resize(30); // peer pane resized with ITS row count → cap changes → clear
    assert!(c.is_empty(), "per-pane resize wiped pane 0's cached row");
}

#[test]
fn sizing_once_to_total_rows_keeps_both_panes_cached() {
    // The fix: size ONCE to the sum of all panes' visible rows, then walk the
    // panes. The cap is stable across the frame, so unchanged rows in either
    // pane stay cached and don't re-shape.
    let mut c = RowGlyphCache::new();
    let total = 10u16 + 30u16; // sum of both panes
    c.resize(total);
    // Both panes cache rows in the same frame; nothing is cleared.
    c.insert(0, 0, 1, CachedRow::default()); // pane 0
    c.insert(1, 0, 2, CachedRow::default()); // pane 1
    c.insert(0, 1, 3, CachedRow::default()); // pane 0, another row
    assert_eq!(c.len(), 3);
    // A second frame re-sizes to the SAME total → no-op, entries survive.
    c.resize(total);
    assert_eq!(c.len(), 3, "stable cap must not clear the cache between frames");
    // Lookups still hit.
    assert!(c.get(0, 0, 1).is_some());
    assert!(c.get(1, 0, 2).is_some());
}
