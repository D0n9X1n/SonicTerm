use super::*;
use sonicterm_types::CellFlags;

/// Production-like key inputs: 10x20 cells, baseline 16, 14 px raster, GPU presenter, no hover.
fn inputs() -> RowKeyInputs {
    RowKeyInputs {
        style_rev: 1,
        cell_w: 10.0,
        cell_h: 20.0,
        baseline_y_in_cell: 16.0,
        raster_px: 14.0,
        software_presenter: false,
        hover_span: None,
    }
}

/// One row of default-coloured cells holding `text`.
fn cells(text: &str) -> Vec<Cell> {
    text.chars()
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
        .collect()
}

/// A record of `kind` anchored at `lead_col`, otherwise zero.
fn glyph(kind: RowGlyphKind, lead_col: u16) -> RowGlyph {
    RowGlyph {
        uv: [0.0, 0.0, 0.5, 0.5],
        color: [1.0; 4],
        raster_offset: [0.0; 2],
        shape_offset: [0.0; 2],
        raster_size: [8.0, 16.0],
        lead_col,
        end_col: lead_col + 1,
        kind_and_bits: RowGlyphBits {
            kind,
            is_color: false,
            is_subpixel: false,
            marker_fit_eligible: false,
            is_wide: false,
            has_extras: false,
            cluster_cells: 1,
        }
        .pack(),
    }
}

/// A row of `glyph_count` natural glyphs, exactly sized.
fn row_of(glyph_count: usize) -> CachedRow {
    CachedRow {
        glyphs: (0..glyph_count).map(|col| glyph(RowGlyphKind::Natural, col as u16)).collect(),
        ..CachedRow::default()
    }
}

/// A cache tracking one pane of `rows` by `cols`, with production budgets.
fn tracked(pane_id: PaneId, rows: u16, cols: u16) -> RowGlyphCache {
    let mut cache = RowGlyphCache::new();
    cache.begin_frame(&[(pane_id, rows, cols)]);
    cache
}

/// A row keeps its key wherever it is drawn, so content that moved one slot hits the entry the
/// previous pass admitted: the key folds no absolute row, slot, origin or surface.
#[test]
fn moved_rows_hit_their_entry() {
    let mut cache = tracked(1, 4, 8);
    let row = cells("moved");
    let key_at_slot_two = cache.content_key(&row, 8, &inputs());
    assert!(cache.insert(1, key_at_slot_two, 0, row_of(5)));
    cache.begin_frame(&[(1, 4, 8)]);
    // The same cells one slot higher: nothing the key reads has changed.
    let key_at_slot_one = cache.content_key(&row, 8, &inputs());
    assert_eq!(key_at_slot_one, key_at_slot_two);
    assert!(cache.get(1, key_at_slot_one, 0, |_| true).is_some(), "the moved row hits");
}

