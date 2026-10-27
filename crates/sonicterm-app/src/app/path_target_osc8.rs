//! OSC 8 label extraction and bounded underline continuation.

use super::*;

/// The visible text of the hover fragments `cells`, read from `frame`.
pub(super) fn hyperlink_label(
    frame: &FrameRows<'_>,
    cells: &sonicterm_render_model::inputs::HoveredUrlCells,
) -> LinkLabel {
    use sonicterm_render_model::inputs::MAX_HOVERED_URL_SPANS;
    let spans = cells.spans();
    let last_row = frame.grid().rows.saturating_sub(1);
    let mut complete = spans.len() < MAX_HOVERED_URL_SPANS
        && spans.first().is_some_and(|span| span.row > 0)
        && spans.last().is_some_and(|span| span.row < last_row);
    let mut text = String::new();
    'spans: for span in spans {
        let Some(row) = frame.row(frame.view_top() + u64::from(span.row)) else {
            // When: a fragment's row is not in the frame, the label cannot be read whole.
            complete = false;
            break;
        };
        let width = usize::from(span.end_col - span.start_col);
        for cell in row.iter().skip(usize::from(span.start_col)).take(width) {
            if cell.flags.contains(sonicterm_types::CellFlags::WIDE_CONT) {
                // When: `WIDE_CONT` marks the right half of a wide `cell`, its text is already read.
                continue;
            }
            if text.len() + cell.ch.len_utf8() + cell.extras().map_or(0, str::len)
                > MAX_LINK_LABEL_BYTES
            {
                // When: the label passes `MAX_LINK_LABEL_BYTES`, the rest is unseen.
                complete = false;
                break 'spans;
            }
            text.push(cell.ch);
            text.extend(cell.extras());
        }
    }
    LinkLabel { text, complete }
}

