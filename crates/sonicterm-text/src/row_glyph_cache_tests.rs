use super::*;
#[test]
fn row_hash_cells_accepts_owned_cells() {
    let cells = vec![
        Cell::plain('a', Color::Default, Color::Default, Default::default()),
        Cell::plain('b', Color::Default, Color::Default, Default::default()),
    ];
    let hash = row_hash_cells(0, 0, cells, 1, 10.0, 20.0, 1.0, 0.0, 0.0, 800.0, 600.0, None);
    assert_ne!(hash, 0);
}

/// A cached absolute row must not replay coordinates from a different viewport slot.
#[test]
fn row_hash_distinguishes_screen_position_for_the_same_absolute_row() {
    let cells = vec![Cell::plain('V', Color::Default, Color::Default, Default::default())];

    let top_slot = row_hash_cells(10, 0, &cells, 1, 10.0, 20.0, 1.0, 0.0, 0.0, 800.0, 600.0, None);
    let next_slot = row_hash_cells(9, 1, &cells, 1, 10.0, 20.0, 1.0, 0.0, 0.0, 800.0, 600.0, None);

    assert_ne!(
        top_slot, next_slot,
        "one absolute row at two viewport Y positions carries different glyph geometry"
    );
}

/// Cached NDC cannot survive a pane move even when content and cell metrics match.
#[test]
fn row_hash_distinguishes_pane_origin() {
    let cells = [Cell::plain('x', Color::Default, Color::Default, Default::default())];
    let before = row_hash_cells(0, 0, &cells, 1, 10.0, 20.0, 1.0, 12.0, 8.0, 800.0, 600.0, None);
    let moved = row_hash_cells(0, 0, &cells, 1, 10.0, 20.0, 1.0, 13.0, 9.0, 800.0, 600.0, None);

    assert_ne!(before, moved, "pane movement changes every cached glyph rectangle");
}

/// Cached NDC cannot survive projection against a different surface extent.
#[test]
fn row_hash_distinguishes_surface_extent() {
    let cells = [Cell::plain('x', Color::Default, Color::Default, Default::default())];
    let before = row_hash_cells(0, 0, &cells, 1, 10.0, 20.0, 1.0, 12.0, 8.0, 800.0, 600.0, None);
    let resized = row_hash_cells(0, 0, &cells, 1, 10.0, 20.0, 1.0, 12.0, 8.0, 801.0, 601.0, None);

    assert_ne!(before, resized, "surface projection changes cached NDC coordinates");
}

#[test]
fn resizing_per_pane_to_differing_row_counts_thrashes_the_cache() {
    // Pins the BUG the renderer fix avoids: calling `resize(pane.rows)` once
    // per pane with different row counts changes the cap each call and clears
    // the whole cache, so a peer pane's entries are lost every frame.
    let mut c = RowGlyphCache::new();
    // Two unequal panes: 10 and 30 rows.
    c.resize(10);
    c.insert(0, 0, 1, 0, CachedRow::default()); // pane 0 caches a row
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
    c.insert(0, 0, 1, 0, CachedRow::default()); // pane 0
    c.insert(1, 0, 2, 0, CachedRow::default()); // pane 1
    c.insert(0, 1, 3, 0, CachedRow::default()); // pane 0, another row
    assert_eq!(c.len(), 3);
    // A second frame re-sizes to the SAME total → no-op, entries survive.
    c.resize(total);
    assert_eq!(c.len(), 3, "stable cap must not clear the cache between frames");
    // Lookups still hit.
    assert!(c.get(0, 0, 1, 0).is_some());
    assert!(c.get(1, 0, 2, 0).is_some());
}

#[test]
fn atlas_identity_change_invalidates_cached_rows() {
    let mut c = RowGlyphCache::new();
    c.resize(1);
    c.insert(0, 0, 1, 7, CachedRow::default());

    assert!(c.get(0, 0, 1, 7).is_some(), "matching atlas identity should reuse the row");
    assert!(
        c.get(0, 0, 1, 8).is_none(),
        "an atlas identity change must reject UVs cached against replaced rectangles"
    );
}