/// Every input that changes a row's records changes its key: each key input, the column count,
/// and each attribute of one cell.
#[test]
fn every_keyed_input_changes_the_key() {
    let cache = RowGlyphCache::new();
    let row = cells("ab");
    let base = cache.content_key(&row, 8, &inputs());
    let varied_inputs = [
        RowKeyInputs { style_rev: 2, ..inputs() },
        RowKeyInputs { cell_w: 10.5, ..inputs() },
        RowKeyInputs { cell_h: 21.0, ..inputs() },
        RowKeyInputs { baseline_y_in_cell: 15.0, ..inputs() },
        RowKeyInputs { raster_px: 15.0, ..inputs() },
        RowKeyInputs { software_presenter: true, ..inputs() },
        RowKeyInputs { hover_span: Some((0, 1)), ..inputs() },
    ];
    for (index, varied) in varied_inputs.iter().enumerate() {
        assert_ne!(cache.content_key(&row, 8, varied), base, "key input {index}");
    }
    assert_ne!(
        cache.content_key(&row, 8, &RowKeyInputs { hover_span: Some((0, 1)), ..inputs() }),
        cache.content_key(&row, 8, &RowKeyInputs { hover_span: Some((0, 2)), ..inputs() }),
        "the hover fragment's columns are keyed"
    );
    assert_ne!(cache.content_key(&row, 9, &inputs()), base, "column count");
    let mut cell_variants: Vec<Vec<Cell>> = Vec::new();
    let mut edit = |change: &dyn Fn(&mut Cell)| {
        let mut changed = row.clone();
        change(&mut changed[1]);
        cell_variants.push(changed);
    };
    edit(&|cell| cell.ch = 'c');
    edit(&|cell| cell.fg = Color::Indexed(2));
    edit(&|cell| cell.bg = Color::Indexed(3));
    edit(&|cell| cell.flags = CellFlags::UNDERLINE);
    edit(&|cell| cell.set_extras(Some("\u{301}".into())));
    edit(&|cell| cell.set_underline_style(UnderlineStyle::Curly));
    edit(&|cell| cell.set_underline_color(Some(Color::Indexed(4))));
    for (index, changed) in cell_variants.iter().enumerate() {
        assert_ne!(cache.content_key(changed, 8, &inputs()), base, "cell variant {index}");
    }
}

/// Selection and focus are not key inputs: a row hashed while selected, while unselected, with
/// and without focus reads the same cells and inputs, so it keeps one key and one entry. Both
/// reach only the background quads and the post-cache recolor.
#[test]
fn selection_and_focus_do_not_enter_the_key() {
    let mut cache = tracked(1, 2, 8);
    let row = cells("select");
    let unselected = cache.content_key(&row, 8, &inputs());
    assert!(cache.insert(1, unselected, 0, row_of(6)));
    let selected = cache.content_key(&row, 8, &inputs());
    assert_eq!(selected, unselected);
    assert!(cache.get(1, selected, 0, |_| true).is_some(), "a selected row replays");
}

/// Entries belong to one pane: a peer with the same content misses, and releasing one pane
/// keeps its peer's rows.
#[test]
fn panes_are_isolated() {
    let mut cache = RowGlyphCache::new();
    cache.begin_frame(&[(1, 2, 8), (2, 2, 8)]);
    let key = cache.content_key(cells("same"), 8, &inputs());
    assert!(cache.insert(1, key, 0, row_of(4)));
    assert!(cache.get(2, key, 0, |_| true).is_none(), "the peer pane has its own entries");
    assert!(cache.insert(2, key, 0, row_of(4)));
    cache.invalidate_pane(1);
    assert!(!cache.is_tracked(1));
    assert!(cache.get(2, key, 0, |_| true).is_some(), "the peer keeps its row");
}

/// Repeated eviction at the quota never grows a pane's table: survivors are drained and
/// reinserted, so no deleted markers accumulate.
#[test]
fn eviction_churn_keeps_the_table_allocation() {
    let mut cache = tracked(1, 8, 8);
    let reserved = cache.panes[&1].entries.capacity();
    for key in 1..2_000_u64 {
        assert!(cache.insert(1, key, 0, row_of(1)));
        assert!(cache.panes[&1].entries.capacity() <= reserved, "key {key} grew the table");
    }
    assert!(cache.pane_len(1).unwrap() < pane_entry_quota(8));
}

/// Two caches key the same row differently, because each draws its own hash keys. A smoke
/// check of the random keying, not a proof: equal keys have probability 2^-64.
#[test]
fn two_caches_hash_differently() {
    let row = cells("seeded");
    let first = RowGlyphCache::new().content_key(&row, 8, &inputs());
    let second = RowGlyphCache::new().content_key(&row, 8, &inputs());
    assert_ne!(first, second);
    assert_ne!(first, 0, "0 marks an empty slot and is never a key");
}

