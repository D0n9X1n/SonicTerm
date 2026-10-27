//! Bounded explicit-wrapper HTTP reconstruction and URI precedence.

use super::*;

#[cfg(test)]
pub(super) fn hardwrap_uri_at_cell(
    grid: &Grid,
    view_top: u64,
    pointed: AbsoluteCell,
) -> HardwrapUri {
    hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed)
}

impl<'grid> HardwrapRows<'grid> {
    /// The view of absolute `row_number`; `None` when the frame does not display it.
    pub(super) fn view(
        &mut self,
        frame: &FrameRows<'grid>,
        row_number: u64,
    ) -> Option<&HardwrapRowView<'grid>> {
        self.views
            .entry(row_number)
            .or_insert_with(|| {
                let row = frame.row(row_number)?;
                let cells = row.iter().collect::<Vec<_>>();
                #[cfg(test)]
                hardwrap_work::add(cells.len());
                let edge = cells.len().min(usize::from(frame.grid().cols));
                let last_break = cells[..edge].iter().rposition(|cell| hardwrap_body_break(cell));
                Some(HardwrapRowView { row, cells, last_break })
            })
            .as_ref()
    }
}

/// Whether `cell` cannot be part of a hard-wrapped URI body: unsafe, hyperlinked, non-ASCII,
/// blank, control, or a wrapper. Every wrapper is itself a break.
pub(super) fn hardwrap_body_break(cell: &Cell) -> bool {
    unsafe_path_cell(&cell)
        || cell.hyperlink().is_some()
        || !cell.ch.is_ascii()
        || cell.ch.is_whitespace()
        || cell.ch.is_control()
        || matches!(cell.ch, '(' | ')' | '[' | ']')
}

pub(super) fn hardwrap_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let first =
        pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    let mut rows = HardwrapRows { views: HashMap::new() };
    for row_number in (first..=pointed.row).rev() {
        let Some(view) = rows.view(frame, row_number) else {
            // When: the frame has no row_number, that row cannot open a visible candidate.
            continue;
        };
        let soft_wrapped = view.row.soft_wrapped_from_previous();
        // A chain's first row must reach the grid's edge with no closer and no break in its body.
        // Every wrapper is a break, so only a wrapper at the row's last break can open such a body;
        // an earlier wrapper's body holds that break and its candidate is never applicable.
        let opener = view.last_break.and_then(|index| {
            let closer = match view.cells[index].ch {
                '(' => ')',
                '[' => ']',
                _ => {
                    // When: view.cells[index].ch at last_break opens no supported wrapper, no wrapper owns a body.
                    return None;
                }
            };
            opens_http_scheme(&view.cells, index + 1).then_some((index + 1, closer))
        });
        // The frame's last row may wrap into a row below it that is not displayed, so on a
        // scrolled-back view that boundary is unknown and the logical scanner, which fails closed
        // there, owns it.
        let next_row_number = row_number + 1;
        let wraps_below = if frame.contains(next_row_number) {
            rows.view(frame, next_row_number)
                .is_some_and(|next| next.row.soft_wrapped_from_previous())
        } else {
            // When: the next row is outside the frame, only a scrolled-back view has one at all.
            frame.rows_below()
        };
        if soft_wrapped || wraps_below {
            // When: soft_wrapped or wraps_below holds, or may hold, the logical scanner owns that boundary.
            continue;
        }
        let Some((start, closer)) = opener else {
            // When: opener is None, no wrapper opens an http(s) body at last_break, so no chain starts here.
            continue;
        };
        let found = hardwrap_uri_candidate(frame, &mut rows, pointed, row_number, start, closer);
        if !matches!(found, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for found, the nearest owning wrapper decides pointed.
            return found;
        }
    }
    HardwrapUri::NotApplicable
}

/// Whether `cells` from column `start` spell `https://` or `http://`, reading at most eight cells.
pub(super) fn opens_http_scheme(cells: &[&Cell], start: usize) -> bool {
    let spells = |prefix: &str| {
        let mut found = cells.get(start..).unwrap_or_default().iter().map(|cell| cell.ch);
        prefix.chars().all(|expected| found.next() == Some(expected))
    };
    spells("https://") || spells("http://")
}

