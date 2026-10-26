//! Renderer row projection contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// Projecting position-free records reproduces the frozen pre-record emission byte for byte,
/// whole instances included, for every kind: separate raster and shaping offsets (including
/// 1.9, 7 and 8.7), a small surface, a fractional origin and pitch, status-marker eligibility,
/// wide and combining lead cells, colour (emoji-style) and subpixel tiles, and blocks spanning
/// one and two columns on both presenters. The oracle is independent of production arithmetic.
#[test]
fn record_projection_matches_the_frozen_emission_geometry() {
    use sonicterm_text::glyph_atlas::GlyphInfo;
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let info = |offset: [i32; 2], size: [u32; 2], is_color: bool| GlyphInfo {
        uv: [0.1, 0.2, 0.3, 0.4],
        px_size: size,
        px_offset: offset,
        advance: 0.0,
        is_color,
        is_subpixel: !is_color,
        missing: false,
        oversize: false,
    };
    let geometries = [
        ((0.0, 0.0), (10.0, 20.0), 16.0, (400.0, 200.0)),
        ((1.9, 7.0), (8.7, 17.3), 13.84, (80.0, 80.0)),
        ((0.5, 0.1), (10.5, 20.25), 16.2, (123.0, 77.0)),
    ];
    let mut compared = 0;
    for (origin, cell_size, baseline, surface) in geometries {
        for row in [0u16, 1, 3] {
            let cell = OracleCell {
                snapped: build_snapped_cell_x(origin.0, cell_size.0, 6),
                cell_size,
                top_inset: origin.1,
                row,
                baseline_y_in_cell: baseline,
                surface,
            };
            let at = RowPlacement {
                slot: row,
                origin,
                cols: 6,
                snapped_cell_x: &cell.snapped,
                cell_size,
                baseline_y_in_cell: baseline,
                surface,
            };
            for is_color in [false, true] {
                let rgba = if is_color { [1.0; 4] } else { [0.3, 0.6, 0.9, 1.0] };
                let tile = info([1, -12], [7, 13], is_color);
                let natural = tile_record(RowGlyphKind::Natural, 2, &tile, rgba);
                assert_eq!(
                    instance_bytes(&project_row_glyph(&natural, &at, false)),
                    instance_bytes(&oracle_natural(&cell, 2, &tile, rgba)),
                    "natural colour={is_color}"
                );
                compared += 1;
                let leads = [
                    ('\u{25cf}', 1usize, false, false),
                    ('\u{23fa}', 1, false, false),
                    ('\u{25ef}', 1, false, false),
                    ('\u{25cf}', 1, true, false),
                    ('\u{25cf}', 1, false, true),
                    ('\u{25cf}', 2, false, false),
                    ('x', 1, false, false),
                    ('😀', 2, true, false),
                ];
                for (character, cluster_cells, is_wide, has_extras) in leads {
                    for kind in [RowGlyphKind::Fallback, RowGlyphKind::Shaped] {
                        for shape_offset in [(0.0, 0.0), (1.9, -7.0), (8.7, 2.5)] {
                            let tile = info([2, -9], [9, 9], is_color);
                            let mut record = tile_record(kind, 3, &tile, rgba);
                            let mut lead_cell = Cell::plain(
                                character,
                                Color::Default,
                                Color::Default,
                                CellFlags::empty(),
                            );
                            if is_wide {
                                lead_cell.flags = CellFlags::WIDE;
                            }
                            if has_extras {
                                lead_cell.set_extras(Some("\u{301}".into()));
                            }
                            let shaped = sonicterm_text::shape::ShapedGlyph {
                                lead_col: 3,
                                cluster_cells: cluster_cells as u16,
                                font_slot: 0,
                                glyph_id: 5,
                                x_advance: 0.0,
                                x_offset: 0.0,
                                y_offset: 0.0,
                                ch: character,
                            };
                            let eligible = status_marker_fit_eligible(
                                character,
                                cluster_cells,
                                is_wide,
                                has_extras,
                            );
                            set_shaped_bits(
                                &mut record,
                                [shape_offset.0, shape_offset.1],
                                eligible,
                                &shaped,
                                is_wide,
                                &lead_cell,
                            );
                            let expected = oracle_shaped(
                                &cell,
                                (3, character, cluster_cells, is_wide, has_extras),
                                &tile,
                                shape_offset,
                                rgba,
                            );
                            assert_eq!(
                                instance_bytes(&project_row_glyph(&record, &at, false)),
                                instance_bytes(&expected),
                                "{kind:?} {character:?} cells={cluster_cells} wide={is_wide} \
                                 extras={has_extras} offset={shape_offset:?} colour={is_color}"
                            );
                            compared += 1;
                        }
                    }
                }
                for span in [1usize, 2] {
                    for software in [false, true] {
                        let tile = info([0, 0], [10, 20], is_color);
                        let mut record = tile_record(RowGlyphKind::Block, 4, &tile, rgba);
                        record.raster_offset = [0.0; 2];
                        record.end_col = (4 + span).min(cell.snapped.len() - 1) as u16;
                        assert_eq!(
                            instance_bytes(&project_row_glyph(&record, &at, software)),
                            instance_bytes(&oracle_block(&cell, 4, span, software, &tile, rgba)),
                            "block span {span} software {software} colour={is_color}"
                        );
                        compared += 1;
                    }
                }
            }
        }
    }
    assert_eq!(compared, 3 * 3 * 2 * (1 + 8 * 2 * 3 + 2 * 2), "every case was compared");
}

