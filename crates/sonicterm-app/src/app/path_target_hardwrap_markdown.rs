//! URLs inside Claude Code's numbered diff and markdown box.
//!
//! The box wraps each long source line at its own content width, short of the pane edge, and hangs
//! the rest below the line number with blank padding after every cut, so neither the edge-reaching
//! bracketed rule nor the bare rule sees the line. This rule rebuilds one numbered line from its
//! rows and finds the URL under the pointer in it, whatever surrounds the URL: a markdown target,
//! a quote, a parenthesis, a comment marker or nothing at all.

use super::*;

/// One displayed row of the box, read once inside the pointed pane.
struct BoxRow<'grid> {
    cells: Vec<&'grid Cell>,
    /// The row's first and last non-blank columns inside the pane; `None` when it holds no text.
    text: Option<(usize, usize)>,
    /// The column of the blank after one to six digits that start the row's text: its gutter end.
    gutter_end: Option<usize>,
    soft_wrapped: bool,
}

impl BoxRow<'_> {
    /// The row's character at `column` when that cell is plain text and not blank.
    fn plain_char(&self, column: usize) -> Option<char> {
        self.cells
            .get(column)
            .filter(|cell| plain_cell(cell) && !hardwrap_blank(cell))
            .map(|cell| cell.ch)
    }

    /// Whether the row holds no text at or before column `end`.
    fn blank_through(&self, end: usize) -> bool {
        self.text.is_none_or(|(first, _)| first > end)
    }
}

/// Whether `cell` is plain text: single-width, without combining extras and outside any OSC 8 link.
fn plain_cell(cell: &Cell) -> bool {
    !unsafe_path_cell(&cell) && cell.hyperlink().is_none()
}

/// The displayed rows of the pointed pane that one decision may read, each read at most once.
struct BoxRows<'frame, 'grid> {
    frame: &'frame FrameRows<'grid>,
    pane: (u16, u16),
    probe: u16,
    /// The first absolute row of `rows`; no decision reads above it.
    top: u64,
    rows: Vec<Option<Option<BoxRow<'grid>>>>,
}

impl<'grid> BoxRows<'_, 'grid> {
    /// Absolute `row_number` inside the pane; `None` when it is outside the read window, not
    /// displayed, or split into other pane columns.
    fn get(&mut self, row_number: u64) -> Option<&BoxRow<'grid>> {
        let index = usize::try_from(row_number.checked_sub(self.top)?).ok()?;
        if index >= self.rows.len() {
            // When: `index` is past the read window, the row is outside every bound this rule keeps.
            return None;
        }
        if self.rows[index].is_none() {
            // The slot was never read: read the row once and keep it for later lookups.
            self.rows[index] = Some(self.read(row_number));
        }
        self.rows[index].as_ref().and_then(Option::as_ref)
    }

    fn read(&self, row_number: u64) -> Option<BoxRow<'grid>> {
        let row = self.frame.row(row_number)?;
        if pane_columns_at(self.frame, row_number, self.probe) != self.pane {
            // When: the row splits into other pane columns, it is not this pane's box.
            return None;
        }
        let cells = row.iter().collect::<Vec<_>>();
        #[cfg(test)]
        hardwrap_work::add(cells.len());
        let left = usize::from(self.pane.0);
        let right = cells.len().min(usize::from(self.pane.1));
        let non_blank = |column: &usize| !hardwrap_blank(cells[*column]);
        let text = (left..right)
            .find(non_blank)
            .map(|first| (first, (first..right).rev().find(non_blank).unwrap_or(first)));
        let gutter_end = text.and_then(|(first, _)| {
            let digit_count =
                (first..right).take_while(|column| cells[*column].ch.is_ascii_digit()).count();
            let end = first + digit_count;
            let blank_after = end < right && cells[end].ch == ' ';
            ((1..=6).contains(&digit_count) && blank_after).then_some(end)
        });
        Some(BoxRow { cells, text, gutter_end, soft_wrapped: row.soft_wrapped_from_previous() })
    }
}