/// A wide one-row pane and a narrow tall pane each stay within their own quotas: ten thousand
/// versions of the wide row never hold more than four entries or four rows of payload, and the
/// narrow pane's rows all survive, because eviction never crosses panes.
#[test]
fn unequal_panes_stay_within_their_own_quotas() {
    let (wide, narrow) = (1, 2);
    let mut cache = RowGlyphCache::with_budgets(256 * 1024 * 1024, 16 * 1024 * 1024);
    cache.begin_frame(&[(wide, 1, 2048), (narrow, 2048, 1)]);
    assert!(cache.is_tracked(wide) && cache.is_tracked(narrow));
    let narrow_keys: Vec<u64> = (1..=64).collect();
    for &key in &narrow_keys {
        assert!(cache.insert(narrow, key, 0, row_of(1)));
    }
    for version in 0..10_000_u64 {
        assert!(cache.insert(wide, 1_000_000 + version, 0, row_of(256)));
        assert!(cache.pane_len(wide).unwrap() <= pane_entry_quota(1));
        assert!(cache.pane_payload_bytes(wide).unwrap() <= pane_payload_quota(1, 2048));
    }
    for key in narrow_keys {
        assert!(cache.contains(narrow, key), "narrow row {key} survived");
    }
    assert_eq!(cache.payload_bytes(), cache.folded_payload_bytes());
}

/// A row larger than its columns' share of the envelope is refused and retains nothing.
#[test]
fn oversized_rows_are_refused() {
    let mut cache = tracked(1, 4, 2);
    let before = cache.retained_amount();
    let oversized = row_of(row_payload_limit(2) / std::mem::size_of::<RowGlyph>() + 1);
    assert!(cached_row_payload_bytes(&oversized) > row_payload_limit(2));
    assert!(!cache.insert(1, 7, 0, oversized));
    assert_eq!(cache.retained_amount(), before);
    assert!(!cache.contains(1, 7));
}

/// The running payload sums equal a fresh fold after random inserts, replacements and
/// evictions, and admission charges the measured capacity after shrinking, never the length of
/// an unshrunk row.
#[test]
fn payload_accounting_matches_a_fresh_fold() {
    let mut cache = RowGlyphCache::new();
    cache.begin_frame(&[(1, 4, 16), (2, 3, 16)]);
    let mut unshrunk = row_of(3);
    unshrunk.glyphs.reserve_exact(40);
    assert!(cached_row_payload_bytes(&unshrunk) > 3 * std::mem::size_of::<RowGlyph>());
    assert!(cache.insert(1, 99, 0, unshrunk));
    assert_eq!(cache.pane_payload_bytes(1), Some(3 * std::mem::size_of::<RowGlyph>()));
    let mut state = 0x2545_f491_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3_000 {
        let pane_id = 1 + next() % 2;
        let key = 1 + next() % 40;
        let glyph_count = (next() % 16) as usize;
        let _ = cache.insert(pane_id, key, 0, row_of(glyph_count));
        assert_eq!(cache.payload_bytes(), cache.folded_payload_bytes());
        let per_pane: usize =
            [1, 2].iter().filter_map(|pane_id| cache.pane_payload_bytes(*pane_id)).sum();
        assert_eq!(per_pane, cache.payload_bytes());
    }
}

