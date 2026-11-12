//! Padded hard-wrapped markdown link targets.
//!
//! Claude Code's numbered diff and markdown view wraps a long `[label](https://…)` target at its
//! own content width, short of the pane edge, and hangs the rest at the content column below the
//! line number. Blank padding follows the cut, so the edge-reaching bracketed rule never sees it.

use super::*;

/// One row's numbered gutter: blanks, one to six digits, one blank, then an optional diff sign.
#[derive(Clone, Copy)]
struct NumberedGutter {
    /// The column the row's content starts at, after the number and any sign.
    content: usize,
    /// The diff sign before `content`, when the row is an added or removed line.
    sign: Option<char>,
}

/// Read `cells`' numbered gutter from pane column `left`, or `None` when the row has none.
fn numbered_gutter(cells: &[&Cell], left: usize) -> Option<NumberedGutter> {
    let char_at = |column: usize| cells.get(column).map(|cell| cell.ch);
    let digits_start = (left..cells.len()).find(|column| !hardwrap_blank(cells[*column]))?;
    let digit_count =
        (digits_start..cells.len()).take_while(|column| cells[*column].ch.is_ascii_digit()).count();
    if !(1..=6).contains(&digit_count) || char_at(digits_start + digit_count) != Some(' ') {
        // When: `digit_count` is not 1..=6 or no blank follows the digits, the row has no gutter.
        return None;
    }
    let after_number = digits_start + digit_count + 1;
    let sign = char_at(after_number).filter(|sign| matches!(sign, '+' | '-'));
    if sign.is_some() && !plain_cell(cells[after_number]) {
        // When: `sign` sits in a cell that is not plain, the gutter cannot be read, so it has none.
        return None;
    }
    Some(NumberedGutter { content: after_number + usize::from(sign.is_some()), sign })
}

/// Whether `cell` is plain text: single-width, without combining extras and outside any OSC 8 link.
/// The structural cells a join removes (`]`, `(`, the closer, a repeated sign) must be plain.
fn plain_cell(cell: &Cell) -> bool {
    !unsafe_path_cell(&cell) && cell.hyperlink().is_none()
}

/// Whether the cells after the closer at column `close` of absolute row `row_number` leave it
/// the end of the URL. Up to the next whitespace, every cell must be sentence punctuation or a
/// non-ASCII character without extras: no URL character is non-ASCII, so such text cannot continue
/// the URL, while ASCII URL text (`)`, `/`, letters) would make the closer possibly internal.
/// Wide-character continuation cells are skipped. Only cells before column `edge` are read; when
/// the tail runs to an `edge` that is the grid's, a soft wrap into the next row, or an undisplayed
/// next row on a scrolled-back view, may carry the token on, so the closer is not proven final.
pub(super) fn closer_tail_ends_token(
    frame: &FrameRows<'_>,
    cells: &[&Cell],
    row_number: u64,
    close: usize,
    edge: usize,
) -> bool {
    let tail = cells.get(close + 1..edge.min(cells.len())).unwrap_or_default();
    let mut reached_whitespace = false;
    for cell in tail.iter().filter(|cell| !cell.flags.contains(CellFlags::WIDE_CONT)) {
        if cell.ch.is_whitespace() {
            // When: `cell.ch` is whitespace, the tail ends on this row.
            reached_whitespace = true;
            break;
        }
        let has_extras = cell.extras().is_some_and(|extras| !extras.is_empty());
        let prose = !cell.ch.is_ascii() || matches!(cell.ch, '.' | ',' | ';' | ':' | '!' | '?');
        if has_extras || cell.hyperlink().is_some() || !prose {
            // When: `cell` carries extras, a hyperlink, or ASCII URL text, the token may go on.
            return false;
        }
    }
    if reached_whitespace || edge < usize::from(frame.grid().cols) {
        // When: whitespace ends the tail, or `edge` is a pane border no wrap is recorded at, it ends here.
        return true;
    }
    let next_row_number = row_number + 1;
    if frame.contains(next_row_number) {
        !frame.row(next_row_number).is_some_and(Row::soft_wrapped_from_previous)
    } else {
        // When: the next row is outside the frame, only a scrolled-back view has one at all.
        !frame.rows_below()
    }
}