fn retained_row(glyphs: usize, underlines: usize, tofu: usize, missing: usize) -> CachedRow {
    CachedRow {
        glyphs: vec![
            crate::GlyphInstance {
                rect: [0.0; 4],
                uv: [0.0; 4],
                color: [0.0; 4],
                flags: [0.0; 4],
            };
            glyphs
        ],
        underlines: vec![
            UnderlineRun {
                start_col: 0,
                end_col: 1,
                style: UnderlineStyle::Single,
                color: Color::Default,
            };
            underlines
        ],
        tofu: vec![(0.0, 0.0, 1.0, 1.0, [0; 4]); tofu],
        missing_chars: vec!['x'; missing],
    }
}

fn payload_bytes(row: &CachedRow) -> usize {
    row.glyphs.capacity() * std::mem::size_of::<crate::GlyphInstance>()
        + row.underlines.capacity() * std::mem::size_of::<UnderlineRun>()
        + row.tofu.capacity() * std::mem::size_of::<(f32, f32, f32, f32, TofuColor)>()
        + row.missing_chars.capacity() * std::mem::size_of::<char>()
}

fn cache_payload_bytes(cache: &RowGlyphCache) -> usize {
    cache.entries.values().map(|entry| payload_bytes(&entry.row)).sum()
}

/// Retention includes table allocation and every nested row-vector capacity.
#[test]
fn retained_amount_counts_glyph_cache_table_and_payloads() {
    let mut cache = RowGlyphCache::new();
    cache.resize(4);
    cache.insert(7, 11, 13, 17, retained_row(3, 5, 7, 9));

    let entry = cache.entries.values().next().expect("one cached row");
    let payload = entry.row.glyphs.capacity() * std::mem::size_of::<crate::GlyphInstance>()
        + entry.row.underlines.capacity() * std::mem::size_of::<UnderlineRun>()
        + entry.row.tofu.capacity() * std::mem::size_of::<(f32, f32, f32, f32, TofuColor)>()
        + entry.row.missing_chars.capacity() * std::mem::size_of::<char>();
    let expected =
        retained_hash_table_bytes::<(PaneId, u64), CachedRowEntry>(cache.entries.capacity())
            + payload;

    assert_eq!(cache.retained_amount(), ResourceAmount { bytes: expected, items: 1 });
}

/// Removing one pane frees its payload while preserving peer cache hits.
#[test]
fn pane_invalidation_reclaims_glyph_payload_and_preserves_peer_rows() {
    let mut cache = RowGlyphCache::new();
    cache.resize(8);
    cache.insert(1, 0, 10, 3, retained_row(64, 8, 4, 2));
    cache.insert(2, 0, 20, 3, retained_row(2, 1, 0, 0));
    let before = cache.retained_amount();
    let payload_before = cache_payload_bytes(&cache);
    let table_before = cache.entries.capacity();

    cache.invalidate_pane(1);

    let after = cache.retained_amount();
    let payload_after = cache_payload_bytes(&cache);
    assert!(after.items < before.items, "the removed pane's row count must disappear");
    assert!(
        payload_after < payload_before,
        "the removed pane's nested vector capacities must be released"
    );
    assert!(
        cache.entries.capacity() <= table_before,
        "table capacity must not grow while shrinking"
    );
    assert!(cache.get(2, 0, 20, 3).is_some(), "peer pane row must survive invalidation");
}