/// Both budgets hold after every mutation: payload admission is refused at the payload budget
/// without evicting another pane, a pane whose tracking would pass the tracking budget is
/// untracked and caches nothing, and the boundary cases the budget proof depends on.
#[test]
fn budgets_refuse_payload_and_untrack_panes() {
    let (rows, cols) = (2_u16, 8_u16);
    let full_row = || row_of(usize::from(cols));
    let row_bytes = cached_row_payload_bytes(&full_row());
    let panes: Vec<(PaneId, u16, u16)> = (1..=4).map(|pane_id| (pane_id, rows, cols)).collect();
    let mut probe = RowGlyphCache::new();
    probe.begin_frame(&panes);
    let four_panes_tracking = probe.tracking_bytes();

    let mut cache = RowGlyphCache::with_budgets(10 * row_bytes, four_panes_tracking);
    let check = |cache: &RowGlyphCache| {
        assert!(cache.payload_bytes() <= cache.payload_budget());
        assert!(cache.tracking_bytes() <= cache.tracking_budget());
        assert!(cache.retained_amount().bytes <= cache.payload_budget() + cache.tracking_budget());
    };
    cache.begin_frame(&panes);
    check(&cache);
    let mut admitted = 0;
    for index in 0..40_u64 {
        let pane_id = 1 + index % 4;
        if cache.insert(pane_id, 100 + index, 0, full_row()) {
            admitted += 1;
        }
        check(&cache);
    }
    assert_eq!(admitted, 10, "admission stops at the payload budget");
    let held: Vec<usize> = (1..=4).map(|pane_id| cache.pane_len(pane_id).unwrap()).collect();
    assert!(!cache.insert(1, 999, 0, full_row()), "the budget is spent");
    let after: Vec<usize> = (1..=4).map(|pane_id| cache.pane_len(pane_id).unwrap()).collect();
    assert_eq!(held, after, "a refused row evicts no other pane's rows");

    // A fifth pane does not fit the tracking budget: untracked, it misses and caches nothing.
    let mut five = panes.clone();
    five.push((5, rows, cols));
    cache.begin_frame(&five);
    check(&cache);
    assert!(!cache.is_tracked(5));
    assert!(!cache.insert(5, 7, 0, row_of(1)));
    assert!(cache.get(5, 7, 0, |_| true).is_none());

    // The first pane, and a one-row pane, tracked at exactly the tracking budget; one byte less
    // retains nothing at all.
    for (pane_rows, pane_cols) in [(rows, cols), (1, 1)] {
        let mut probe = RowGlyphCache::new();
        probe.begin_frame(&[(1, pane_rows, pane_cols)]);
        let exact = probe.tracking_bytes();
        let mut at_budget = RowGlyphCache::with_budgets(row_bytes, exact);
        at_budget.begin_frame(&[(1, pane_rows, pane_cols)]);
        assert!(at_budget.is_tracked(1), "{pane_rows}x{pane_cols} fits exactly");
        check(&at_budget);
        let mut below = RowGlyphCache::with_budgets(row_bytes, exact - 1);
        below.begin_frame(&[(1, pane_rows, pane_cols)]);
        assert!(!below.is_tracked(1));
        assert_eq!(below.retained_amount().bytes, 0, "a refused pane leaves nothing retained");
    }

    // A pane whose own storage fits but whose insertion grows the outer table past the budget.
    let three: Vec<(PaneId, u16, u16)> = (1..=3).map(|pane_id| (pane_id, 1, 1)).collect();
    let mut probe = RowGlyphCache::new();
    probe.begin_frame(&three);
    let own = PaneRows::new(1, 1).tracking_bytes();
    let mut growth = RowGlyphCache::with_budgets(row_bytes, probe.tracking_bytes() + own);
    growth.begin_frame(&three);
    let (outer_before, tracking_before) = (growth.panes.capacity(), growth.tracking_bytes());
    let mut four = three.clone();
    four.push((4, 1, 1));
    growth.begin_frame(&four);
    assert!(!growth.is_tracked(4), "the outer table's growth is charged before keeping a pane");
    assert_eq!(growth.panes.capacity(), outer_before);
    assert_eq!(growth.tracking_bytes(), tracking_before);
    check(&growth);

    // A failed admission keeps no reservation.
    let mut refusal = RowGlyphCache::with_budgets(row_bytes, four_panes_tracking);
    refusal.begin_frame(&[(1, rows, cols)]);
    let before = refusal.retained_amount();
    assert!(!refusal.insert(1, 3, 0, row_of(usize::from(cols) + 1)));
    assert_eq!(refusal.retained_amount(), before);

    // A same-key replacement that fits on `total - old + new` but not on `total + new`.
    let mut replacing = RowGlyphCache::with_budgets(row_bytes, four_panes_tracking);
    replacing.begin_frame(&[(1, rows, cols)]);
    assert!(replacing.insert(1, 3, 0, full_row()));
    assert_eq!(replacing.payload_bytes(), replacing.payload_budget());
    assert!(replacing.insert(1, 3, 1, full_row()), "a replacement frees the old payload first");
    assert!(replacing.get(1, 3, 1, |_| true).is_some());
    check(&replacing);

    // Contraction releases capacity.
    let mut contracting = RowGlyphCache::new();
    contracting.begin_frame(&[(1, 64, 8)]);
    let large = contracting.tracking_bytes();
    contracting.begin_frame(&[(1, 2, 8)]);
    assert!(contracting.tracking_bytes() < large);
}