pub(super) fn hardwrap_uri_candidate<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    pointed: AbsoluteCell,
    first_row: u64,
    first_col: usize,
    closer: char,
) -> HardwrapUri {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    let mut joined = String::new();
    let mut indent = None;
    let mut authority_end = None;
    let refusal = |spans: &[AbsoluteCellSpan]| {
        // When: a span contains pointed and spans_join_across_pane_border is false, suppress that fragment's truncated prefix.
        if spans.iter().any(|span| span.contains(pointed))
            && !spans_join_across_pane_border(frame, spans)
        {
            HardwrapUri::Incomplete
        } else {
            HardwrapUri::NotApplicable
        }
    };
    for offset in 0..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        // When: row_number reaches view_end, the chain leaves the viewport and cannot be proven complete.
        if row_number >= frame.view_end() {
            return refusal(&spans);
        }
        let Some(view) = rows.view(frame, row_number) else {
            // When: the frame has no row_number, the chain cannot be continued or proven.
            return refusal(&spans);
        };
        if view.row.soft_wrapped_from_previous() {
            // When: soft_wrapped_from_previous holds, the wrap-aware scanner owns this boundary.
            return refusal(&spans);
        }
        let cells = &view.cells;
        // Each chain row's closer, indent and body scans walk at most its cells once.
        #[cfg(test)]
        hardwrap_work::add(cells.len());
        let start = if offset == 0 {
            first_col
        } else {
            // When: offset is past zero, start skips the continuation indent rather than first_col.
            cells.iter().position(|cell| cell.ch != ' ').unwrap_or(cells.len())
        };
        let close = cells
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, cell)| cell.ch == closer)
            .map(|(col, _)| col);
        if offset == 0 && close.is_some() {
            // When: close exists at offset zero, single-row detection stays authoritative.
            return HardwrapUri::NotApplicable;
        }
        let end = close.unwrap_or(usize::from(frame.grid().cols));
        let Some(body) = cells.get(start..end).filter(|body| !body.is_empty()) else {
            // When: cells hold no non-empty body between start and end, this row adds no fragment.
            return refusal(&spans);
        };
        if body.iter().any(|cell| hardwrap_body_break(cell)) {
            // When: any body cell is unsafe, hyperlinked, non-ascii, blank, control, or a wrapper,
            // the row carries prose rather than a continuation of this candidate.
            return refusal(&spans);
        }
        if offset > 0 {
            // When: offset is past zero, a continuation row must open no scheme and respect the indent.
            let prefix = body.iter().take(8).map(|cell| cell.ch).collect::<String>();
            if prefix.starts_with("https://") || prefix.starts_with("http://") {
                // When: prefix opens its own scheme, that independent URI keeps its destination.
                return refusal(&spans);
            }
            if start > MAX_HARDWRAP_INDENT {
                // When: start passes MAX_HARDWRAP_INDENT, the row is too deep to be one continued line.
                return refusal(&spans);
            }
        }
        if offset > 0 && *indent.get_or_insert(start) != start {
            // When: start differs from the recorded indent, the preceding fragment does not own this row.
            return refusal(&spans);
        }
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: start as u16,
            end_col: end as u16,
        });
        if joined.len() + body.len() > MAX_LOGICAL_PATH_BYTES {
            // When: joined plus body passes MAX_LOGICAL_PATH_BYTES, stop before admitting more text.
            return refusal(&spans);
        }
        joined.extend(body.iter().map(|cell| cell.ch));
        if offset == 0 {
            // When: offset is zero, joined must prove a complete authority before the margin cut.
            let scheme_len = if joined.starts_with("https://") { 8 } else { 7 };
            authority_end = joined[scheme_len..]
                .find(['/', '?', '#'])
                .filter(|index| *index > 0 && joined.as_bytes()[scheme_len + index] == b'/')
                .map(|index| scheme_len + index);
            if authority_end.is_none() {
                // When: authority_end is absent, no path slash followed the host, so invent no suffix.
                return refusal(&spans);
            }
        }
        if joined.matches("://").count() != 1 {
            // When: joined holds other than one :// separator, two destinations were concatenated.
            return refusal(&spans);
        }
        if close.is_some() {
            // When: close exists, the wrapper terminates and joined must validate as one whole URI.
            if unsafe_path_cell(&cells[end])
                || cells[end].hyperlink().is_some()
                || cells[end + 1..].iter().take_while(|cell| !cell.ch.is_whitespace()).any(|cell| {
                    unsafe_path_cell(cell) || !matches!(cell.ch, '.' | ',' | ';' | ':' | '!' | '?')
                })
            {
                // When: cells after end continue the token, the apparent closer may be internal URI text.
                return refusal(&spans);
            }
            let matches = sonicterm_cfg::url_scan::find_urls(&joined);
            let Some(found) = matches.first().filter(|found| {
                matches.len() == 1
                    && found.start == 0
                    && found.end == joined.len()
                    && found.url == joined
                    && authority_end.is_some()
            }) else {
                // When: find_urls does not return exactly joined as one whole match, reject the prefix.
                return refusal(&spans);
            };
            if !spans.iter().any(|span| span.contains(pointed)) {
                // When: no span contains pointed, this proven chain does not own the pointer.
                return HardwrapUri::NotApplicable;
            }
            if spans_join_across_pane_border(frame, &spans) {
                // When: spans_join_across_pane_border holds, the chain runs into the next pane's text.
                return HardwrapUri::NotApplicable;
            }
            return HardwrapUri::Complete(LogicalTargetCandidate {
                target: DetectedTarget::Uri(found.url.clone()),
                spans,
                joined: false,
            });
        }
    }
    refusal(&spans)
}

/// Validate a proven chain's joined text as one URL for `pointed`: `find_urls` must read it as one
/// URL from its start, and sentence punctuation it trims must lie inside the last fragment.
pub(super) fn hardwrap_uri_target(
    mut spans: SmallVec<[AbsoluteCellSpan; 2]>,
    joined: &str,
    pointed: AbsoluteCell,
) -> HardwrapUri {
    let matches = sonicterm_cfg::url_scan::find_urls(joined);
    let Some(found) = matches.first().filter(|found| {
        matches.len() == 1
            && found.start == 0
            && joined.matches("://").count() == 1
            && joined.get(..found.end) == Some(found.url.as_str())
    }) else {
        // When: `find_urls` does not read `joined` as one URL from its start, the owned chain stays inert.
        return HardwrapUri::Incomplete;
    };
    let trimmed = joined.len() - found.end;
    let Some(last) =
        spans.last_mut().filter(|last| trimmed < usize::from(last.end_col - last.start_col))
    else {
        // When: the trimmed tail covers the whole last fragment, the URL ends before it.
        return HardwrapUri::Incomplete;
    };
    last.end_col -= trimmed as u16;
    if !spans.iter().any(|span| span.contains(pointed)) {
        // When: `pointed` is on punctuation `find_urls` trimmed, it is not part of the URL.
        return HardwrapUri::NotApplicable;
    }
    HardwrapUri::Complete(LogicalTargetCandidate {
        target: DetectedTarget::Uri(found.url.clone()),
        spans,
        joined: true,
    })
}

#[cfg(test)]
#[path = "path_target_hardwrap_uri_tests.rs"]
mod path_target_hardwrap_uri_tests;