impl BoxRow<'_> {
    /// The first column of the row's text after the gutter end `gutter_end`, skipping one plain
    /// `+` or `-` that opens it: a diff row's sign, which is gutter, not text. `None` when the row
    /// holds no text there.
    fn text_start(&self, gutter_end: usize) -> Option<usize> {
        let (first, last) = self.text?;
        let first = first.max(gutter_end + 1);
        let first = (first..=last).find(|column| !hardwrap_blank(self.cells[*column]))?;
        let signed = self.plain_char(first).is_some_and(|sign| matches!(sign, '+' | '-'));
        let start = first + usize::from(signed);
        (start..=last).find(|column| !hardwrap_blank(self.cells[*column]))
    }

    /// The plain `+` or `-` that opens the row's text after `gutter_end`, when one does.
    fn sign(&self, gutter_end: usize) -> Option<char> {
        let (first, _) = self.text?;
        self.plain_char(first.max(gutter_end + 1)).filter(|sign| matches!(sign, '+' | '-'))
    }

    /// Whether the row continues a numbered line whose head carries `head_sign`: it soft-wraps
    /// nowhere, writes nothing into the gutter, holds text, and repeats the head's sign or, under
    /// an unsigned head, opens with no sign, which would start another diff line.
    fn continues(&self, gutter_end: usize, head_sign: Option<char>) -> bool {
        !self.soft_wrapped
            && self.blank_through(gutter_end)
            && self.text.is_some()
            && self.sign(gutter_end) == head_sign
    }
}

/// One cell's byte in a rebuilt line: plain ASCII text, blank padding, or text no URL may join.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineByte {
    /// A plain ASCII cell at `(row, column)`.
    Text(u64, u16),
    /// A blank cell, or the blank a row that stops before the box's cut ends with.
    Blank,
    /// Plain non-ASCII text: no URL character, so it ends a URL cleanly.
    Prose,
    /// A cell with combining extras or OSC 8 ownership: it may hide URL text, so no URL may abut it.
    Unsafe,
}

/// Decide the URL under `pointed` in one numbered line of the box. A row that reaches the box's
/// cut, the widest text column of the box's rows, joins the next row directly, so a URL that
/// starts mid-row, ends mid-row, or wraps many times is found as `find_urls` finds it in the source
/// line. A URL on one row is left to the single-row rules (`NotApplicable`); a URL the displayed
/// rows cannot prove whole is `Incomplete`, so its prefix never opens.
pub(super) fn numbered_box_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane target lies under it.
        return HardwrapUri::NotApplicable;
    }
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let top = pointed.row.saturating_sub(2 * reach).max(frame.view_top());
    let window_rows = (pointed.row + 2 * reach + 1).min(frame.view_end()).saturating_sub(top);
    let mut rows = BoxRows {
        frame,
        pane,
        probe: pointed.col,
        top,
        rows: (0..window_rows).map(|_| None).collect(),
    };
    let Some(head_row) = box_line_head(&mut rows, pointed.row) else {
        // When: no numbered row owns the pointed row, the pointer is not inside a box line.
        return HardwrapUri::NotApplicable;
    };
    box_line_uri(&mut rows, head_row, pointed)
}

/// The numbered row whose line holds `pointed_row`: the nearest row at or above it whose gutter
/// ends before every row between them starts its text, at most `MAX_WRAPPED_PATH_ROWS` rows up.
fn box_line_head(rows: &mut BoxRows<'_, '_>, pointed_row: u64) -> Option<u64> {
    let mut leftmost_text = usize::MAX;
    for row_number in
        (pointed_row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64)..=pointed_row).rev()
    {
        let row = rows.get(row_number)?;
        let (first, _) = row.text?;
        if row.gutter_end.is_some_and(|end| end < leftmost_text) {
            // When: the row's gutter ends before every continuation below starts, it heads the line.
            return Some(row_number);
        }
        if row.soft_wrapped {
            // When: a continuation row soft-wraps, its text is not this box's hard-broken line.
            return None;
        }
        leftmost_text = leftmost_text.min(first);
    }
    None
}