/// Repeated replacement stays inside the table-plus-payload high-water envelope.
#[test]
fn glyph_cache_churn_stays_inside_derived_high_water_envelope() {
    const ROWS: u16 = 8;
    const CAP: usize = ROWS as usize * CACHE_HEADROOM_FACTOR;
    const GENERATIONS: u64 = 12;
    let mut cache = RowGlyphCache::new();
    cache.resize(ROWS);
    let payload = payload_bytes(&retained_row(128, 16, 8, 4));
    let envelope = retained_hash_table_bytes::<(PaneId, u64), CachedRowEntry>(CAP)
        .saturating_add(CAP.saturating_mul(payload));
    let mut peak = 0usize;

    for generation in 0..GENERATIONS {
        for row in 0..CAP as u64 {
            cache.insert(
                generation,
                generation * CAP as u64 + row,
                row,
                1,
                retained_row(128, 16, 8, 4),
            );
        }
        let held = cache.retained_amount().bytes;
        peak = peak.max(held);
        assert!(held <= envelope, "generation {generation} held {held} above {envelope}");
    }

    assert!(peak > envelope / 2, "fixture never approached its derived envelope");
}

/// Bytes the viewport list's allocation holds, as `retained_amount` reports it.
fn visible_list_bytes(cache: &RowGlyphCache) -> usize {
    cache.visible.capacity() * std::mem::size_of::<(PaneId, std::ops::Range<u64>)>()
}

/// Dropping one dirty row is one keyed removal: it examines that entry alone, however
/// many other rows the cache holds, and leaves every other row cached.
#[test]
fn invalidating_one_row_does_not_visit_the_other_rows() {
    const ROW_COUNT: u64 = 2_000;
    let mut cache = RowGlyphCache::new();
    cache.resize(ROW_COUNT as u16);
    for row in 0..ROW_COUNT {
        cache.insert(4, row, row, 1, CachedRow::default());
    }
    cache.invalidate_visits = 0;

    cache.invalidate_row_abs(4, 777);

    assert_eq!(cache.invalidate_visits, 1, "one dirty row must examine one entry");
    assert_eq!(cache.len(), ROW_COUNT as usize - 1);
    assert!(cache.get(4, 777, 777, 1).is_none(), "the dirty row is gone");
    assert!(cache.get(4, 778, 778, 1).is_some(), "its neighbour is still cached");
}

/// Re-shaping a row that is already cached replaces its entry; it is not a new admission,
/// so a full cache keeps every peer row.
#[test]
fn replacing_an_existing_row_at_capacity_evicts_nothing() {
    let mut cache = RowGlyphCache::new();
    cache.resize(1);
    for row in 0..4 {
        cache.insert(1, row, 10, 0, CachedRow::default());
    }
    assert_eq!(cache.len(), 4, "the cache is at its capacity of four");

    cache.insert(1, 2, 11, 0, CachedRow::default());

    assert_eq!(cache.len(), 4, "a replacement must not clear the peers");
    assert!(cache.get(1, 2, 11, 0).is_some(), "the row now holds its new content");
    assert!(cache.get(1, 2, 10, 0).is_none(), "the old content no longer hits");
    for row in [0, 1, 3] {
        assert!(cache.get(1, row, 10, 0).is_some(), "peer row {row} survived");
    }
}

/// A new row admitted at capacity drops only rows outside this frame's viewports. Ranges
/// are per pane: pane 2 shows rows 100..102 of a scrolled-back history, pane 3 shows rows
/// 0..2, so pane 2's row 0 is not protected by pane 3's range, and pane 9 (not drawn this
/// frame) loses every row.
#[test]
fn admission_at_capacity_keeps_every_panes_visible_rows() {
    let mut cache = RowGlyphCache::new();
    cache.resize(2);
    cache.begin_frame(&[(2, 100..102), (3, 0..2)]);
    let seeded = [(2, 100), (2, 101), (3, 0), (3, 1), (2, 0), (9, 100), (9, 0), (3, 100)];
    for (pane, row) in seeded {
        cache.insert(pane, row, 1, 0, CachedRow::default());
    }
    assert_eq!(cache.len(), 8, "the cache is at its capacity of eight");

    cache.insert(3, 5, 1, 0, CachedRow::default());

    for (pane, row) in [(2, 100), (2, 101), (3, 0), (3, 1), (3, 5)] {
        assert!(cache.get(pane, row, 1, 0).is_some(), "visible row {pane}/{row} must survive");
    }
    for (pane, row) in [(2, 0), (9, 100), (9, 0), (3, 100)] {
        assert!(cache.get(pane, row, 1, 0).is_none(), "off-viewport row {pane}/{row} evicted");
    }
    assert_eq!(cache.len(), 5);
}

