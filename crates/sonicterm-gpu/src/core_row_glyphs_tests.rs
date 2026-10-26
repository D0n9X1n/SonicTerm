//! Renderer row glyphs contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// A row of base characters each carrying the most combining marks a cell may hold shapes to
/// more records than its columns' share of the envelope: it is drawn but never admitted, and
/// every emission draws exactly what a cold draw does.
#[test]
fn refused_rows_draw_like_cold_rows() {
    let mut grid = Grid::new(2, 1);
    for base in ['a', 'b'] {
        grid.put_char(base, Color::Default, Color::Default, CellFlags::empty());
        // Each U+0301 is two bytes, so 32 fill the cell's 64-byte extras bound.
        for _ in 0..32 {
            grid.put_char('\u{301}', Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let limit = sonicterm_text::row_glyph_cache::row_payload_limit(2);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let first = rig.emit(&grid, 0, 0);
    assert!(
        first.glyphs.len() * std::mem::size_of::<sonicterm_text::row_glyph_cache::RowGlyph>()
            > limit,
        "the fixture's records exceed the row envelope: {} glyphs",
        first.glyphs.len()
    );
    assert!(!first.replayed && !rig.cache.contains(7, first.key), "an oversized row is refused");
    let second = rig.emit(&grid, 0, 0);
    assert!(!second.replayed, "a refused row is shaped again");
    assert_eq!(second.glyph_bytes(), first.glyph_bytes(), "and draws what a cold draw does");
}

/// At the history cap a scroll shifts every absolute row, yet only the new row reshapes: the
/// three rows that moved one slot up replay with no shape request, and the unique shaping row
/// written after the linefeed (the positive control) is shaped. A wheel move starts cold.
#[test]
fn scrolling_one_line_at_the_history_cap_reshapes_only_the_new_row() {
    let mut grid = Grid::new(8, 4);
    grid.set_scrollback_limit(4);
    for index in 0..10 {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in format!("r{index}=>é").chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    assert_eq!(grid.scrollback_len(), 4);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let top = grid.scrollback_len() as u64;
    for slot in 0..4 {
        rig.emit(&grid, top, slot);
    }
    rig.cache.commit_slots();
    grid.carriage_return();
    grid.linefeed();
    for character in "r10=>é".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert_eq!(grid.scrollback_len(), 4, "the history stayed at its cap");
    rig.begin(&grid);
    let top = grid.scrollback_len() as u64;
    for slot in 0..4u16 {
        let (row, stats) = counted(|| rig.emit(&grid, top, slot));
        if slot < 3 {
            assert!(row.replayed, "slot {slot} moved up and replays");
            assert_eq!(stats.shape_requests, 0, "slot {slot} is not shaped");
        } else {
            assert!(!row.replayed, "the new row misses");
            assert!(stats.shape_requests >= 1, "the new row is shaped");
        }
    }

    // A wheel move up one row from a cold cache and history: three rows replay one slot lower.
    let mut grid = Grid::new(8, 4);
    for index in 0..8 {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in format!("w{index}=>é").chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let mut rig = GlyphRig::new(false);
    let live = grid.scrollback_len() as u64;
    rig.begin(&grid);
    for slot in 0..4 {
        rig.emit(&grid, live, slot);
    }
    rig.cache.commit_slots();
    rig.begin(&grid);
    let replays: Vec<bool> = (0..4).map(|slot| rig.emit(&grid, live - 1, slot).replayed).collect();
    assert_eq!(replays, [false, true, true, true]);
}

/// Presentation-only dirt (every row marked dirty with unchanged cells) replays every row
/// through the production pane seam with no shaping, and the glyph cache's invalidation
/// counters stay 0 because nothing drops a glyph row for dirt.
#[test]
fn presentation_only_dirt_replays_through_the_production_seam() {
    let mut grid = text_grid(8, &["p0=>é", "p1=>é", "p2=>é", "p3=>é"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let first = policy_plan(4, 0.0, 1, Vec::new(), Vec::new(), None);
    let (_, warm) = assemble_pass(&mut rig, &mut ink, &grid, &first);
    assert_eq!(warm.row_cache_misses, 4);
    present_pass(&mut rig, &mut ink, &first);
    grid.mark_all_dirty();
    let dirty: Vec<usize> = grid.dirty_rows().collect();
    let second = policy_plan(4, 0.0, 2, dirty, Vec::new(), Some(&first.key));
    assert!(second.panes[0].emit_rows.iter().all(|emitted| *emitted));
    let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &second);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (4, 0), "every row replays");
    assert_eq!(stats.shape_requests, 0, "nothing is shaped again");
    assert_eq!((stats.row_cache_invalidate_visits, stats.row_cache_invalidate_us), (0, 0));
}

/// A software-presenter block row is accepted only where every block still rasterizes at its
/// stored size: at a fractional cell height two slots with different integer heights miss, and
/// the second's shape replaces the entry; a slot with the second's height then hits. A
/// half-pixel origin at a fractional pitch changes a block's width and misses. GPU blocks and
/// ASCII rows hit at any slot.
#[test]
fn software_block_rows_hit_only_where_their_size_holds() {
    let block_rows: Vec<&str> = vec!["██"; 64];
    let grid = text_grid(2, &block_rows);
    let height_at = |cell_h: f32, top_inset: f32, slot: u16| {
        let top = top_inset + f32::from(slot) * cell_h;
        let bottom = top_inset + (f32::from(slot) + 1.0) * cell_h;
        software_block_glyph_target_rect(0.0, top, 10.0, bottom).3
    };
    let (cell_h, top_inset) = (7.3, 0.1);
    let first_slot = 0u16;
    let other = (1..64)
        .find(|slot| {
            height_at(cell_h, top_inset, *slot) != height_at(cell_h, top_inset, first_slot)
        })
        .unwrap();
    let same_as_other = (0..64)
        .find(|slot| {
            *slot != other
                && height_at(cell_h, top_inset, *slot) == height_at(cell_h, top_inset, other)
        })
        .unwrap();
    let mut rig = GlyphRig::new(true);
    rig.cell_size = (10.0, cell_h);
    rig.origin = (0.0, top_inset);
    rig.surface = (40.0, 480.0);
    rig.begin(&grid);
    assert!(!rig.emit(&grid, 0, first_slot).replayed, "the first draw is shaped");
    assert!(!rig.emit(&grid, 0, other).replayed, "another integer height misses");
    assert!(rig.emit(&grid, 0, same_as_other).replayed, "the replacement fits its own height");
    assert!(!rig.emit(&grid, 0, first_slot).replayed, "and no longer fits the first height");

    let mut rig = GlyphRig::new(true);
    rig.cell_size = (10.5, 20.0);
    rig.surface = (40.0, 480.0);
    let width_at = |origin_x: f32| {
        let snapped = build_snapped_cell_x(origin_x, 10.5, 2);
        software_block_glyph_target_rect(snapped[0], 0.0, snapped[1], 20.0).2
    };
    assert_ne!(width_at(0.0), width_at(0.5), "the fixture changes a block's width");
    rig.begin(&grid);
    rig.emit(&grid, 0, 0);
    rig.origin = (0.5, 0.0);
    assert!(!rig.emit(&grid, 0, 0).replayed, "a half-pixel origin changes the width");
    assert!(rig.emit(&grid, 0, 0).replayed, "the same width hits");

    let mut rig = GlyphRig::new(false);
    rig.cell_size = (10.0, cell_h);
    rig.origin = (0.0, top_inset);
    rig.surface = (40.0, 480.0);
    rig.begin(&grid);
    rig.emit(&grid, 0, first_slot);
    assert!(rig.emit(&grid, 0, other).replayed, "GPU blocks hit at any slot");
    let ascii = text_grid(8, &["text", "text"]);
    let mut rig = GlyphRig::new(true);
    rig.begin(&ascii);
    rig.emit(&ascii, 0, 0);
    assert!(rig.emit(&ascii, 0, 1).replayed, "ASCII rows hit at any slot");
}

/// A block that drew nothing is never cached: on the software presenter at cell height 2048.4
/// and top 0.1 the block's target is 2049 pixels at slot 1, past a 2048 atlas, so it is
/// skipped and the row is not admitted; at slot 0 the target is 2048, so the row misses and
/// draws the block as a cold draw does. A glyph the atlas refuses because its entry index is
/// full with eviction barred keeps its row out of the cache too, while drawing tofu.
#[test]
fn blocks_that_drew_nothing_are_never_cached() {
    let height_at = |slot: u16| {
        let top = 0.1 + f32::from(slot) * 2048.4;
        software_block_glyph_target_rect(0.0, top, 10.0, top + 2048.4).3
    };
    assert_eq!((height_at(0), height_at(1)), (2048.0, 2049.0));
    let grid = text_grid(2, &["█", "█"]);
    let mut rig = GlyphRig::new(true);
    rig.atlas = GlyphAtlas::new(2048, 2048);
    rig.cell_size = (10.0, 2048.4);
    rig.origin = (0.0, 0.1);
    rig.surface = (40.0, 4200.0);
    rig.begin(&grid);
    let skipped = rig.emit(&grid, 0, 1);
    assert!(skipped.glyphs.is_empty(), "the block drew nothing at slot 1");
    assert!(!rig.cache.contains(7, skipped.key), "and its row was not admitted");
    let drawn = rig.emit(&grid, 0, 0);
    assert!(!drawn.replayed && !drawn.glyphs.is_empty(), "slot 0 misses and draws the block");
    rig.cache.invalidate_all();
    assert_eq!(rig.emit(&grid, 0, 0).glyph_bytes(), drawn.glyph_bytes(), "as a cold draw does");

    let grid = text_grid(4, &["AB"]);
    let mut rig = GlyphRig::new(false);
    rig.atlas = GlyphAtlas::new(256, 256);
    rig.atlas.__set_entry_cap_for_test(1);
    rig.atlas.set_eviction_enabled(false);
    rig.begin(&grid);
    let refused = rig.emit(&grid, 0, 0);
    assert_eq!(refused.missing, vec!['B'], "the refused glyph draws tofu");
    assert!(!rig.cache.contains(7, refused.key), "a row with a refused glyph is not admitted");
}

/// A run whose shaping fails is drawn (as nothing) but its row is never cached; the next
/// emission at the same pane, slot, origin, surface, cells, style and atlas identity, with no
/// cache clear and no dirt, shapes it again and admits it, and the one after replays. A row
/// whose first style run fails still draws its later run and is not admitted, and a row drawn
/// with no font stack is not admitted.
#[test]
fn failed_shaping_runs_are_drawn_but_never_cached() {
    let grid = text_grid(8, &["a=>b"]);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    fail_next_shape_for_test();
    let failed = rig.emit(&grid, 0, 0);
    assert!(failed.glyphs.is_empty(), "the failed run draws nothing");
    assert!(!rig.cache.contains(7, failed.key), "a row with a failed run is not admitted");
    let (second, stats) = counted(|| rig.emit(&grid, 0, 0));
    assert!(!second.replayed && stats.shape_requests >= 1, "the run is shaped again");
    assert!(!second.glyphs.is_empty() && rig.cache.contains(7, second.key), "and admitted");
    let (third, stats) = counted(|| rig.emit(&grid, 0, 0));
    assert!(third.replayed && stats.shape_requests == 0, "then it replays");
    rig.cache.invalidate_all();
    assert_eq!(rig.emit(&grid, 0, 0).glyph_bytes(), second.glyph_bytes(), "a cold draw agrees");

    let mut mixed = Grid::new(8, 1);
    for (character, flags) in [
        ('a', CellFlags::BOLD),
        ('=', CellFlags::BOLD),
        ('>', CellFlags::empty()),
        ('b', CellFlags::empty()),
    ] {
        mixed.put_char(character, Color::Default, Color::Default, flags);
    }
    let mut rig = GlyphRig::new(false);
    rig.begin(&mixed);
    fail_next_shape_for_test();
    let partial = rig.emit(&mixed, 0, 0);
    assert!(!partial.glyphs.is_empty(), "the later run still draws");
    assert!(!rig.cache.contains(7, partial.key), "the row is not admitted");

    let mut rig = GlyphRig::new(false);
    rig.stack = None;
    rig.raster = None;
    rig.begin(&grid);
    let unshaped = rig.emit(&grid, 0, 0);
    assert!(!rig.cache.contains(7, unshaped.key), "no font stack: the row is not admitted");
}

/// A Partial frame reuses cached rows: with F0 an edit of slot 2 emits slot 2 alone, one miss
/// that adds a seventh entry and evicts nothing; with the production-like pad of F20 the same
/// edit emits slots 0 to 4, four of them hits. A later Full repaint hits all six rows, five
/// unchanged and the admitted edited row.
#[test]
fn partial_frames_reuse_cached_rows() {
    for (pad, mask, hits) in [(0.0, vec![2usize], 0u64), (20.0, vec![0, 1, 2, 3, 4], 4)] {
        let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
        let mut rig = GlyphRig::new(false);
        let mut ink = crate::row_ink::RowInkTable::default();
        let warm = policy_plan(6, pad, 1, Vec::new(), Vec::new(), None);
        assemble_pass(&mut rig, &mut ink, &grid, &warm);
        present_pass(&mut rig, &mut ink, &warm);
        assert_eq!(rig.cache.len(), 6);
        write_row(&mut grid, 2, "EDIT");
        let edit = policy_plan(6, pad, 2, vec![2], strip_records(6), Some(&warm.key));
        assert_eq!(edit.mode, RenderMode::Partial, "pad {pad}");
        let emitted: Vec<usize> = (0..6).filter(|slot| edit.panes[0].emit_rows[*slot]).collect();
        assert_eq!(emitted, mask, "pad {pad}: the planned mask");
        let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &edit);
        assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (hits, 1), "pad {pad}");
        assert_eq!(rig.cache.len(), 7, "pad {pad}: one admission and no eviction");
        present_pass(&mut rig, &mut ink, &edit);
        let full = policy_plan(6, pad, 3, Vec::new(), Vec::new(), None);
        let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &full);
        assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (6, 0), "pad {pad}");
    }
}

/// Pinned rows survive quota churn: a hundred presented Partial frames each edit slot 5 of a
/// six-row pane (quota 24) with new content, and the committed rows of slots 0 to 4, never
/// touched since the warm frame and the oldest entries, survive; the final Full repaint hits
/// all six rows.
#[test]
fn pinned_rows_survive_quota_churn() {
    let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let mut previous = policy_plan(6, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &grid, &previous);
    present_pass(&mut rig, &mut ink, &previous);
    let unchanged: Vec<u64> =
        (0..5).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    for frame in 0..100u64 {
        write_row(&mut grid, 5, &format!("e{frame:03}"));
        let edit = policy_plan(6, 0.0, 2 + frame, vec![5], strip_records(6), Some(&previous.key));
        assert_eq!(edit.mode, RenderMode::Partial);
        assemble_pass(&mut rig, &mut ink, &grid, &edit);
        present_pass(&mut rig, &mut ink, &edit);
        previous = edit;
    }
    for (slot, key) in unchanged.iter().enumerate() {
        assert!(rig.cache.contains(7, *key), "slot {slot}'s committed row survived");
    }
    let full = policy_plan(6, 0.0, 500, Vec::new(), Vec::new(), None);
    let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &full);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (6, 0));
}