/// Decide a padded markdown link target under `pointed`. Every refusal is `NotApplicable`, so the
/// rule only adds joins and the single-row result stays the fallback.
pub(super) fn padded_markdown_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane target lies under it.
        return HardwrapUri::NotApplicable;
    }
    let top = pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    let mut rows = HardwrapRows { views: HashMap::new() };
    for first_row in (top..=pointed.row).rev() {
        let found = padded_markdown_chain(frame, &mut rows, pointed, pane, first_row);
        if !matches!(found, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for found, the nearest opener at or above owns pointed.
            return found;
        }
    }
    HardwrapUri::NotApplicable
}

/// The chain whose opener is on `first_row`, when it is complete and holds `pointed`.
fn padded_markdown_chain<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    pointed: AbsoluteCell,
    pane: (u16, u16),
    first_row: u64,
) -> HardwrapUri {
    let (left, right) = (usize::from(pane.0), usize::from(pane.1));
    let same_pane = |row_number: u64| pane_columns_at(frame, row_number, pointed.col) == pane;
    if !same_pane(first_row) {
        // When: `same_pane(first_row)` fails, the opener row is in another pane and opens no chain.
        return HardwrapUri::NotApplicable;
    }
    let Some(first) = rows.view(frame, first_row) else {
        // When: the frame has no `first_row`, it cannot open a visible chain.
        return HardwrapUri::NotApplicable;
    };
    if first.row.soft_wrapped_from_previous() {
        // When: the opener row continues a soft wrap, the wrap-aware scanner owns it.
        return HardwrapUri::NotApplicable;
    }
    let first_cells = first.cells.clone();
    let Some(gutter) = numbered_gutter(&first_cells, left) else {
        // When: the opener row has no numbered gutter, it is not the padded box this rule reads.
        return HardwrapUri::NotApplicable;
    };
    let edge = first_cells.len().min(right);
    let Some(last) =
        (gutter.content..edge).rev().find(|column| !hardwrap_blank(first_cells[*column]))
    else {
        // When: the opener row holds no content, it opens nothing.
        return HardwrapUri::NotApplicable;
    };
    let Some(opener) =
        (gutter.content..=last).rev().find(|column| hardwrap_body_break(first_cells[*column]))
    else {
        // When: no `hardwrap_body_break` cell precedes the fragment, no wrapper opens it.
        return HardwrapUri::NotApplicable;
    };
    let markdown_target = opener > gutter.content
        && first_cells[opener].ch == '('
        && first_cells[opener - 1].ch == ']'
        && plain_cell(first_cells[opener])
        && plain_cell(first_cells[opener - 1])
        && opens_http_scheme(&first_cells, opener + 1);
    if !markdown_target || matches!(first_cells[last].ch, '.' | ',' | ';' | ':' | '!' | '?') {
        // When: `markdown_target` is false or `first_cells[last]` is punctuation a word wrapper breaks after, refuse.
        return HardwrapUri::NotApplicable;
    }
    let mut joined = first_cells[opener + 1..=last].iter().map(|cell| cell.ch).collect::<String>();
    let scheme_len = if joined.starts_with("https://") { 8 } else { 7 };
    let path_slash = joined[scheme_len..]
        .find(['/', '?', '#'])
        .is_some_and(|index| index > 0 && joined.as_bytes()[scheme_len + index] == b'/');
    if !path_slash {
        // When: `path_slash` is false, the authority and its first slash are not on the opener row.
        return HardwrapUri::NotApplicable;
    }
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    spans.push(AbsoluteCellSpan {
        row: first_row,
        start_col: (opener + 1) as u16,
        end_col: (last + 1) as u16,
    });
    for offset in 1..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        if !same_pane(row_number) {
            // When: `same_pane(row_number)` fails, the chain leaves the pane before its closer.
            return HardwrapUri::NotApplicable;
        }
        let Some(view) = rows.view(frame, row_number) else {
            // When: the frame does not display `row_number`, the chain leaves it before its closer.
            return HardwrapUri::NotApplicable;
        };
        if view.row.soft_wrapped_from_previous() {
            // When: a continuation row is a soft wrap, its text is not this hard-broken target.
            return HardwrapUri::NotApplicable;
        }
        let cells = &view.cells;
        let blank_through = |end: usize| {
            (left..end).all(|column| cells.get(column).is_none_or(|cell| hardwrap_blank(cell)))
        };
        // The continuation repeats the opener row's sign, or leaves its column blank.
        let gutter_matches = match gutter.sign {
            Some(sign) => {
                blank_through(gutter.content - 1)
                    && cells
                        .get(gutter.content - 1)
                        .is_some_and(|cell| cell.ch == sign && plain_cell(cell))
            }
            None => blank_through(gutter.content),
        };
        if !gutter_matches || cells.get(gutter.content).is_none_or(|cell| hardwrap_blank(cell)) {
            // When: the gutter differs or the content column is blank, the row is not a hanging continuation.
            return HardwrapUri::NotApplicable;
        }
        let row_edge = cells.len().min(right);
        let close = (gutter.content..row_edge).find(|column| cells[*column].ch == ')');
        let body_end = match close {
            Some(close) => close,
            None => {
                // When: `close` is None, this is a middle row ending at the opener row's cut.
                let row_last =
                    (gutter.content..row_edge).rev().find(|column| !hardwrap_blank(cells[*column]));
                if row_last != Some(last) {
                    // When: `row_last` is not `last`, the middle row was not cut at the box width.
                    return HardwrapUri::NotApplicable;
                }
                last + 1
            }
        };
        let body = &cells[gutter.content..body_end];
        if body.is_empty() || body.iter().any(|cell| hardwrap_body_break(cell)) {
            // When: the body is empty (the URL ended on the row above) or holds a break, refuse.
            return HardwrapUri::NotApplicable;
        }
        if joined.len() + body.len() > MAX_LOGICAL_PATH_BYTES {
            // When: the joined text would pass `MAX_LOGICAL_PATH_BYTES`, stop before admitting more.
            return HardwrapUri::NotApplicable;
        }
        joined.extend(body.iter().map(|cell| cell.ch));
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: gutter.content as u16,
            end_col: body_end as u16,
        });
        let Some(close) = close else {
            // When: `close` is None, this was a middle row and the closer is further down.
            continue;
        };
        let Some(view) = rows.view(frame, row_number) else {
            // When: `rows.view` has no closer row, the chain cannot be proven.
            return HardwrapUri::NotApplicable;
        };
        // The tail after the closer is read only inside this pane.
        if !plain_cell(view.cells[close])
            || !closer_tail_ends_token(frame, &view.cells, row_number, close, right)
        {
            // When: `plain_cell` or `closer_tail_ends_token` fails, the closer may be internal URL text.
            return HardwrapUri::NotApplicable;
        }
        let matches = sonicterm_cfg::url_scan::find_urls(&joined);
        let whole = matches.len() == 1
            && joined.matches("://").count() == 1
            && matches[0].start == 0
            && matches[0].url == joined;
        if !whole
            || !spans.iter().any(|span| span.contains(pointed))
            || spans_join_across_pane_border(frame, &spans)
        {
            // When: the joined text is not one whole URL, misses the pointer, or crosses a pane border, refuse.
            return HardwrapUri::NotApplicable;
        }
        return HardwrapUri::Complete(LogicalTargetCandidate {
            target: DetectedTarget::Uri(joined),
            spans,
            joined: false,
        });
    }
    // Every row up to the cap continued the target, so its closer is out of reach.
    HardwrapUri::NotApplicable
}

#[cfg(test)]
#[path = "path_target_hardwrap_markdown_tests.rs"]
mod path_target_hardwrap_markdown_tests;
