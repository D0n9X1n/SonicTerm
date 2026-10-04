use super::*;

use sonicterm_types::{retained_hash_table_bytes, ClassCoverage, ResourceClass};

fn rect(x: i32, y: i32, w: u32, h: u32) -> PixelRect {
    PixelRect { x, y, w, h }
}

fn ink(rect: PixelRect, abs_row: u64, content_seq: u64) -> RowInk {
    RowInk { rect, abs_row, content_seq: Some(content_seq) }
}

/// Stage one record per slot `0..rows` of `pane_id` and commit with `pane_id` the only survivor.
fn present_rows(table: &mut RowInkTable, pane_id: u64, rows: u16) {
    table.begin_frame();
    for slot in 0..rows {
        let top = i32::from(slot) * 20;
        table.stage(pane_id, slot, ink(rect(0, top, 100, 20), u64::from(slot), 1));
    }
    table.commit(&[(pane_id, rows)]);
}

/// Records describe presented pixels only: a frame that stages but never commits (a surface
/// retry, an atlas retry or an unavailable device) leaves the committed records as they were.
#[test]
fn staged_records_count_only_once_committed() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    table.stage(7, 0, ink(rect(0, 0, 100, 20), 20, 3));
    // The frame was not presented: the next frame begins without committing.
    table.begin_frame();
    assert_eq!(table.valid_rect(7, 0, 20, Some(3)), None, "an unpresented stage is discarded");
    table.stage(7, 0, ink(rect(0, 0, 100, 20), 20, 3));
    table.commit(&[(7, 4)]);
    assert_eq!(table.valid_rect(7, 0, 20, Some(3)), Some(rect(0, 0, 100, 20)));
    assert_eq!(table.len(), 1);
}

/// A row a partial frame did not emit keeps its pixels, so it keeps its record; an emitted
/// row's record is replaced, not unioned with what it drew before.
#[test]
fn a_non_emitted_row_keeps_its_record_and_an_emitted_row_replaces_it() {
    let mut table = RowInkTable::default();
    present_rows(&mut table, 7, 4);
    table.begin_frame();
    table.stage(7, 1, ink(rect(4, 22, 10, 8), 1, 2));
    table.commit(&[(7, 4)]);
    assert_eq!(table.valid_rect(7, 0, 0, Some(1)), Some(rect(0, 0, 100, 20)), "kept");
    assert_eq!(table.valid_rect(7, 1, 1, Some(2)), Some(rect(4, 22, 10, 8)), "replaced");
    assert_eq!(table.len(), 4);
}

/// Each emitted row is staged once, by the glyph loop; its background and decoration quads merge
/// their rectangles into that one record, which commits as the union. An empty rectangle (a loop
/// that drew nothing for the row) adds no area.
#[test]
fn one_staged_record_per_row_merges_its_background_and_decorations() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    let staged = table.stage(7, 2, ink(rect(10, 40, 5, 30), 2, 9));
    table.merge_staged(staged, rect(0, 44, 100, 20));
    table.merge_staged(staged, PixelRect { x: 500, y: 500, w: 0, h: 0 });
    assert_eq!(table.staged_len(), 1);
    table.commit(&[(7, 4)]);
    assert_eq!(table.valid_rect(7, 2, 2, Some(9)), Some(rect(0, 40, 100, 30)));
}

/// A heavily decorated row still holds one staging record: fifty underline runs merge into it,
/// so staging is bounded by the emitted rows, not by their decorations.
#[test]
fn a_decorated_row_stays_one_staged_record() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    let staged = table.stage(7, 0, ink(rect(0, 0, 100, 20), 0, 1));
    for run in 0..50 {
        table.merge_staged(staged, rect(run * 2, 18, 2, 3));
    }
    assert_eq!(table.staged_len(), 1);
    table.commit(&[(7, 1)]);
    assert_eq!(table.valid_rect(7, 0, 0, Some(1)), Some(rect(0, 0, 100, 21)));
}

/// Staging capacity follows the rows a frame can stage, not the empty length after a commit:
/// repeated identical 200-row frames keep one staging allocation, and a narrow 5-row frame in
/// between keeps it too, so neither a full nor a partial frame regrows it.
#[test]
fn staging_capacity_is_reused_across_frames() {
    let mut table = RowInkTable::default();
    present_rows(&mut table, 7, 200);
    let capacity = table.staged_capacity();
    assert!(capacity >= 200, "staging keeps room for the rows it staged: {capacity}");
    for _ in 0..3 {
        present_rows(&mut table, 7, 200);
        assert_eq!(table.staged_capacity(), capacity);
        table.begin_frame();
        table.stage(7, 5, ink(rect(0, 100, 100, 20), 5, 2));
        table.commit(&[(7, 200)]);
        assert_eq!(table.staged_capacity(), capacity, "a narrow frame keeps the allocation");
    }
}

