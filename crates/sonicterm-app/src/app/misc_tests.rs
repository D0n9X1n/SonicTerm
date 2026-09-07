use super::*;
use sonicterm_gpu::{
    core::{build_snapped_cell_x, emit_cell_bg_quads_for_row},
    row_quad_cache::{row_quad_hash_cells, CachedRowQuads, LineQuadCache},
};
use sonicterm_text::row_glyph_cache::row_hash_cells;

/// Prompt navigation must move cached colored rows without relying on later PTY output or dirty invalidation.
#[test]
fn prompt_navigation_reprojects_overlapping_colored_history_without_dirty_rows() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("history");
    let parser = app.main().unwrap().panes[&pane_id].parser.clone();
    {
        let mut guard = parser.lock();
        *guard = Parser::new(Grid::new(4, 3));
        let mut input = Vec::new();
        for row in 0..15 {
            input.extend_from_slice(b"\x1b]133;A\x07\x1b[48;2;120;40;80m");
            input.extend_from_slice(format!("r{row:02}\x1b[0m\r\n").as_bytes());
        }
        guard.advance(&input);
        guard.grid_mut().clear_dirty();
        assert!(guard.grid().scrollback_len() > 10);
        assert_eq!(guard.grid().dirty_count(), 0);
    }
    app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap().viewport_top_abs = Some(10);
    let mut cache = LineQuadCache::new();
    cache.resize(6);
    let theme = Theme::default();
    let snapped = build_snapped_cell_x(0.0, 10.0, 4);
    let hash_at = |top, slot| {
        let guard = parser.lock();
        let row = guard.grid().row_at_abs(10).unwrap();
        row_quad_hash_cells(top, slot, row.iter(), 1, 10.0, 20.0, 0.0, 0.0, 40.0, 60.0, None)
    };
    let glyph_hash_at = |top, slot| {
        let guard = parser.lock();
        let row = guard.grid().row_at_abs(10).unwrap();
        row_hash_cells(top, slot, row.iter(), 1, 10.0, 20.0, 1.0, 0.0, 0.0, 40.0, 60.0, None)
    };
    let project = |top, slot| {
        let guard = parser.lock();
        let mut quads = Vec::new();
        emit_cell_bg_quads_for_row(
            guard.grid(),
            top,
            &theme,
            0.0,
            0.0,
            10.0,
            20.0,
            40.0,
            60.0,
            4,
            slot,
            &mut quads,
            &snapped,
        );
        quads
    };
    let first_key = hash_at(10, 0);
    let first_glyph_key = glyph_hash_at(10, 0);
    let first = project(10, 0);
    assert_eq!(first.len(), 1, "non-default background must emit visible geometry");
    cache.insert(pane_id, 10, first_key, CachedRowQuads { quads: first.clone() });
    cache.insert(pane_id + 1, 10, first_key, CachedRowQuads { quads: first.clone() });
    let revision = parser.lock().grid().revision();

    app.scroll_to_prompt(false);

    let top = app.main().unwrap().panes[&pane_id].viewport_top_abs.unwrap();
    assert_eq!(top, 9);
    assert_eq!(parser.lock().grid().revision(), revision);
    assert_eq!(parser.lock().grid().dirty_count(), 0);
    let shifted_key = hash_at(top, 1);
    let expected = project(top, 1);
    let replayed = cache
        .get(pane_id, 10, shifted_key)
        .map(|cached| cached.quads.clone())
        .unwrap_or_else(|| expected.clone());
    assert_ne!(glyph_hash_at(top, 1), first_glyph_key, "glyphs already track their viewport slot");
    assert_ne!(expected[0].rect, first[0].rect);
    assert_eq!(replayed[0].rect, expected[0].rect, "background must follow the same moved row");
    assert!(cache.get(pane_id + 1, 10, first_key).is_some());

    app.scroll_to_prompt(true);

    assert_eq!(app.main().unwrap().panes[&pane_id].viewport_top_abs, Some(10));
    assert_eq!(hash_at(10, 0), first_key);
    assert_eq!(parser.lock().grid().dirty_count(), 0);
}