/// The packed kind and flags decode to what was packed, for every kind, every flag combination
/// and every `u16` cluster cell count.
#[test]
fn row_glyph_bits_round_trip() {
    for kind in RowGlyphKind::ALL {
        for flags in 0..32_u32 {
            for cluster_cells in 0..=u16::MAX {
                let bits = RowGlyphBits {
                    kind,
                    is_color: flags & 1 != 0,
                    is_subpixel: flags & 2 != 0,
                    marker_fit_eligible: flags & 4 != 0,
                    is_wide: flags & 8 != 0,
                    has_extras: flags & 16 != 0,
                    cluster_cells,
                };
                assert_eq!(RowGlyphBits::unpack(bits.pack()), bits);
            }
        }
    }
}

/// A lookup built against another atlas identity misses: its UVs may name other tiles.
#[test]
fn an_atlas_identity_change_misses() {
    let mut cache = tracked(1, 1, 8);
    assert!(cache.insert(1, 5, 7, row_of(1)));
    assert!(cache.get(1, 5, 7, |_| true).is_some());
    assert!(cache.get(1, 5, 8, |_| true).is_none());
}

/// Only a row holding block glyphs is offered to the block validator, and a rejected block row
/// misses; a replacement with the same key then takes its place.
#[test]
fn only_block_rows_are_validated() {
    let mut cache = tracked(1, 2, 8);
    assert!(cache.insert(1, 5, 0, row_of(2)));
    assert!(cache.get(1, 5, 0, |_| panic!("a text row is never validated")).is_some());
    let mut blocks = row_of(1);
    blocks.glyphs.push(glyph(RowGlyphKind::Block, 1));
    assert!(cache.insert(1, 6, 0, blocks));
    assert!(cache.get(1, 6, 0, |_| false).is_none(), "a rejected block row misses");
    assert!(cache.get(1, 6, 0, |_| true).is_some());
}

/// Pinned keys survive eviction: with the committed slots and this pass's keys pinned, a
/// hundred admissions of new content never evict them.
#[test]
fn pinned_keys_survive_quota_churn() {
    let mut cache = tracked(1, 3, 8);
    for slot in 0..3_u16 {
        let key = 10 + u64::from(slot);
        assert!(cache.insert(1, key, 0, row_of(1)));
        cache.stage_slot(1, slot, key);
    }
    cache.commit_slots();
    for churn in 0..100_u64 {
        cache.begin_frame(&[(1, 3, 8)]);
        cache.pin(1, &[1_000 + churn]);
        assert!(cache.insert(1, 1_000 + churn, 0, row_of(1)));
    }
    for key in 10..13 {
        assert!(cache.contains(1, key), "committed key {key} stayed pinned");
    }
}

