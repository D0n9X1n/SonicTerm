//! Pane edge, recorded wrap and inferred continuation boundaries.

use super::*;

pub(super) fn is_pane_border(cell: Option<&Cell>) -> bool {
    cell.is_some_and(|cell| cell.hyperlink().is_none() && PANE_BORDER_GLYPHS.contains(&cell.ch))
}

/// Return the columns `[left, right)` of the pane holding columns `start..end` of two adjacent rows.
///
/// Pane edges are the grid's edges and the borders both rows draw in the same column. A border in
/// only one row is text, so it cannot narrow the pane.
pub(super) fn shared_pane_columns(
    upper: &[&Cell],
    lower: &[&Cell],
    cols: u16,
    start: u16,
    end: u16,
) -> (u16, u16) {
    let shared = |column: &u16| {
        let index = usize::from(*column);
        is_pane_border(upper.get(index).copied()) && is_pane_border(lower.get(index).copied())
    };
    let left = (0..start).rev().find(shared).map_or(0, |border| border + 1);
    let right = (end..cols).find(shared).unwrap_or(cols);
    (left, right)
}

/// Report whether `cell` shows text: any visible cell except a blank or box drawing, which
/// multiplexers use for pane borders and rules. The trailing half of a wide character is text.
pub(super) fn is_text_cell(cell: Option<&Cell>) -> bool {
    cell.is_some_and(|cell| {
        cell.flags.contains(CellFlags::WIDE_CONT)
            || (cell.ch != ' ' && !('\u{2500}'..='\u{257F}').contains(&cell.ch))
    })
}

/// Return the visible spans of the whitespace-free text run that holds the `pointed` cell.
///
/// The run stops at blank cells, box drawing and the ends of the logical line, so a pane border
/// ends it. `cells` and `positions` hold the scanned cells of one logical line, one per cell.
pub(super) fn pointed_text_spans(
    cells: &[&Cell],
    positions: &[AbsoluteCell],
    pointed: usize,
) -> SmallVec<[AbsoluteCellSpan; 2]> {
    let mut spans = SmallVec::new();
    if !is_text_cell(cells.get(pointed).copied()) {
        // When: is_text_cell rejects the pointed cell, it is blank or a border and starts no run.
        return spans;
    }
    let mut start = pointed;
    while start > 0 && is_text_cell(cells.get(start - 1).copied()) {
        start -= 1;
    }
    let mut end = pointed + 1;
    while is_text_cell(cells.get(end).copied()) {
        end += 1;
    }
    for position in positions.get(start..end).unwrap_or_default() {
        match spans.last_mut() {
            Some(span) if span.row == position.row => {
                span.end_col = position.col.saturating_add(1);
            }
            _ => spans.push(AbsoluteCellSpan {
                row: position.row,
                start_col: position.col,
                end_col: position.col.saturating_add(1),
            }),
        }
    }
    spans
}

/// Report whether `column` of absolute `row` is a pane border: a vertical box-drawing glyph that
/// the displayed row above or below also draws in the same column. A neighbour outside the frame
/// is never read, so at a frame edge only the displayed neighbour counts.
pub(super) fn pane_border_at(frame: &FrameRows<'_>, row: u64, column: u16) -> bool {
    let border_in = |row_number: u64| {
        is_pane_border(
            frame.row(row_number).and_then(|found_row| found_row.get(usize::from(column))),
        )
    };
    border_in(row)
        && (row.checked_sub(1).filter(|above| *above >= frame.view_top()).is_some_and(border_in)
            || row.checked_add(1).filter(|below| frame.contains(*below)).is_some_and(border_in))
}

/// Report whether consecutive hard-wrap `spans` continue across a vertical pane border: on the
/// alternate screen, a border that both rows draw between where the lower fragment starts and
/// where the upper one starts puts the next row's text in another pane.
pub(super) fn spans_join_across_pane_border(
    frame: &FrameRows<'_>,
    spans: &[AbsoluteCellSpan],
) -> bool {
    frame.grid().is_alt()
        && spans.iter().zip(spans.iter().skip(1)).any(|(upper, lower)| {
            upper.row.checked_add(1) == Some(lower.row)
                && (lower.start_col..upper.start_col).any(|column| {
                    pane_border_at(frame, upper.row, column)
                        && pane_border_at(frame, lower.row, column)
                })
        })
}

