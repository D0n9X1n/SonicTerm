//! Logical-row text mapping and shared candidate detection.

use super::*;

#[cfg(test)]
pub(super) fn row_target_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    lookup: impl FnOnce(&str, usize, PathStyle) -> Option<TargetMatch>,
) -> Option<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    cells.get(col)?;
    let (token_start, token_end) = token_bounds(&cells, col);
    if cells[token_start..token_end].iter().any(unsafe_path_cell) {
        // When: any cell in the token is wide or combining, reject the whole token rather than exposing an ASCII suffix.
        return None;
    }

    let mut text = String::with_capacity(cells.len());
    let mut byte_ranges = Vec::with_capacity(cells.len());
    for cell in cells {
        let start = text.len();
        let character =
            // When: `cell.flags` contains `WIDE_CONT`, preserve its column with a non-path sentinel; otherwise keep the cell character.
            if cell.flags.contains(CellFlags::WIDE_CONT) { '\u{fdd0}' } else { cell.ch };
        text.push(character);
        byte_ranges.push((start, text.len()));
    }
    let matched = lookup(&text, col, style)?;
    let start_col = byte_ranges.iter().position(|(start, _)| *start == matched.start)?;
    let end_col =
        byte_ranges.iter().position(|(start, _)| *start >= matched.end).unwrap_or(row.len());
    if start_col < token_start || end_col > token_end {
        // When: the detected span escapes `token_start..token_end`, reject cross-token reconstruction.
        return None;
    }
    let start_col = u16::try_from(start_col).ok()?;
    let end_col = u16::try_from(end_col).ok()?;
    Some(RowTarget { matched, start_col, end_col })
}

#[cfg(test)]
pub(in crate::app) fn target_at_row_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, |text, col, style| {
        let clicked_byte = text.char_indices().nth(col)?.0;
        find_targets_for_style(text, style)
            .into_iter()
            .find(|matched| clicked_byte >= matched.start && clicked_byte < matched.end)
    })
}

#[cfg(test)]
pub(in crate::app) fn bare_target_at_row_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, bare_name_at_char_col_for_style)
}

impl PathCellText {
    pub(super) fn from_cells(cells: &[&Cell], positions: &[AbsoluteCell]) -> Option<Self> {
        let mut text = String::new();
        let mut scalars = Vec::new();
        let mut index = 0;
        while index < cells.len() {
            let cell = cells[index];
            let wide = cell.flags.contains(CellFlags::WIDE);
            let continuation = cell.flags.contains(CellFlags::WIDE_CONT);
            let paired = wide
                && !continuation
                && cells.get(index + 1).is_some_and(|next| {
                    next.flags.contains(CellFlags::WIDE_CONT)
                        && !next.flags.contains(CellFlags::WIDE)
                        && positions[index].row == positions[index + 1].row
                        && positions[index].col.checked_add(1) == Some(positions[index + 1].col)
                });
            let end = index + if paired { 2 } else { 1 };
            let byte_start = text.len();
            // When: continuation is orphaned, keep an invalid non-delimiter scalar so its neighbors cannot join across it.
            let character = if continuation { '\u{fdd0}' } else { cell.ch };
            text.push(character);
            if text.len() > MAX_LOGICAL_LINE_BYTES {
                // When: text exceeds MAX_LOGICAL_LINE_BYTES, refuse before any candidate enumeration.
                return None;
            }
            scalars.push(PathScalar {
                byte_start,
                byte_end: text.len(),
                cell_start: index,
                cell_end: end,
                valid: !continuation && (!wide || paired),
            });
            index = end;
        }
        Some(Self { text, scalars })
    }

    /// The scalar starting at byte `byte`. Scalars are in strictly increasing byte order, so a
    /// binary search finds the one match a linear scan would.
    pub(super) fn scalar_starting_at(&self, byte: usize) -> Option<usize> {
        let found = self.scalars.binary_search_by_key(&byte, |scalar| scalar.byte_start).ok();
        #[cfg(test)]
        assert_eq!(found, self.scalars.iter().position(|scalar| scalar.byte_start == byte));
        found
    }