/// A row that emitted nothing has a valid, empty record, which is not a missing record.
#[test]
fn an_empty_row_has_an_empty_record_rather_than_none() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    table.stage(
        7,
        0,
        RowInk { rect: PixelRect { x: 0, y: 0, w: 0, h: 0 }, abs_row: 30, content_seq: None },
    );
    table.commit(&[(7, 1)]);
    let record = table.valid_rect(7, 0, 30, None).expect("an empty row is recorded");
    assert!(record.is_empty());
}

/// A record is trusted only for the content it describes: a slot that now shows another
/// absolute row, or whose row's content stamp moved, has no valid record, and a slot never
/// presented has none either.
#[test]
fn a_changed_content_stamp_or_absolute_row_invalidates_a_record() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    table.stage(7, 3, ink(rect(0, 60, 100, 20), 23, 5));
    table.commit(&[(7, 4)]);
    assert!(table.valid_rect(7, 3, 23, Some(5)).is_some());
    assert_eq!(table.valid_rect(7, 3, 23, Some(6)), None, "content stamp changed");
    assert_eq!(table.valid_rect(7, 3, 24, Some(5)), None, "slot shows another absolute row");
    assert_eq!(table.valid_rect(7, 3, 23, None), None, "the row is no longer held");
    assert_eq!(table.valid_rect(7, 2, 22, Some(5)), None, "never presented");
    assert_eq!(table.valid_rect(9, 3, 23, Some(5)), None, "another pane");
}

/// Records of a pane that left the plan are pruned at commit, and closing a pane drops its
/// records at once; slots at or past a surviving pane's row count are pruned too.
#[test]
fn records_of_closed_panes_and_vanished_slots_are_dropped() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    for (pane_id, slot) in [(7, 0), (7, 3), (9, 0)] {
        table.stage(pane_id, slot, ink(rect(0, 0, 1, 1), u64::from(slot), 1));
    }
    table.commit(&[(7, 4), (9, 1)]);
    assert_eq!(table.len(), 3);
    // Pane 9 closed and pane 7 shrank to three rows.
    table.begin_frame();
    table.commit(&[(7, 3)]);
    assert_eq!(table.len(), 1);
    assert!(table.valid_rect(7, 0, 0, Some(1)).is_some());
    table.drop_pane(7);
    assert_eq!(table.len(), 0);
}

/// A pane grown to 2,000 rows and shrunk back to 40 releases the table: after the shrinking
/// frame commits, 40 records remain, the capacity falls to at most 128 and the reported bytes
/// fall with it.
#[test]
fn the_table_releases_capacity_after_a_pane_shrinks() {
    let mut table = RowInkTable::default();
    present_rows(&mut table, 7, 2_000);
    let grown = table.retained_amount();
    assert_eq!(grown.items, 2_000);
    present_rows(&mut table, 7, 40);
    assert_eq!(table.len(), 40);
    assert!(table.capacity() <= 128, "capacity {} after shrinking", table.capacity());
    let shrunk = table.retained_amount();
    assert_eq!(shrunk.items, 40);
    assert!(shrunk.bytes * 10 < grown.bytes, "{} -> {} bytes", grown.bytes, shrunk.bytes);
}

/// The reported bytes count allocated buckets, not usable capacity: a capacity of 112 is 128
/// buckets of entries plus a control byte each and the 16-byte trailing group, plus the staging
/// buffer's capacity in entries. A 2,000-row frame stays inside the class's coverage envelope,
/// which is computed with the same helper.
#[test]
fn reported_bytes_count_allocated_buckets_and_staging() {
    let mut table = RowInkTable::default();
    present_rows(&mut table, 7, 2_000);
    present_rows(&mut table, 7, 40);
    assert_eq!(table.capacity(), 112);
    let entry = std::mem::size_of::<(RowInkKey, RowInk)>();
    let table_bytes = retained_hash_table_bytes::<RowInkKey, RowInk>(112);
    assert_eq!(table_bytes, 128 * entry + 128 + 16);
    assert_ne!(table_bytes, 112 * entry);
    assert_eq!(table.retained_amount().bytes, table_bytes + table.staged_capacity() * entry);

    let visible = sonicterm_render_model::boundary::grid::grid::MAX_VISIBLE_GRID_CELLS as usize;
    let envelope = retained_hash_table_bytes::<RowInkKey, RowInk>(2 * visible) + visible * entry;
    let ClassCoverage::UnchargedRetention { per_owner_bytes } = ResourceClass::RowInk.coverage()
    else {
        panic!("RowInk must record its uncharged envelope");
    };
    assert!(per_owner_bytes >= envelope, "tabled {per_owner_bytes} < envelope {envelope}");
    present_rows(&mut table, 7, 2_000);
    assert!(table.retained_amount().bytes <= envelope);
}