/// The tracked font faces only (no OS font discovery), warm and cold, draw the same bytes. The
/// grid covers normal, bold, italic and bold-italic runs, a ligature trigger, box drawing, a
/// combining cluster, and CJK, emoji and wide clusters. Fallback is controlled: the stack has no
/// system font source, so CJK draws from the tracked faces and the emoji is a stable missing
/// glyph. Exactly these rows are drawn once to
/// warm the atlas and fonts; then only the row cache is cleared, a cold pass misses every row,
/// and a warm pass hits every row with identical glyphs, decorations and missing characters.
#[test]
fn tracked_fonts_draw_the_same_warm_and_cold() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let styles = [
        CellFlags::empty(),
        CellFlags::BOLD,
        CellFlags::ITALIC,
        CellFlags::BOLD | CellFlags::ITALIC,
    ];
    let mut grid = Grid::new(12, 8);
    for (row, flags) in styles.iter().enumerate() {
        grid.goto(row as u16, 0);
        for character in "ab=>c->d".chars() {
            grid.put_char(character, Color::Default, Color::Default, *flags);
        }
    }
    write_row(&mut grid, 4, "│─┼█▌ e");
    grid.put_char('\u{301}', Color::Default, Color::Default, CellFlags::empty());
    write_row(&mut grid, 5, "中文 x");
    write_row(&mut grid, 6, "😀 ok");
    write_row(&mut grid, 7, "a中😀b");
    grid.clear_dirty();
    let rows = grid.rows;
    let mut rig = GlyphRig::with_stack(crate::lib_tests::tracked_font_stack(14.0), false);
    rig.surface = (400.0, 400.0);
    rig.begin(&grid);
    for slot in 0..rows {
        rig.emit(&grid, 0, slot);
    }
    rig.cache.invalidate_all();
    rig.begin(&grid);
    let (cold, cold_stats) =
        counted(|| (0..rows).map(|slot| rig.emit(&grid, 0, slot)).collect::<Vec<_>>());
    rig.begin(&grid);
    let (warm, warm_stats) =
        counted(|| (0..rows).map(|slot| rig.emit(&grid, 0, slot)).collect::<Vec<_>>());
    let row_count = u64::from(rows);
    assert_eq!((cold_stats.row_cache_hits, cold_stats.row_cache_misses), (0, row_count));
    assert_eq!((warm_stats.row_cache_hits, warm_stats.row_cache_misses), (row_count, 0));
    assert_eq!(warm_stats.shape_requests, 0, "the warm pass shapes nothing");
    for (slot, (cold_row, warm_row)) in cold.iter().zip(&warm).enumerate() {
        assert!(!cold_row.glyphs.is_empty() || !cold_row.missing.is_empty(), "slot {slot} draws");
        assert_eq!(warm_row.glyph_bytes(), cold_row.glyph_bytes(), "slot {slot} glyph bytes");
        assert_eq!(warm_row.decorations, cold_row.decorations, "slot {slot} decorations");
        assert_eq!(warm_row.missing, cold_row.missing, "slot {slot} missing characters");
    }
    let style_bytes: Vec<Vec<u8>> = cold[..4].iter().map(EmittedRow::glyph_bytes).collect();
    for (index, bytes) in style_bytes.iter().enumerate().skip(1) {
        assert_ne!(*bytes, style_bytes[0], "style row {index} draws other faces than normal");
    }
    // `ConfigDirsOnly` installs no system source, so CJK draws from the tracked face's own
    // coverage and the emoji, which no tracked face has, is a stable missing glyph.
    assert!(
        !cold[5].glyphs.is_empty() && cold[5].missing.is_empty(),
        "CJK draws from tracked faces"
    );
    assert!(cold[6].missing.contains(&'😀'), "emoji resolves to the controlled missing glyph");
}

/// A pane left untracked by a zero tracking budget still draws: every row emitted through the
/// renderer seam equals a cold draw from a tracked cache, glyphs, decorations and missing
/// characters alike, and nothing is cached for it.
#[test]
fn untracked_panes_draw_like_cold_rows() {
    let grid = text_grid(10, &["ab=>c", "│─┼█", "plain", "→é x"]);
    let mut untracked = GlyphRig::new(false);
    untracked.cache = sonicterm_text::row_glyph_cache::RowGlyphCache::with_budgets(
        sonicterm_text::row_glyph_cache::DEFAULT_PAYLOAD_BUDGET_BYTES,
        0,
    );
    untracked.begin(&grid);
    assert!(!untracked.cache.is_tracked(7), "a zero tracking budget leaves the pane untracked");
    let mut cold = GlyphRig::new(false);
    cold.begin(&grid);
    for slot in 0..4 {
        let drawn = untracked.emit(&grid, 0, slot);
        let reference = cold.emit(&grid, 0, slot);
        assert!(!drawn.replayed, "slot {slot}: an untracked pane never replays");
        assert!(!reference.glyphs.is_empty(), "slot {slot}: the reference draws glyphs");
        assert_eq!(drawn.glyph_bytes(), reference.glyph_bytes(), "slot {slot} glyph bytes");
        assert_eq!(drawn.decorations, reference.decorations, "slot {slot} decorations");
        assert_eq!(drawn.missing, reference.missing, "slot {slot} missing characters");
    }
    assert!(untracked.cache.is_empty() && untracked.cache.retained_amount().bytes == 0);
}

/// Warm and cold glyph lists of the scrolled software frame are byte-identical.
#[test]
fn warm_and_cold_scrolled_frames_emit_identical_glyphs() {
    let (_atlas, cold, warm, _size) = warm_and_cold_row_glyphs();
    assert!(!cold.is_empty());
    assert_eq!(bytemuck::cast_slice::<_, u8>(&warm), bytemuck::cast_slice::<_, u8>(&cold));
}