/// Rebuild the line headed by `head_row` and decide the URL under `pointed` in it.
fn box_line_uri(rows: &mut BoxRows<'_, '_>, head_row: u64, pointed: AbsoluteCell) -> HardwrapUri {
    let (gutter_end, head_sign) = {
        let Some(head) = rows.get(head_row) else {
            // When: the head row cannot be read, no line can be rebuilt.
            return HardwrapUri::NotApplicable;
        };
        let Some(gutter_end) = head.gutter_end else {
            // When: the head row has no numbered gutter, it heads no line.
            return HardwrapUri::NotApplicable;
        };
        if head.soft_wrapped {
            // When: the head row continues a soft wrap, the wrap-aware scanner owns it.
            return HardwrapUri::NotApplicable;
        }
        (gutter_end, head.sign(gutter_end))
    };

    // The box: the contiguous rows around the head that are its numbered rows or their hanging
    // continuations. Its widest text column is the cut every wrapped row reaches, and its
    // continuations' leftmost text start is the hang column every continuation's text starts at.
    let mut cut_end = 0;
    let mut hang = None::<usize>;
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let mut scan = |rows: &mut BoxRows<'_, '_>, row_number: u64| {
        let Some(row) = rows.get(row_number) else {
            // When: the row is not displayed in this pane, the box ends at it.
            return false;
        };
        let numbered = row.gutter_end == Some(gutter_end);
        let continuation = row.text.is_some() && row.blank_through(gutter_end);
        if row.soft_wrapped || !(numbered || continuation) {
            // When: the row soft-wraps or is neither a numbered row nor a continuation, the box ends.
            return false;
        }
        cut_end = cut_end.max(row.text.map_or(0, |(_, last)| last));
        if continuation {
            // A continuation's text start bounds the box's hang column.
            if let Some(start) = row.text_start(gutter_end) {
                hang = Some(hang.map_or(start, |hang| hang.min(start)));
            }
        }
        true
    };
    if !scan(rows, head_row) {
        // When: `scan` refuses `head_row` itself, no box line can be rebuilt around it.
        return HardwrapUri::NotApplicable;
    }
    for row_number in (head_row.saturating_sub(reach)..head_row).rev() {
        if !scan(rows, row_number) {
            // When: `scan` reached a row outside the box, the box starts below it.
            break;
        }
    }
    for row_number in head_row + 1..=head_row + 2 * reach {
        if !scan(rows, row_number) {
            // When: `scan` reached a row outside the box, the box ends above it.
            break;
        }
    }

    // The line: the head and every continuation below it, at most `MAX_WRAPPED_PATH_ROWS` rows.
    let mut line_end = head_row;
    let mut end_proven = false;
    for row_number in head_row + 1..=head_row + MAX_WRAPPED_PATH_ROWS as u64 {
        if row_number == head_row + MAX_WRAPPED_PATH_ROWS as u64 {
            // When: the line already holds `MAX_WRAPPED_PATH_ROWS` rows, the next row can only end
            // it; one that continues leaves the line's end unproven.
            end_proven = rows
                .get(row_number)
                .is_none_or(|row| !row.soft_wrapped && !row.continues(gutter_end, head_sign))
                && rows.frame.contains(row_number);
            break;
        }
        if !rows.frame.contains(row_number) {
            // When: the next row is outside the frame, only a view with rows below hides more.
            end_proven = !rows.frame.rows_below();
            break;
        }
        let continues =
            rows.get(row_number).is_some_and(|row| row.continues(gutter_end, head_sign));
        if !continues {
            // When: the row `continues` nothing, the line ended above it unless it soft-wraps.
            end_proven = rows.get(row_number).is_none_or(|row| !row.soft_wrapped);
            break;
        }
        line_end = row_number;
    }
    if pointed.row > line_end {
        // When: the pointed row is below the line's last row, this line does not hold it.
        return HardwrapUri::NotApplicable;
    }

    // Rebuild the line: the head from its text start, each continuation from its own. A row that
    // reaches the cut joins the next row directly, as the box cut the source line there; a row that
    // stops short ended at a blank in the source, and a continuation that starts right of the hang
    // column began with one, so one blank separates those rows.
    let mut text = String::new();
    let mut bytes = Vec::new();
    for row_number in head_row..=line_end {
        let Some(row) = rows.get(row_number) else {
            // When: a line row cannot be read, the line cannot be rebuilt.
            return HardwrapUri::NotApplicable;
        };
        let Some(start_col) = row.text_start(gutter_end) else {
            // When: a line row holds no text after its gutter, the line cannot be rebuilt.
            return HardwrapUri::NotApplicable;
        };
        let row_last = row.text.map_or(0, |(_, last)| last);
        if row_number > head_row && hang.is_some_and(|hang| start_col > hang) {
            // A continuation that starts right of the hang column began with a blank in the source.
            text.push(' ');
            bytes.push(LineByte::Blank);
        }
        for column in start_col..=row_last {
            if text.len() >= MAX_LOGICAL_LINE_BYTES {
                // When: the line passes `MAX_LOGICAL_LINE_BYTES`, the rest is not read.
                end_proven = false;
                break;
            }
            let byte = match row.cells.get(column) {
                None => LineByte::Blank,
                Some(cell) if cell.flags.contains(CellFlags::WIDE_CONT) => {
                    // When: the cell's flags hold WIDE_CONT, it is text only without its wide lead.
                    let lead = column.checked_sub(1).and_then(|lead| row.cells.get(lead));
                    if lead.is_some_and(|lead| lead.flags.contains(CellFlags::WIDE)) {
                        // When: the lead cell is WIDE, this cell continues it and adds no text.
                        continue;
                    }
                    LineByte::Unsafe
                }
                Some(cell) if !plain_cell(cell) => {
                    // When: an ascii or hyperlink cell is not plain, it may hide URL text.
                    if cell.ch.is_ascii() || cell.hyperlink().is_some() {
                        LineByte::Unsafe
                    } else {
                        LineByte::Prose
                    }
                }
                Some(cell) if hardwrap_blank(cell) => LineByte::Blank,
                Some(cell) if cell.ch.is_ascii() && !cell.ch.is_control() => {
                    LineByte::Text(row_number, column as u16)
                }
                Some(_) => LineByte::Prose,
            };
            text.push(match byte {
                LineByte::Text(..) => row.cells[column].ch,
                LineByte::Blank | LineByte::Prose | LineByte::Unsafe => ' ',
            });
            bytes.push(byte);
        }
        if row_number < line_end && row_last < cut_end {
            // A row above the last that stops before the cut ended at a blank in the source.
            text.push(' ');
            bytes.push(LineByte::Blank);
        }
    }
    let pointed_byte =
        bytes.iter().position(|byte| *byte == LineByte::Text(pointed.row, pointed.col));
    let Some(pointed_byte) = pointed_byte else {
        // When: no `pointed_byte` exists, the pointer is on padding, the gutter or non-URL text.
        return HardwrapUri::NotApplicable;
    };
    let matches = sonicterm_cfg::url_scan::find_urls(&text);
    let Some(found) = matches.iter().find(|found| (found.start..found.end).contains(&pointed_byte))
    else {
        // When: no `found` URL in `matches` covers the pointer, the single-row rules decide it.
        return HardwrapUri::NotApplicable;
    };
    let row_of = |byte: &LineByte| match byte {
        LineByte::Text(row, _) => Some(*row),
        _ => None,
    };
    let url_bytes = &bytes[found.start..found.end];
    let first_row = row_of(&url_bytes[0]);
    let multi_row = url_bytes.iter().any(|byte| row_of(byte) != first_row);
    if !multi_row {
        // When: `multi_row` is false, the single-row rules already read the URL whole.
        return HardwrapUri::NotApplicable;
    }
    if found.start > 0 && bytes[found.start - 1] == LineByte::Unsafe {
        // When: an unsafe cell abuts the URL's start, the visible text may not be its start.
        return HardwrapUri::NotApplicable;
    }
    if text.get(found.start..found.end) != Some(found.url.as_str())
        || found.url.matches("://").count() != 1
    {
        // When: the match is not one scheme's text copied from the line, two destinations joined.
        return HardwrapUri::NotApplicable;
    }
    let after = &bytes[found.end..];
    let runs_off = !end_proven && after.iter().all(|byte| matches!(byte, LineByte::Text(..)));
    if found.url.len() > MAX_LOGICAL_PATH_BYTES
        || after.first() == Some(&LineByte::Unsafe)
        || runs_off
    {
        // When: the url passes MAX_LOGICAL_PATH_BYTES, abuts an unsafe cell, or `runs_off` the
        // displayed rows, its visible text is not proven whole, so no prefix of it opens.
        return HardwrapUri::Incomplete;
    }
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    for byte in url_bytes {
        let LineByte::Text(row, column) = *byte else {
            // When: a URL byte is not a cell, `find_urls` matched padding, which it never does.
            return HardwrapUri::NotApplicable;
        };
        match spans.last_mut() {
            Some(span) if span.row == row && span.end_col == column => span.end_col += 1,
            _ => spans.push(AbsoluteCellSpan { row, start_col: column, end_col: column + 1 }),
        }
    }
    if spans_join_across_pane_border(rows.frame, &spans) {
        // When: `spans_join_across_pane_border` holds, the chain runs into the next pane's text.
        return HardwrapUri::NotApplicable;
    }
    HardwrapUri::Complete(LogicalTargetCandidate {
        target: DetectedTarget::Uri(found.url.clone()),
        spans,
        joined: false,
    })
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

#[cfg(test)]
#[path = "path_target_hardwrap_markdown_tests.rs"]
mod path_target_hardwrap_markdown_tests;

#[cfg(test)]
#[path = "path_target_hardwrap_markdown_quoted_tests.rs"]
mod path_target_hardwrap_markdown_quoted_tests;