/// Ink edges union every finite rectangle and round outward; a non-finite rectangle makes the
/// row's ink the whole surface, and a row with no rectangle has empty ink.
#[test]
fn ink_edges_round_outward_and_make_non_finite_ink_the_surface() {
    let surface = rect(0, 0, 240, 160);
    let mut edges = InkEdges::default();
    assert!(edges.to_rect(surface).is_empty());
    edges.add_px((10.5, 20.25, 4.0, 3.0));
    edges.add_px((2.0, 30.0, 1.0, 1.5));
    assert_eq!(edges.to_rect(surface), rect(2, 20, 13, 12));
    edges.add_px((f32::NAN, 0.0, 1.0, 1.0));
    assert_eq!(edges.to_rect(surface), surface);
}

/// The planner receives a record only while it describes the slot's current content: a staged
/// row records the absolute row and content stamp the grid shows, an edit to one row invalidates
/// only that slot, a history row keeps its record, and a view whose top moved shows other
/// absolute rows, so none of its old records apply.
#[test]
fn valid_records_follow_the_grids_rows_and_content_stamps() {
    use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
    let mut grid = Grid::new(4, 3);
    let strips = [rect(0, 0, 40, 20), rect(0, 20, 40, 20), rect(0, 40, 40, 20)];
    let mut table = RowInkTable::default();
    table.begin_frame();
    for (slot, strip) in (0..3).zip(strips) {
        table.stage_row(7, slot, &grid, 0, strip);
    }
    table.commit(&[(7, 3)]);
    assert_eq!(table.valid_records(7, &grid, 0, 3), strips.map(Some));
    assert_eq!(table.valid_records(9, &grid, 0, 3), [None; 3], "another pane has none");

    grid.goto(1, 0);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    assert_eq!(table.valid_records(7, &grid, 0, 3), [Some(strips[0]), None, Some(strips[2])]);

    grid.scroll_up(1);
    assert_eq!(table.valid_records(7, &grid, 0, 3)[0], Some(strips[0]), "history keeps its stamp");
    assert_eq!(table.valid_records(7, &grid, 1, 3), [None; 3], "the view top moved");
}

/// A test reads one slot's committed record as it stands, whatever content it describes; staged
/// records are not visible until committed, and a dropped pane has none.
#[test]
fn committed_rect_reads_the_committed_record_only() {
    let mut table = RowInkTable::default();
    table.begin_frame();
    table.stage(7, 2, ink(rect(0, 40, 100, 20), 2, 5));
    assert_eq!(table.committed_rect(7, 2), None, "a staged record is not committed");
    table.commit(&[(7, 4)]);
    assert_eq!(table.committed_rect(7, 2), Some(rect(0, 40, 100, 20)));
    assert_eq!(table.committed_rect(7, 1), None);
    table.drop_pane(7);
    assert_eq!(table.committed_rect(7, 2), None);
}

/// One emitted row can push more than one glyph span (a row glyph a test attaches after the
/// row's own glyphs); its record is the union of every span's ink and its tofu, never only the
/// first span's, so the record bounds every glyph the row drew. A span with no ink adds nothing.
#[test]
fn an_emitted_rows_ink_unions_every_span_and_its_tofu() {
    use crate::cursor::RowGlyphSpan;
    let surface = rect(0, 0, 240, 160);
    let spans = [
        RowGlyphSpan { glyphs: 0..2, ink_px: Some([10.0, 20.0, 30.0, 40.0]) },
        RowGlyphSpan { glyphs: 2..2, ink_px: None },
        RowGlyphSpan { glyphs: 2..3, ink_px: Some([50.0, 5.0, 60.0, 25.0]) },
    ];
    let ink = emitted_row_ink(&spans, [(0.0, 30.0, 4.0, 4.0)]);
    assert_eq!(ink.to_rect(surface), rect(0, 5, 60, 35));
    assert!(emitted_row_ink(&[], []).to_rect(surface).is_empty());
}