pub(super) fn hyperlink_hover_cells(
    frame: &FrameRows<'_>,
    pane_id: u64,
    pointed_row: u16,
    col: u16,
    hyperlink_id: sonicterm_types::HyperlinkId,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan, MAX_HOVERED_URL_SPANS};

    let grid = frame.grid();
    let cells_at = move |row_number: u16| {
        let row = frame.row(frame.view_top().checked_add(u64::from(row_number))?)?;
        Some((row, row.iter().collect::<Vec<_>>()))
    };
    let fragment = |row_number: u16, column: u16| {
        let (row, cells) = cells_at(row_number)?;
        let column = usize::from(column);
        if cells.get(column)?.hyperlink() != Some(hyperlink_id) {
            // When: the pointed column has another hyperlink identity, it cannot continue this occurrence.
            return None;
        }
        let mut start = column;
        let mut end = column + 1;
        while start > 0 && cells[start - 1].hyperlink() == Some(hyperlink_id) {
            start -= 1;
        }
        while end < cells.len() && cells[end].hyperlink() == Some(hyperlink_id) {
            end += 1;
        }
        Some((
            HoveredUrlSpan {
                row: row_number,
                start_col: u16::try_from(start).ok()?,
                end_col: u16::try_from(end).ok()?,
            },
            row.soft_wrapped_from_previous(),
        ))
    };
    // A multiplexer positions each pane row with a cursor move, so its rows record no soft wrap, and
    // an app such as Claude Code wraps a long link itself with a hanging indent and a small right
    // margin. On the alternate screen a fragment continues into the next row's fragment of the same
    // link when, inside one pane, at most `LINK_CONTINUATION_MARGIN` blank cells follow the upper
    // fragment and only `LINK_CONTINUATION_INDENT` blank cells precede the lower one. Activation
    // opens the stored destination, not joined text, so this changes only which cells are underlined.
    let pane_continuation = grid.is_alt();
    // A margin or indent must be truly blank: a rule or border glyph between two fragments ends
    // the link's line even though it is not text.
    let is_blank = |cell: Option<&&Cell>| {
        cell.is_none_or(|cell| cell.ch == ' ' && !cell.flags.contains(CellFlags::WIDE_CONT))
    };
    let pane_predecessor = |lower: HoveredUrlSpan| {
        let upper_row = lower.row.checked_sub(1)?;
        let (_, upper) = cells_at(upper_row)?;
        let (_, below) = cells_at(lower.row)?;
        let (left, right) =
            shared_pane_columns(&upper, &below, grid.cols, lower.start_col, lower.end_col);
        if lower.start_col - left > LINK_CONTINUATION_INDENT
            || !(left..lower.start_col).all(|column| is_blank(below.get(usize::from(column))))
        {
            // When: `lower.start_col` follows non-blank cells or more than `LINK_CONTINUATION_INDENT` cells, it starts a line.
            return None;
        }
        let last_visible =
            (left..right).rev().find(|column| !is_blank(upper.get(usize::from(*column))))?;
        if right - last_visible - 1 > LINK_CONTINUATION_MARGIN {
            // When: more than `LINK_CONTINUATION_MARGIN` blank cells end the upper row, it did not wrap.
            return None;
        }
        fragment(upper_row, last_visible)
    };
    let pane_successor = |upper: HoveredUrlSpan| {
        let (_, above) = cells_at(upper.row)?;
        let (_, lower) = cells_at(upper.row + 1)?;
        let (left, right) =
            shared_pane_columns(&above, &lower, grid.cols, upper.start_col, upper.end_col);
        if right - upper.end_col > LINK_CONTINUATION_MARGIN
            || !(upper.end_col..right).all(|column| is_blank(above.get(usize::from(column))))
        {
            // When: non-blank cells or more than `LINK_CONTINUATION_MARGIN` cells follow `upper`, it did not wrap.
            return None;
        }
        let first_visible =
            (left..right).find(|column| !is_blank(lower.get(usize::from(*column))))?;
        if first_visible - left > LINK_CONTINUATION_INDENT {
            // When: `first_visible` lies past `LINK_CONTINUATION_INDENT`, the lower row is not a hanging indent.
            return None;
        }
        fragment(upper.row + 1, first_visible)
    };
    if pointed_row >= grid.rows {
        // When: pointed_row is outside the viewport, never project retained scrollback as visible geometry.
        return None;
    }
    let (pointed, mut incoming_wrap) = fragment(pointed_row, col)?;
    let mut spans = Vec::with_capacity(MAX_HOVERED_URL_SPANS);
    spans.push(pointed);
    // Start at the pointer so clipping an overlong occurrence cannot discard its pointed fragment.
    while spans.len() < MAX_HOVERED_URL_SPANS {
        let first = spans[0];
        if first.row == 0 {
            // When: first.row is the viewport's top row, no visible row above can continue this occurrence.
            break;
        }
        let soft_wrapped = (first.start_col == 0 && incoming_wrap)
            .then(|| fragment(first.row - 1, grid.cols.checked_sub(1)?))
            .flatten();
        let predecessor =
            soft_wrapped.or_else(|| pane_continuation.then_some(first).and_then(pane_predecessor));
        let Some((previous, wrap)) = predecessor else {
            // When: predecessor is None, no soft wrap or shared pane edge links the row above to this occurrence.
            break;
        };
        spans.insert(0, previous);
        incoming_wrap = wrap;
    }
    while spans.len() < MAX_HOVERED_URL_SPANS {
        let last = *spans.last()?;
        if last.row + 1 >= grid.rows {
            // When: last.row is the viewport's final row, no visible row below can continue this occurrence.
            break;
        }
        let soft_wrapped = (last.end_col == grid.cols)
            .then(|| fragment(last.row + 1, 0))
            .flatten()
            .filter(|(_, wrap)| *wrap);
        let successor =
            soft_wrapped.or_else(|| pane_continuation.then_some(last).and_then(pane_successor));
        let Some((next, _)) = successor else {
            // When: successor is None, no soft wrap or shared pane edge links the row below to this occurrence.
            break;
        };
        spans.push(next);
    }
    HoveredUrlCells::new(pane_id, spans, false)
}

#[cfg(test)]
#[path = "path_target_osc8_tests.rs"]
mod path_target_osc8_tests;