/// Staged keys become committed only on `commit_slots`; `discard_staged` keeps the committed
/// keys, an unemitted slot keeps its committed key across a commit, and `begin_frame` clears
/// the stage.
#[test]
fn staged_slots_commit_only_when_presented() {
    let mut cache = tracked(1, 3, 8);
    cache.stage_slot(1, 0, 10);
    cache.stage_slot(1, 1, 11);
    cache.commit_slots();
    assert_eq!(cache.committed_slot(1, 0), Some(10));
    cache.begin_frame(&[(1, 3, 8)]);
    cache.stage_slot(1, 0, 20);
    cache.discard_staged();
    cache.commit_slots();
    assert_eq!(cache.committed_slot(1, 0), Some(10), "a discarded stage commits nothing");
    cache.stage_slot(1, 0, 30);
    cache.begin_frame(&[(1, 3, 8)]);
    assert_eq!(cache.staged_slot(1, 0), Some(0), "a new pass starts with an empty stage");
    cache.stage_slot(1, 1, 31);
    cache.commit_slots();
    assert_eq!(cache.committed_slot(1, 0), Some(10), "an unemitted slot keeps its key");
    assert_eq!(cache.committed_slot(1, 1), Some(31));
}

/// A pane not drawn in a pass, or drawn at another size, is released at that pass's start, and
/// releasing a pane drops its committed slots with its rows.
#[test]
fn undrawn_and_resized_panes_are_released() {
    let mut cache = RowGlyphCache::new();
    cache.begin_frame(&[(1, 2, 8), (2, 2, 8)]);
    assert!(cache.insert(1, 5, 0, row_of(3)) && cache.insert(2, 5, 0, row_of(3)));
    cache.stage_slot(1, 0, 5);
    cache.commit_slots();
    cache.begin_frame(&[(1, 3, 8)]);
    assert!(!cache.is_tracked(2), "an undrawn pane is released");
    assert_eq!(cache.pane_len(1), Some(0), "a resized pane starts empty");
    assert_eq!(cache.committed_slot(1, 0), Some(0), "and with no committed slots");
    assert_eq!(cache.payload_bytes(), 0);
    assert!(cache.insert(1, 5, 0, row_of(3)));
    cache.stage_slot(1, 0, 5);
    cache.commit_slots();
    cache.invalidate_pane(1);
    cache.begin_frame(&[(1, 3, 8)]);
    assert_eq!(cache.committed_slot(1, 0), Some(0), "invalidation dropped the committed slots");
}

/// `invalidate_all` drops every row and its payload while panes stay tracked.
#[test]
fn invalidate_all_drops_rows_and_keeps_tracking() {
    let mut cache = tracked(1, 2, 8);
    assert!(cache.insert(1, 5, 0, row_of(3)));
    cache.invalidate_all();
    assert!(cache.is_empty() && cache.is_tracked(1));
    assert_eq!(cache.payload_bytes(), 0);
    assert_eq!(cache.retained_amount().bytes, cache.tracking_bytes());
}

/// Eviction starts only when an admission would pass the quota (four rows for a one-row
/// pane), then removes the oldest by last accepted use down to three quarters of it: a row hit
/// this pass outlives older rows.
#[test]
fn eviction_removes_the_least_recently_used_rows() {
    let mut cache = tracked(1, 1, 8);
    for key in 1..=4 {
        assert!(cache.insert(1, key, 0, row_of(1)));
        cache.begin_frame(&[(1, 1, 8)]);
    }
    assert_eq!(cache.pane_len(1), Some(4), "the quota is filled without eviction");
    assert!(cache.get(1, 1, 0, |_| true).is_some(), "key 1 becomes the most recent");
    cache.begin_frame(&[(1, 1, 8)]);
    assert!(cache.insert(1, 5, 0, row_of(1)));
    assert_eq!(cache.pane_len(1), Some(3), "eviction aimed for three quarters of the quota");
    assert!(cache.contains(1, 1), "the recently hit row survived");
    assert!(!cache.contains(1, 2) && !cache.contains(1, 3), "the oldest unhit rows went");
}

