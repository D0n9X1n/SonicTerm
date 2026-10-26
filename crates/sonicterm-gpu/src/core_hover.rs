//! Hover responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) fn pane_focus_flash_sample(elapsed: Duration) -> Option<(u8, f32)> {
    if elapsed >= PANE_FOCUS_FLASH_DURATION {
        // When: `elapsed` reaches the bounded lifetime, no flash frame remains.
        return None;
    }
    let bucket = ((elapsed.as_millis() / PANE_FOCUS_FLASH_BUCKET.as_millis()) + 1)
        .min(u128::from(u8::MAX)) as u8;
    let t = elapsed.as_secs_f32() / PANE_FOCUS_FLASH_DURATION.as_secs_f32();
    Some((bucket, (1.0 - t).powi(2) * 0.12))
}

pub(super) fn hovered_url_needs_accent(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
) -> bool {
    hovered.is_some_and(|h| h.active)
}

pub(super) fn hovered_url_for_pane_row(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pane_id: u64,
    row: u16,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    let hovered = hovered.filter(|hovered| hovered.pane_id == pane_id)?;
    let span = hovered.span_for_row(row)?;
    sonicterm_render_model::inputs::HoveredUrlCells::new(pane_id, [span], hovered.active)
}

/// The inclusive columns of `row`'s active (recoloring) hover fragment, folded into that row's
/// content key; `None` for an absent or hint-only hover, which leaves glyph colours unchanged.
pub(super) fn hovered_url_row_key_span(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    row: u16,
) -> Option<(u16, u16)> {
    let hovered = hovered.filter(|hovered| hovered.active)?;
    let span = hovered.span_for_row(row)?;
    Some((span.start_col, span.end_col))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn hovered_url_span_rect(
    span: sonicterm_render_model::inputs::HoveredUrlSpan,
    cols: u16,
    rows: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Option<(f32, f32, f32, f32)> {
    if cols == 0 || span.row >= rows || span.start_col >= cols || span.end_col <= span.start_col {
        // When: `span` has no visible row or column coverage, emit no underline rectangle.
        return None;
    }
    let start_col = span.start_col as usize;
    let end_col = span.end_col.min(cols) as usize;
    let x = snapped_cell_x
        .get(start_col)
        .copied()
        .unwrap_or(origin_x + f32::from(span.start_col) * cell_w);
    let width = snapped_cell_x
        .get(end_col)
        .map(|right| right - x)
        .unwrap_or_else(|| f32::from(span.end_col.min(cols) - span.start_col) * cell_w);
    (width > 0.0).then_some((x, origin_y + f32::from(span.row) * cell_h, width, cell_h))
}

#[cfg(test)]
#[path = "core_hover_tests.rs"]
mod core_hover_tests;