/// Return the columns `[left, right)` of the pane that holds `column` of absolute `row`: the cells
/// between the nearest pane borders on either side, or the grid's edges where there is none.
pub(super) fn pane_columns_at(frame: &FrameRows<'_>, row: u64, column: u16) -> (u16, u16) {
    let cols = frame.grid().cols;
    let border = |edge_column: &u16| pane_border_at(frame, row, *edge_column);
    let left = (0..column).rev().find(border).map_or(0, |edge_column| edge_column + 1);
    let right = (column..cols).find(border).unwrap_or(cols);
    (left, right)
}

/// Apply the row cap to one chain step: `Err` fails the scan closed, `Ok(None)` stops growing in
/// that direction, and `Ok(Some(segment))` adds the row. A recorded wrap past the cap leaves the
/// line unproven, while an inferred continuation only stops there.
pub(super) fn admit_chain_step(
    step: ChainStep,
    chain_rows: usize,
) -> Result<Option<PaneRowSegment>, ()> {
    match step {
        ChainStep::Incomplete => Err(()),
        ChainStep::End => Ok(None),
        ChainStep::Recorded(_) if chain_rows >= MAX_WRAPPED_PATH_ROWS => Err(()),
        ChainStep::Inferred(_) if chain_rows >= MAX_WRAPPED_PATH_ROWS => Ok(None),
        ChainStep::Recorded(segment) | ChainStep::Inferred(segment) => Ok(Some(segment)),
    }
}

/// Report whether the wrap a terminal recorded from absolute `upper_row` into the next row
/// continues one pane's text.
///
/// A terminal records a wrap only at the grid's edge, so the wrap joins the pane that reaches the
/// right edge on `upper_row` to the pane that starts at the left edge on the next row. A split
/// draws its border down both rows, so those are two panes unless either row spans its far edge.
/// The primary screen shows one program, so every wrap there continues its text.
pub(super) fn wrap_joins_one_pane(frame: &FrameRows<'_>, upper_row: u64) -> bool {
    let grid = frame.grid();
    if !grid.is_alt() {
        // When: not grid.is_alt(), one program owns every row, so a recorded wrap continues its text.
        return true;
    }
    let Some(lower_row) = upper_row.checked_add(1) else {
        // When: `upper_row` has no successor, no wrap leaves it.
        return false;
    };
    let last_column = grid.cols.saturating_sub(1);
    if pane_border_at(frame, upper_row, last_column) || pane_border_at(frame, lower_row, 0) {
        // When: a pane border fills `upper_row`'s last column or `lower_row`'s first, no text crosses the wrap.
        return false;
    }
    let (upper_left, _) = pane_columns_at(frame, upper_row, last_column);
    let (_, lower_right) = pane_columns_at(frame, lower_row, 0);
    upper_left == 0 || lower_right == grid.cols
}

/// Return the next visible row's segment of `upper`'s pane when `upper`'s text fills the pane's
/// last column and that segment starts with text, so a multiplexer's unrecorded pane-edge wrap
/// continues one logical line. Mismatched pane edges or a blank start end the line.
pub(super) fn inferred_continuation_below(
    frame: &FrameRows<'_>,
    upper: PaneRowSegment,
) -> Option<PaneRowSegment> {
    let grid = frame.grid();
    let lower_row = upper.row.checked_add(1).filter(|row| *row < frame.view_end())?;
    if (upper.left == 0 || upper.right == grid.cols)
        && frame.row(lower_row)?.soft_wrapped_from_previous()
        && !wrap_joins_one_pane(frame, upper.row)
    {
        // When: `upper` touches a grid edge and `wrap_joins_one_pane` fails, the recorded wrap took that text elsewhere.
        return None;
    }
    let cell_at = |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let last_column = upper.right.checked_sub(1)?;
    if !is_text_cell(cell_at(upper.row, last_column))
        || !is_text_cell(cell_at(lower_row, upper.left))
    {
        // When: `is_text_cell` rejects `last_column` or `lower_row`'s first pane cell, no text crosses the edge.
        return None;
    }
    let (left, right) = pane_columns_at(frame, lower_row, upper.left);
    (left == upper.left && right == upper.right).then_some(PaneRowSegment {
        row: lower_row,
        left,
        right,
    })
}