/// Retention reports payload and tracking storage, and the item count of cached rows.
#[test]
fn retained_amount_counts_payload_and_tracking() {
    let mut cache = tracked(7, 4, 16);
    assert!(cache.insert(7, 11, 17, row_of(9)));
    let payload = 9 * std::mem::size_of::<RowGlyph>();
    assert_eq!(cache.payload_bytes(), payload);
    assert_eq!(
        cache.retained_amount(),
        ResourceAmount { bytes: payload + cache.tracking_bytes(), items: 1 }
    );
}

/// Releasing everything leaves the figures of a new cache, keeps both budgets and the hasher (the
/// same row keeps its key), and the next frame tracks and admits again.
#[test]
fn release_all_returns_to_a_new_caches_figures() {
    let mut cache = RowGlyphCache::with_budgets(1 << 20, 1 << 16);
    cache.begin_frame(&[(1, 4, 16), (2, 3, 16)]);
    assert!(cache.insert(1, 99, 0, row_of(5)));
    assert!(cache.insert(2, 7, 0, row_of(3)));
    let key = cache.content_key(cells("hello"), 16, &inputs());
    assert!(cache.retained_amount().bytes > 0, "precondition: rows are cached");
    cache.release_all();
    let fresh = RowGlyphCache::with_budgets(1 << 20, 1 << 16);
    assert_eq!(cache.retained_amount(), fresh.retained_amount());
    assert_eq!(cache.payload_bytes(), 0);
    assert_eq!((cache.payload_budget(), cache.tracking_budget()), (1 << 20, 1 << 16));
    assert_eq!(cache.content_key(cells("hello"), 16, &inputs()), key, "the hasher is kept");
    cache.begin_frame(&[(1, 4, 16)]);
    assert!(cache.insert(1, 99, 0, row_of(5)), "the next frame admits again");
    assert!(cache.contains(1, 99));
}

/// Rasterizes every glyph as one 16x16 coverage tile, so a small atlas fills after a few glyphs.
struct SixteenPixelTiles;

impl crate::glyph_atlas::Rasterizer for SixteenPixelTiles {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<crate::glyph_atlas::RasterTile> {
        Some(crate::glyph_atlas::RasterTile {
            width: 16,
            height: 16,
            offset_x: 0,
            offset_y: 0,
            advance: 16.0,
            coverage: vec![255; 16 * 16],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// A glyph key for code point `code`, plain style.
fn atlas_key(code: u32) -> sonicterm_types::GlyphKey {
    sonicterm_types::GlyphKey {
        ch: char::from_u32(code).unwrap_or('a'),
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: code,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    }
}

/// Pin: a row admitted during an eviction-disabled retry stays a hit when eviction is re-enabled,
/// because that changes no atlas identity, and misses once a later insertion evicts a glyph, so a
/// kept row can never replay UVs a recycled tile now holds.
#[test]
fn a_row_admitted_before_an_eviction_misses_after_it() {
    let mut atlas = crate::glyph_atlas::GlyphAtlas::new(256, 256);
    let mut raster = SixteenPixelTiles;
    atlas.__set_entry_cap_for_test(4);
    atlas.set_eviction_enabled(false);
    for code in 65..69u32 {
        assert!(atlas.get_or_insert(atlas_key(code), &mut raster).is_some(), "{code} fits");
    }
    let mut cache = tracked(1, 1, 8);
    let admitted_at = atlas.identity();
    assert!(cache.insert(1, 5, admitted_at, row_of(4)));

    atlas.set_eviction_enabled(true);
    assert_eq!(atlas.identity(), admitted_at, "re-enabling eviction changes no identity");
    assert!(cache.get(1, 5, atlas.identity(), |_| true).is_some(), "the kept row still hits");

    let evictions = atlas.evictions();
    assert!(atlas.get_or_insert(atlas_key(90), &mut raster).is_some(), "the fifth glyph evicts");
    assert!(atlas.evictions() > evictions, "precondition: an eviction happened");
    assert!(cache.get(1, 5, atlas.identity(), |_| true).is_none(), "the kept row misses after it");
}