    /// The scalar ending at byte `byte`, by binary search as [`Self::scalar_starting_at`].
    pub(super) fn scalar_ending_at(&self, byte: usize) -> Option<usize> {
        let found = self.scalars.binary_search_by_key(&byte, |scalar| scalar.byte_end).ok();
        #[cfg(test)]
        assert_eq!(found, self.scalars.iter().position(|scalar| scalar.byte_end == byte));
        found
    }

    /// The scalar whose cells hold cell `cell`. Scalars cover contiguous, increasing cell ranges.
    pub(super) fn scalar_covering_cell(&self, cell: usize) -> Option<usize> {
        let index = self.scalars.partition_point(|scalar| scalar.cell_end <= cell);
        let found =
            self.scalars.get(index).filter(|scalar| scalar.cell_start <= cell).map(|_| index);
        #[cfg(test)]
        assert_eq!(
            found,
            self.scalars
                .iter()
                .position(|scalar| (scalar.cell_start..scalar.cell_end).contains(&cell))
        );
        found
    }

    pub(super) fn candidates(
        &self,
        cells: &[&Cell],
        pointed: usize,
        style: PathStyle,
        include_bare_names: bool,
    ) -> Vec<(TargetMatch, std::ops::Range<usize>)> {
        let Some(col) = self.scalar_covering_cell(pointed) else {
            // When: no scalar contains pointed, it cannot own a scanner target.
            return Vec::new();
        };
        target_candidates_at_char_col_for_style(&self.text, col, style, include_bare_names)
            .into_iter()
            .filter_map(|matched| {
                let start = self.scalar_starting_at(matched.start)?;
                let end = self.scalar_ending_at(matched.end)? + 1;
                let source_start = self.scalar_starting_at(matched.source_start)?;
                let source_end = self.scalar_ending_at(matched.source_end)? + 1;
                if source_start > start || source_end < end || start >= end {
                    // When: source_start/source_end fail to enclose start..end, no identity can authorize the match.
                    return None;
                }
                for scalar in &self.scalars[source_start..source_end] {
                    if !scalar.valid
                        || cells[scalar.cell_start..scalar.cell_end].iter().any(|cell| {
                            cell.hyperlink().is_some()
                                || cell.extras().is_some_and(|extras| !extras.is_empty())
                                || cell.ch.is_control()
                        })
                    {
                        // When: scalar identity is unsafe, neither removed boundaries nor display text may be discarded to recover a target.
                        return None;
                    }
                }
                let range = self.scalars[start].cell_start..self.scalars[end - 1].cell_end;
                range.contains(&pointed).then_some((matched, range))
            })
            .collect()
    }
}

