//! Pure expected-damage geometry shared by this integration target.

use super::*;

/// `(x, y, w, h)` in surface pixels rounded outward to whole pixels, as the renderer bounds ink.
pub(super) fn outward((left, top, width, height): (f32, f32, f32, f32)) -> PixelRect {
    let (x, y) = (left.floor() as i32, top.floor() as i32);
    let (right, bottom) = ((left + width).ceil() as i32, (top + height).ceil() as i32);
    PixelRect { x, y, w: (right - x).max(0) as u32, h: (bottom - y).max(0) as u32 }
}

/// The rows a one-row edit at `edit_row` must emit, from the geometry alone: the edited row, every
/// row whose strip meets the edited row's strip with positive area, and every row whose `record`
/// meets that strip.
pub(super) fn edit_rows(
    layout: &Layout,
    edit_row: u16,
    record: impl Fn(u16) -> Option<PixelRect>,
) -> Vec<u16> {
    let Some(damage) = layout.strip_rect(edit_row) else {
        // When: the edited row's strip is off the pane, it damages nothing but is still emitted.
        return vec![edit_row];
    };
    let meets = |rect: Option<PixelRect>| rect.is_some_and(|rect| rect.intersect(damage).is_some());
    (0..layout.rows)
        .filter(|row| *row == edit_row || meets(layout.strip_rect(*row)) || meets(record(*row)))
        .collect()
}

/// The nearest row above `cursor_row` that showing the cursor skips while the recolored `glyph`
/// reaches it: its strip and its `record` both miss the cursor row's strip, and its record meets
/// `glyph`. `None` when no row qualifies.
pub(super) fn skipped_reached_row(
    layout: &Layout,
    cursor_row: u16,
    glyph: PixelRect,
    record: impl Fn(u16) -> Option<PixelRect>,
) -> Option<u16> {
    let damage = layout.strip_rect(cursor_row)?;
    let misses = |rect: PixelRect| rect.intersect(damage).is_none();
    (0..cursor_row).rev().find(|row| {
        layout.strip_rect(*row).is_none_or(misses)
            && record(*row).is_some_and(|ink| misses(ink) && ink.intersect(glyph).is_some())
    })
}