/// A moved row is pinned before any admission: with three rows (quota 12), eleven entries of
/// which the committed `[A, B, C]` are the oldest, a Full pass emitting `[X, Y, A]` admits X
/// and Y, evicting only unpinned filler, and A hits. When that pass is not presented, its
/// staged keys are discarded, the committed keys stay `[A, B, C]`, and the retry draws the
/// same bytes. The history-cap drain that shifts every absolute row is covered by
/// `scrolling_one_line_at_the_history_cap_reshapes_only_the_new_row`.
#[test]
fn moved_rows_are_pinned_before_any_admission() {
    let first = text_grid(8, &["A=>1", "B=>2", "C=>3"]);
    let moved = text_grid(8, &["X=>4", "Y=>5", "A=>1"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let plan = policy_plan(3, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &first, &plan);
    present_pass(&mut rig, &mut ink, &plan);
    let committed: Vec<u64> =
        (0..3).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    rig.cache.begin_frame(&[(7, 3, 8)]);
    // Fillers are admitted on a later pass, with the committed slots pinned as every pass pins them.
    rig.cache.pin(7, &[]);
    let identity = row_cache_atlas_identity(&rig.atlas);
    for filler in 0..8u64 {
        let row = sonicterm_text::row_glyph_cache::CachedRow::default();
        assert!(rig.cache.insert(7, 1_000 + filler, identity, row));
    }
    assert_eq!(rig.cache.len(), 11);
    let full = policy_plan(3, 0.0, 2, Vec::new(), Vec::new(), None);
    let (glyphs, stats) = assemble_pass(&mut rig, &mut ink, &moved, &full);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (1, 2), "A hits, X and Y miss");
    for key in &committed {
        assert!(rig.cache.contains(7, *key), "committed row {key} stayed pinned");
    }
    rig.cache.discard_staged();
    let after: Vec<u64> = (0..3).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    assert_eq!(after, committed, "an unpresented pass commits nothing");
    let (retry, stats) = assemble_pass(&mut rig, &mut ink, &moved, &full);
    assert_eq!(stats.row_cache_hits, 3, "the retry replays all three rows");
    assert_eq!(bytemuck::cast_slice::<_, u8>(&retry), bytemuck::cast_slice::<_, u8>(&glyphs));
}

/// Identical rows in one pass share one entry: the first misses and is admitted, the others
/// hit it and draw what a cold draw of their own slot does. An atlas growth changes the atlas
/// identity, so the clean retry misses every row rather than replaying stale UVs.
#[test]
fn identical_rows_share_an_entry_and_atlas_growth_misses() {
    let grid = text_grid(8, &["same=>x", "same=>x", "same=>x"]);
    let mut rig = GlyphRig::new(false);
    rig.atlas = GlyphAtlas::growable(256, 2048);
    rig.begin(&grid);
    let replays: Vec<bool> = (0..3).map(|slot| rig.emit(&grid, 0, slot).replayed).collect();
    assert_eq!(replays, [false, true, true]);
    rig.begin(&grid);
    let warm_slot_two = rig.emit(&grid, 0, 2);
    rig.cache.invalidate_all();
    rig.begin(&grid);
    assert_eq!(rig.emit(&grid, 0, 2).glyph_bytes(), warm_slot_two.glyph_bytes());
    assert!(rig.atlas.grow_to(512), "the atlas doubles");
    rig.begin(&grid);
    assert!(!rig.emit(&grid, 0, 0).replayed, "a grown atlas's identity misses");
}

/// A Partial first pass that falls back admits its real entries; the forced-Full second pass
/// starts with an empty stage, replays the admitted row, draws what a cold Full does, stages
/// one ink record per emitted slot, and its keys, not the first pass's, are the ones committed.
#[test]
fn a_partial_fallbacks_staged_keys_never_commit() {
    let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let warm = policy_plan(6, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &grid, &warm);
    present_pass(&mut rig, &mut ink, &warm);
    let warm_key = rig.cache.committed_slot(7, 2).unwrap();
    write_row(&mut grid, 2, "EDIT");
    let partial = policy_plan(6, 0.0, 2, vec![2], strip_records(6), Some(&warm.key));
    assert_eq!(partial.mode, RenderMode::Partial);
    let (_, first) = assemble_pass(&mut rig, &mut ink, &grid, &partial);
    assert_eq!(first.row_cache_misses, 1, "the first pass admits the edited row");
    let edited_key = rig.cache.staged_slot(7, 2).unwrap();
    assert_ne!(edited_key, warm_key);
    let mut forced = policy_plan(6, 0.0, 2, vec![2], strip_records(6), Some(&warm.key));
    forced.force_full();
    let (forced_glyphs, second) = assemble_pass(&mut rig, &mut ink, &grid, &forced);
    assert_eq!((second.row_cache_hits, second.row_cache_misses), (6, 0));
    assert_eq!(ink.staged_len(), 6, "one surviving ink record per emitted slot");
    present_pass(&mut rig, &mut ink, &forced);
    assert_eq!(rig.cache.committed_slot(7, 2), Some(edited_key));
    rig.cache.invalidate_all();
    let (cold, _) = assemble_pass(&mut rig, &mut ink, &grid, &forced);
    assert_eq!(bytemuck::cast_slice::<_, u8>(&forced_glyphs), bytemuck::cast_slice::<_, u8>(&cold));

    // The first pass's stage is cleared before the second pass emits anything.
    rig.cache.begin_frame(&[(7, 6, 8)]);
    rig.cache.stage_slot(7, 2, 99);
    rig.cache.begin_frame(&[(7, 6, 8)]);
    assert_eq!(rig.cache.staged_slot(7, 2), Some(0));
}

/// With eviction off, as during a glyph-atlas retry, and the atlas full, a row needing a new glyph
/// still draws its resident glyphs but is never admitted to the row cache. The positive control drops
/// a resident row's cache entry first, so its admission in the same configuration is a new one.
#[test]
fn a_row_the_full_retry_atlas_refuses_is_drawn_but_not_admitted() {
    let plain = CellFlags::empty();
    let resident = styled_grid(4, &[('a', plain), ('b', plain)]);
    let novel = styled_grid(4, &[('a', plain), ('z', plain)]);
    let mut rig = GlyphRig::new(false);
    rig.begin(&resident);
    let first = rig.emit(&resident, 0, 0);
    assert!(rig.cache.contains(7, first.key), "precondition: a complete row is admitted");
    rig.atlas.set_eviction_enabled(false);
    let full = rig.atlas.len();
    rig.atlas.__set_entry_cap_for_test(full);

    rig.cache.invalidate_pane(7);
    assert!(!rig.cache.contains(7, first.key), "precondition: the entry was dropped");
    rig.begin(&resident);
    let again = rig.emit(&resident, 0, 0);
    assert_eq!(again.key, first.key);
    assert!(rig.cache.contains(7, again.key), "a resident row is admitted afresh");

    rig.begin(&novel);
    let refused = rig.emit(&novel, 0, 0);
    assert!(!refused.glyphs.is_empty(), "the resident glyph still draws");
    assert_eq!(rig.atlas.len(), full, "the full atlas admitted nothing");
    assert!(!rig.cache.contains(7, refused.key), "a row missing a glyph is not admitted");
}

#[test]
fn unwinding_while_row_keys_are_in_use_keeps_the_key_buffer() {
    // The pane seam borrows the frame scratch's row-key buffer in place while it pins and emits
    // rows. An unwind during emission (here, column edges too short for a block glyph's lead
    // column) must leave that buffer, with its warm allocation, in the scratch.
    let grid = text_grid(8, &["─x", "ab", "cd"]);
    let plan = policy_plan(3, 0.0, 1, Vec::new(), Vec::new(), None);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    begin_pass(&mut rig, &mut ink, &grid, &plan);
    let mut row_keys: Vec<u64> = Vec::with_capacity(64);
    let warm_pointer = row_keys.as_ptr();
    let planned = &plan.panes[0];
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut glyphs, mut underlines, mut tofu) = (Vec::new(), Vec::new(), Vec::new());
        let (mut missing, mut spans, mut owners) = (Vec::new(), Vec::new(), Vec::new());
        assemble_pane_glyph_rows(
            rig.shaping(),
            PaneGlyphRows {
                pane_id: planned.id,
                grid: &grid,
                planned,
                origin: (planned.layout.x, planned.layout.y),
                // No column edges: the block glyph's lookup unwinds mid-emission.
                snapped_cell_x: &[],
                pane_hovered_url: None,
            },
            GlyphFrame {
                glyph_instances: &mut glyphs,
                underlines: &mut underlines,
                missing_tofu: &mut tofu,
                missing_chars_this_frame: &mut missing,
                row_spans: &mut spans,
            },
            PaneGlyphSinks {
                row_ink: &mut ink,
                ink_surface: plan.surface,
                underline_owners: &mut owners,
                injected_row_glyph: None,
                emitted_slots: None,
                row_keys: &mut row_keys,
                missing_rows: None,
                fonts: None,
            },
        );
    }));
    assert!(unwound.is_err(), "emission unwinds on the missing column edges");
    assert!(row_keys.capacity() >= 64, "the key buffer stays in the scratch with its capacity");
    assert_eq!(row_keys.as_ptr(), warm_pointer, "and keeps its warm allocation");
}