/// Return the previous visible row's segment of `lower`'s pane when it fills the pane's last
/// column and `lower` starts with text: the mirror of [`inferred_continuation_below`].
pub(super) fn inferred_continuation_above(
    frame: &FrameRows<'_>,
    lower: PaneRowSegment,
) -> Option<PaneRowSegment> {
    let grid = frame.grid();
    let upper_row = lower.row.checked_sub(1).filter(|row| *row >= frame.view_top())?;
    if (lower.left == 0 || lower.right == grid.cols)
        && frame.row(lower.row)?.soft_wrapped_from_previous()
        && !wrap_joins_one_pane(frame, upper_row)
    {
        // When: `lower` touches a grid edge and `wrap_joins_one_pane` fails, the recorded wrap joins other panes.
        return None;
    }
    let cell_at = |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let last_column = lower.right.checked_sub(1)?;
    if !is_text_cell(cell_at(lower.row, lower.left))
        || !is_text_cell(cell_at(upper_row, last_column))
    {
        // When: `is_text_cell` rejects `lower`'s first cell or `upper_row`'s `last_column`, no text crosses the edge.
        return None;
    }
    let (left, right) = pane_columns_at(frame, upper_row, lower.left);
    (left == lower.left && right == lower.right).then_some(PaneRowSegment {
        row: upper_row,
        left,
        right,
    })
}

/// Return the OSC 7 directory that relative and contextual text resolves against.
///
/// On the alternate screen a full-screen program such as tmux or rmux can show several panes while
/// it relays only the active pane's directory, and the screen cannot show which pane holds the
/// text: pane borders may be box drawing, ASCII or blank, or look like a program's own rule. So
/// such text gets no directory there. The primary screen shows the reporting shell's own output.
pub(super) fn relative_text_cwd(parser: &sonicterm_vt::vt::Parser) -> Option<Osc7Cwd> {
    if parser.grid().is_alt() {
        // When: parser.grid().is_alt(), the reported directory may belong to another pane, so none applies.
        return None;
    }
    parser.osc7_cwd().cloned()
}

/// Report whether plain-text `spans` touch a pane edge that a multiplexer may have cut.
///
/// On the alternate screen a multiplexer positions every pane row with a cursor move, so text
/// that reaches its pane's right edge may continue on the next row, and text that starts at the
/// left edge under a row that filled the pane may be the rest of a longer target. Neither end can
/// be proven from the grid, so the caller refuses such text instead of offering a cut-off prefix.
pub(super) fn spans_reach_cut_pane_edge(
    frame: &FrameRows<'_>,
    spans: &[AbsoluteCellSpan],
    chain_rows: &[u64],
) -> bool {
    let (grid, view_top) = (frame.grid(), frame.view_top());
    let cell_at = move |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let border = |row: u64, column: u16| pane_border_at(frame, row, column);
    let text = |row: u64, column: u16| is_text_cell(cell_at(row, column));
    let (Some(head), Some(tail)) = (spans.first(), spans.last()) else {
        // When: spans is empty, the target covers no cell and so touches no pane edge.
        return false;
    };
    // `chain_rows` holds every row the scan joined, by a recorded wrap or an inferred pane-edge
    // continuation, so text continues past an edge exactly when the next chain row exists.
    let tail_pane_right =
        (tail.end_col..grid.cols).find(|column| border(tail.row, *column)).unwrap_or(grid.cols);
    let wrapped_below = chain_rows.contains(&(tail.row + 1));
    let tail_cut =
        !wrapped_below && (tail.end_col..tail_pane_right).all(|column| text(tail.row, column));
    let head_pane_left = (0..head.start_col)
        .rev()
        .find(|column| border(head.row, *column))
        .map_or(0, |column| column + 1);
    let wrapped_above = head.row.checked_sub(1).is_some_and(|above| chain_rows.contains(&above));
    let starts_line = head.row > view_top && !wrapped_above;
    let head_pane_right =
        (head.end_col..grid.cols).find(|column| border(head.row, *column)).unwrap_or(grid.cols);
    let head_cut = starts_line
        && (head_pane_left..head.start_col).all(|column| text(head.row, column))
        && head
            .row
            .checked_sub(1)
            .zip(head_pane_right.checked_sub(1))
            .is_some_and(|(above, column)| text(above, column));
    tail_cut || head_cut
}

#[cfg(test)]
#[path = "path_target_pane_spans_tests.rs"]
mod path_target_pane_spans_tests;