#[cfg(test)]
pub(super) fn logical_path_scan_at_cell(
    grid: &Grid,
    view_top: u64,
    pointed: AbsoluteCell,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<LogicalPathScan> {
    logical_path_scan_in(&FrameRows::new(grid, view_top), pointed, style, include_bare_names)
}

pub(super) fn logical_path_scan_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<LogicalPathScan> {
    let grid = frame.grid();
    let (view_top, view_end) = (frame.view_top(), frame.view_end());
    if !frame.contains(pointed.row) || pointed.col >= grid.cols {
        // When: `pointed` lies outside the visible grid, no complete visible logical line owns it.
        return None;
    }

    // On the alternate screen a multiplexer can draw several panes on one row, and a terminal
    // records a wrap only at the grid's edge. So each row adds only its pane's columns, a wrap
    // continues text only within one pane (`wrap_joins_one_pane`), and a pane border ends every
    // name as the grid's edge does. The primary screen reads whole rows.
    let alt_screen = grid.is_alt();
    if alt_screen && pane_border_at(frame, pointed.row, pointed.col) {
        // When: alt_screen and the pointed cell is itself a pane border, it belongs to no pane's text.
        return None;
    }
    let segment_at = |row: u64, column: u16| {
        let (left, right) = if alt_screen {
            pane_columns_at(frame, row, column)
        } else {
            // When: not alt_screen, one program owns the whole row, so its segment spans every column.
            (0, grid.cols)
        };
        PaneRowSegment { row, left, right }
    };
    let pointed_segment = segment_at(pointed.row, pointed.col);
    // Each step adds one row by a recorded wrap or, on the alternate screen, by an inferred
    // pane-edge continuation. A recorded wrap whose partner row is out of view or past the cap
    // leaves the line unproven, so the scan fails closed; an inferred continuation only stops.
    // Every row a step reaches is checked for its own recorded wrap on the next step, so an
    // inferred join never hides an incomplete chain.
    let step_up = |first: PaneRowSegment| -> ChainStep {
        let Some(row) = frame.row(first.row) else {
            // When: `first.row` is no longer retained, nothing proves where its text began.
            return ChainStep::Incomplete;
        };
        let recorded = first.left == 0 && row.soft_wrapped_from_previous();
        if recorded && (first.row == 0 || first.row == view_top) {
            // When: `recorded` and the row that wrapped into `first.row` is out of view, the chain is unproven.
            return ChainStep::Incomplete;
        }
        if recorded && !wrap_joins_one_pane(frame, first.row - 1) {
            // When: `recorded` but `wrap_joins_one_pane` fails, the row above ends another pane's text.
            return ChainStep::End;
        }
        if recorded {
            // When: `recorded`, the terminal proved the row above continues into `first`.
            return ChainStep::Recorded(segment_at(first.row - 1, grid.cols.saturating_sub(1)));
        }
        if !alt_screen {
            // When: not `alt_screen`, only recorded wraps join rows.
            return ChainStep::End;
        }
        inferred_continuation_above(frame, first).map_or(ChainStep::End, ChainStep::Inferred)
    };
    let step_down = |last: PaneRowSegment| -> ChainStep {
        let Some(next_row_number) = last.row.checked_add(1) else {
            // When: `last.row` has no successor, the logical line ends at `last`.
            return ChainStep::End;
        };
        // The row below the frame is never read. On a scrolled-back view a segment reaching the
        // grid's edge may wrap into it, so that chain is unproven; the live view has no row below.
        if next_row_number >= view_end {
            // When: `next_row_number` reaches `view_end`, geometry alone decides Incomplete or End.
            let may_wrap = last.right == grid.cols && frame.rows_below();
            return if may_wrap { ChainStep::Incomplete } else { ChainStep::End };
        }
        let recorded = last.right == grid.cols
            && frame.row(next_row_number).is_some_and(|row| row.soft_wrapped_from_previous());
        if recorded && !wrap_joins_one_pane(frame, last.row) {
            // When: `recorded` but `wrap_joins_one_pane` fails, the wrap carries this pane's text into another pane.
            return ChainStep::End;
        }
        if recorded {
            // When: `recorded`, the terminal proved `last` continues on the next row.
            return ChainStep::Recorded(segment_at(next_row_number, 0));
        }
        if !alt_screen {
            // When: not `alt_screen`, only recorded wraps join rows.
            return ChainStep::End;
        }
        inferred_continuation_below(frame, last).map_or(ChainStep::End, ChainStep::Inferred)
    };
    let mut segments = VecDeque::from([pointed_segment]);
    let (mut first, mut last) = (pointed_segment, pointed_segment);
    let (mut grow_up, mut grow_down) = (true, true);
    while grow_up || grow_down {
        if grow_up {
            // `admit_chain_step` fails the whole scan when a recorded wrap leaves the line unproven.
            match admit_chain_step(step_up(first), segments.len()).ok()? {
                Some(segment) => {
                    first = segment;
                    segments.push_front(segment);
                }
                None => grow_up = false,
            }
        }
        if grow_down {
            match admit_chain_step(step_down(last), segments.len()).ok()? {
                Some(segment) => {
                    last = segment;
                    segments.push_back(segment);
                }
                None => grow_down = false,
            }
        }
    }

    let mut cells = Vec::new();
    let mut positions = Vec::new();
    let mut rows = SmallVec::<[PathRowIdentity; 2]>::new();
    for segment in &segments {
        let row = frame.row(segment.row)?;
        rows.push(PathRowIdentity { row: segment.row, fingerprint: row_fingerprint(row) });
        for (column, cell) in row.iter().enumerate() {
            let column = u16::try_from(column).ok()?;
            if !(segment.left..segment.right).contains(&column) {
                // When: `column` lies outside `segment`, its cell is a border or another pane's text.
                continue;
            }
            cells.push(cell);
            positions.push(AbsoluteCell { row: segment.row, col: column });
        }
        // Every scalar is at least one byte and covers at most two cells, so a chain past twice the
        // byte cap is a line `PathCellText::from_cells` refuses; stop gathering cells here.
        if cells.len() > 2 * MAX_LOGICAL_LINE_BYTES {
            // When: `cells` passes twice `MAX_LOGICAL_LINE_BYTES`, the line exceeds the cap.
            return None;
        }
    }
    let pointed_index = positions.iter().position(|position| *position == pointed)?;
    let mapped = PathCellText::from_cells(&cells, &positions)?;
    let candidates = mapped
        .candidates(&cells, pointed_index, style, include_bare_names)
        .into_iter()
        .filter_map(|(matched, range)| {
            let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
            for position in &positions[range] {
                match spans.last_mut() {
                    Some(span) if span.row == position.row && span.end_col == position.col => {
                        span.end_col = position.col.checked_add(1)?;
                    }
                    Some(span) if span.row != position.row => {
                        // Positions follow the joined segments, so a new row starts its next fragment.
                        spans.push(AbsoluteCellSpan {
                            row: position.row,
                            start_col: position.col,
                            end_col: position.col.checked_add(1)?,
                        });
                    }
                    None => spans.push(AbsoluteCellSpan {
                        row: position.row,
                        start_col: position.col,
                        end_col: position.col.checked_add(1)?,
                    }),
                    Some(_) => {
                        // When: `spans.last_mut()` is `Some(_)` without adjacency, reject the discontinuous span map.
                        return None;
                    }
                }
            }
            Some(LogicalTargetCandidate { target: matched.target, spans, joined: false })
        })
        .collect::<Vec<_>>();
    // A multiplexer may have cut the text at a pane edge. Shortest-first selection never needs to
    // rule out a longer name, so only the pointed run and each candidate that reaches a cut edge
    // are refused; a candidate wholly inside the visible chain stays valid.
    let chain_rows = segments.iter().map(|segment| segment.row).collect::<SmallVec<[u64; 8]>>();
    if alt_screen
        && spans_reach_cut_pane_edge(
            frame,
            &pointed_text_spans(&cells, &positions, pointed_index),
            &chain_rows,
        )
    {
        // When: alt_screen and the pointed run reaches a cut pane edge, its text may continue out of view.
        return None;
    }
    let candidates = candidates
        .into_iter()
        .filter(|candidate| {
            !alt_screen || !spans_reach_cut_pane_edge(frame, &candidate.spans, &chain_rows)
        })
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(LogicalPathScan { candidates, rows })
}

#[cfg(test)]
pub(super) fn row_target_candidates_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    include_bare_names: bool,
) -> Vec<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    if cells.get(col).is_none() {
        // When: `col` lies beyond the materialized row, no scanner span can own it.
        return Vec::new();
    }
    let positions =
        (0..cells.len()).map(|col| AbsoluteCell { row: 0, col: col as u16 }).collect::<Vec<_>>();
    let Some(mapped) = PathCellText::from_cells(&cells, &positions) else {
        // When: PathCellText::from_cells exceeds the logical bound, the row cannot authorize an incomplete candidate.
        return Vec::new();
    };
    mapped
        .candidates(&cells, col, style, include_bare_names)
        .into_iter()
        .filter_map(|(matched, range)| {
            Some(RowTarget {
                matched,
                start_col: u16::try_from(range.start).ok()?,
                end_col: u16::try_from(range.end).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "path_target_logical_tests.rs"]
mod path_target_logical_tests;
