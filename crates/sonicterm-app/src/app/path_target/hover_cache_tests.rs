use super::*;
use sonicterm_grid::grid::{CellFlags, Color};

/// A context for pane 1's cell (0, 0) on a live view with every setting at its default.
fn context() -> DetectionContext {
    DetectionContext {
        pane_id: 1,
        pointed: (0, 0),
        view_top: 0,
        rows_below: false,
        scrollback_evicted: 0,
        screen_epoch: 0,
        size_generation: 0,
        alt_screen: false,
        cwd_revision: 0,
        clickable_local_targets: true,
        clickable_bare_names: false,
        clickable_path_max_chars: 1024,
    }
}

/// Write `text` at the cursor.
fn write(grid: &mut Grid, text: &str) {
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A stored result is reused for the same context while its read rows keep their stamps; a
/// write to a row outside the span keeps it, and a write inside the span drops it.
#[test]
fn a_result_is_reused_until_a_row_in_its_read_span_changes() {
    let mut grid = Grid::new(10, 4);
    let mut cache = HoverDetectionCache::default();
    assert!(cache.lookup(&context(), &grid).is_none(), "an empty cache answers nothing");
    cache.store(context(), &grid, Some((0, 1)), None);
    assert_eq!(cache.lookup(&context(), &grid), Some(None), "a cached no-target is a hit");
    grid.cursor.row = 3;
    write(&mut grid, "x");
    assert_eq!(cache.lookup(&context(), &grid), Some(None), "row 3 is outside the span");
    grid.cursor.row = 1;
    write(&mut grid, "y");
    assert!(cache.lookup(&context(), &grid).is_none(), "row 1 is inside the span");
}

/// Any change to the context misses, one field at a time, and `clear` forgets the result.
#[test]
fn any_context_change_misses_and_clear_forgets() {
    let grid = Grid::new(10, 4);
    let mut cache = HoverDetectionCache::default();
    cache.store(context(), &grid, None, None);
    let changes: [fn(&mut DetectionContext); 12] = [
        |changed| changed.pane_id = 2,
        |changed| changed.pointed = (0, 1),
        |changed| changed.view_top = 1,
        |changed| changed.rows_below = true,
        |changed| changed.scrollback_evicted = 1,
        |changed| changed.screen_epoch = 1,
        |changed| changed.size_generation = 1,
        |changed| changed.alt_screen = true,
        |changed| changed.cwd_revision = 1,
        |changed| changed.clickable_local_targets = false,
        |changed| changed.clickable_bare_names = true,
        |changed| changed.clickable_path_max_chars = 10,
    ];
    for (index, change) in changes.iter().enumerate() {
        let mut changed = context();
        change(&mut changed);
        assert!(cache.lookup(&changed, &grid).is_none(), "change {index} misses");
    }
    assert!(cache.lookup(&context(), &grid).is_some());
    cache.clear();
    assert!(cache.is_empty() && cache.lookup(&context(), &grid).is_none());
}