/// When every cached row is visible, eviction frees nothing and admission clears the
/// table, as it did before viewports were known.
#[test]
fn admission_clears_the_table_when_every_cached_row_is_visible() {
    let mut cache = RowGlyphCache::new();
    cache.resize(1);
    cache.begin_frame(&[(1, 0..4)]);
    for row in 0..4 {
        cache.insert(1, row, 1, 0, CachedRow::default());
    }

    cache.insert(1, 9, 1, 0, CachedRow::default());

    assert_eq!(cache.len(), 1, "nothing was evictable, so the table was cleared");
    assert!(cache.get(1, 9, 1, 0).is_some());
}

/// Pin: a stored row hits only when the content hash matches; a different hash is a miss.
#[test]
fn a_hash_mismatch_is_a_miss() {
    let mut cache = RowGlyphCache::new();
    cache.resize(1);
    cache.insert(1, 3, 42, 7, CachedRow::default());

    assert!(cache.get(1, 3, 42, 7).is_some());
    assert!(cache.get(1, 3, 43, 7).is_none(), "changed content must re-shape");
    assert!(cache.get(2, 3, 42, 7).is_none(), "another pane's row is a different key");
}

/// The viewport list is reported in `retained_amount`, grows with the pane count, shrinks
/// once the pane set contracts, and loses a removed pane's entry.
#[test]
fn viewport_list_is_retained_bounded_and_follows_the_pane_set() {
    let mut cache = RowGlyphCache::new();
    cache.resize(8);
    let many: Vec<(PaneId, std::ops::Range<u64>)> = (0..40).map(|pane| (pane, 0..8)).collect();
    cache.begin_frame(&many);
    let grown = cache.visible.capacity();
    assert!(grown >= 40, "the list holds every drawn pane");
    let table = retained_hash_table_bytes::<(PaneId, u64), CachedRowEntry>(0);
    assert_eq!(
        cache.retained_amount().bytes,
        table + visible_list_bytes(&cache),
        "the list's capacity is retained storage"
    );

    cache.begin_frame(&[(1, 0..8), (2, 0..8)]);
    assert!(cache.visible.capacity() < grown, "a contracted pane set releases the list");
    assert!(cache.visible.capacity() >= 2);

    cache.invalidate_pane(1);
    assert_eq!(cache.visible, vec![(2, 0..8)], "a removed pane leaves the viewport list");
}

/// Repeated partial eviction at capacity never grows the table: evicted slots are reset to
/// empty rather than left as deleted markers that use up the table's spare room and force
/// it to double while it holds no more rows than its cap.
#[test]
fn repeated_viewport_eviction_keeps_the_table_allocation() {
    const ROWS: u16 = 8;
    const CAP: u64 = ROWS as u64 * CACHE_HEADROOM_FACTOR as u64;
    let mut cache = RowGlyphCache::new();
    cache.resize(ROWS);
    cache.begin_frame(&[(0, 0..u64::from(ROWS))]);
    for row in 0..CAP {
        cache.insert(0, row, 1, 0, CachedRow::default());
    }
    let filled = cache.entries.capacity();

    for generation in 1..400_u64 {
        for row in 0..CAP {
            cache.insert(0, generation * 1_000 + row, 1, 0, CachedRow::default());
        }
        assert!(
            cache.entries.capacity() <= filled,
            "generation {generation} grew the table from {filled} to {}",
            cache.entries.capacity()
        );
    }
}
