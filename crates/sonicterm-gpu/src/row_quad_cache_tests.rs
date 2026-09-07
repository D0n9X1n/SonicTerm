use super::*;
use sonicterm_render_model::boundary::grid::line::{Cluster, Line};

#[test]
fn row_quad_hash_cells_accepts_cluster_storage() {
    let line = Line::from_clusters(vec![
        Cluster {
            cell: Cell::plain('a', Default::default(), Default::default(), Default::default()),
            count: 2,
        },
        Cluster {
            cell: Cell::plain('b', Default::default(), Default::default(), Default::default()),
            count: 1,
        },
    ]);
    let hash = row_quad_hash_cells(0, 0, line.iter(), 1, 10.0, 20.0, 0.0, 0.0, 100.0, 20.0, None);
    assert_ne!(hash, 0);
}

/// Projected background rows cannot reuse screen coordinates from a different viewport slot.
#[test]
fn projected_row_hash_distinguishes_viewport_slots_for_the_same_absolute_row() {
    let row = [Cell::plain(
        'x',
        Default::default(),
        sonicterm_types::Color::Rgb(120, 40, 80),
        Default::default(),
    )];
    let hash =
        |top, slot| row_quad_hash(top, slot, &row, 1, 10.0, 20.0, 8.0, 24.0, 80.0, 60.0, None);

    assert_eq!(hash(10, 0), hash(10, 0), "unchanged placement must remain cacheable");
    assert_ne!(hash(10, 0), hash(9, 1), "absolute row 10 moved down one screen row");
}

/// Absolute invalidation removes every projection of one pane's row without evicting peer rows.
#[test]
fn absolute_invalidation_preserves_peer_panes_and_other_rows() {
    let mut cache = LineQuadCache::new();
    cache.resize(3);
    let row = [Cell::default()];
    let hash =
        |top, slot| row_quad_hash(top, slot, &row, 1, 10.0, 20.0, 0.0, 0.0, 80.0, 60.0, None);
    let first = hash(10, 0);
    let shifted = hash(9, 1);
    cache.insert(1, 10, first, CachedRowQuads::default());
    cache.insert(1, 10, shifted, CachedRowQuads::default());
    cache.insert(1, 11, hash(10, 1), CachedRowQuads::default());
    cache.insert(2, 10, first, CachedRowQuads::default());
    let cap = cache.cap;

    cache.invalidate_row_abs(1, 10);

    assert!(cache.get(1, 10, first).is_none());
    assert!(cache.get(1, 10, shifted).is_none());
    assert!(cache.get(1, 11, hash(10, 1)).is_some());
    assert!(cache.get(2, 10, first).is_some());
    assert_eq!(cache.cap, cap);
}

/// Moving an existing row at capacity replaces its validity stamp without clearing peer entries.
#[test]
fn projected_row_replacement_does_not_consume_capacity_or_flush_peers() {
    let mut cache = LineQuadCache::new();
    cache.resize(2);
    let cap = cache.cap;
    for row in 0..cap as u64 - 1 {
        cache.insert(2, row, row, CachedRowQuads::default());
    }
    cache.insert(1, 10, 100, CachedRowQuads::default());
    assert_eq!(cache.len(), cap);

    for hash in 101..121 {
        cache.insert(1, 10, hash, CachedRowQuads::default());
        assert_eq!(cache.len(), cap);
        assert!(cache.get(1, 10, hash).is_some());
        assert!(cache.get(1, 10, hash - 1).is_none());
        assert!(cache.get(2, 0, 0).is_some(), "peer entries survive projection changes");
        assert_eq!(cache.cap, cap);
    }
}

/// Retention includes the quad-cache table and each cached vector's capacity.
#[test]
fn retained_amount_counts_quad_cache_table_and_payloads() {
    let mut cache = LineQuadCache::new();
    cache.resize(4);
    cache.insert(7, 11, 13, CachedRowQuads { quads: vec![QuadInstance::default(); 9] });

    let payload = cache.entries.values().next().unwrap().1.quads.capacity()
        * std::mem::size_of::<QuadInstance>();
    let expected =
        retained_hash_table_bytes::<(PaneId, u64), (u64, CachedRowQuads)>(cache.entries.capacity())
            + payload;

    assert_eq!(cache.retained_amount(), ResourceAmount { bytes: expected, items: 1 });
}

/// Pane invalidation releases its quad payload without evicting peer rows.
#[test]
fn pane_invalidation_reclaims_quad_payload_and_preserves_peer_rows() {
    let mut cache = LineQuadCache::new();
    cache.resize(8);
    cache.insert(1, 0, 10, CachedRowQuads { quads: vec![QuadInstance::default(); 64] });
    cache.insert(2, 0, 20, CachedRowQuads { quads: vec![QuadInstance::default(); 2] });
    let before = cache.retained_amount();
    let payload_before: usize = cache
        .entries
        .values()
        .map(|(_, row)| row.quads.capacity() * std::mem::size_of::<QuadInstance>())
        .sum();
    let table_before = cache.entries.capacity();

    cache.invalidate_pane(1);

    let after = cache.retained_amount();
    let payload_after: usize = cache
        .entries
        .values()
        .map(|(_, row)| row.quads.capacity() * std::mem::size_of::<QuadInstance>())
        .sum();
    assert!(after.items < before.items, "the removed pane's row count must disappear");
    assert!(payload_after < payload_before, "the removed pane's quad vector must be released");
    assert!(
        cache.entries.capacity() <= table_before,
        "table capacity must not grow while shrinking"
    );
    assert!(cache.get(2, 0, 20).is_some(), "peer pane row must survive invalidation");
}

/// Repeated replacement stays inside the table-plus-payload high-water envelope.
#[test]
fn quad_cache_churn_stays_inside_derived_high_water_envelope() {
    const ROWS: u16 = 8;
    const CAP: usize = ROWS as usize * CACHE_HEADROOM_FACTOR;
    const GENERATIONS: u64 = 12;
    const QUADS: usize = 128;
    let mut cache = LineQuadCache::new();
    cache.resize(ROWS);
    let payload = QUADS * std::mem::size_of::<QuadInstance>();
    let envelope = retained_hash_table_bytes::<(PaneId, u64), (u64, CachedRowQuads)>(CAP)
        .saturating_add(CAP.saturating_mul(payload));
    let mut peak = 0usize;

    for generation in 0..GENERATIONS {
        for row in 0..CAP as u64 {
            cache.insert(
                generation,
                generation * CAP as u64 + row,
                row,
                CachedRowQuads { quads: vec![QuadInstance::default(); QUADS] },
            );
        }
        let held = cache.retained_amount().bytes;
        peak = peak.max(held);
        assert!(held <= envelope, "generation {generation} held {held} above {envelope}");
    }

    assert!(peak > envelope / 2, "fixture never approached its derived envelope");
}
