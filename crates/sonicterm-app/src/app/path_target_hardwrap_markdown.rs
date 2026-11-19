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
    /// The first non-blank column after the gutter end `gutter_end`; `None` when there is none.
    fn first_after(&self, gutter_end: usize) -> Option<usize> {
        let (first, last) = self.text?;
        (first.max(gutter_end + 1)..=last).find(|column| !hardwrap_blank(self.cells[*column]))
    }

    /// The first column of the row's text after the gutter end, skipping one plain `+` or `-`
    /// that opens it. It measures the box's hang column, where a diff sign is gutter, not text.
    fn hang_start(&self, gutter_end: usize) -> Option<usize> {
        let first = self.first_after(gutter_end)?;
        let signed = self.plain_char(first).is_some_and(|sign| matches!(sign, '+' | '-'));
        let start = first + usize::from(signed);
        let (_, last) = self.text?;
        (start..=last).find(|column| !hardwrap_blank(self.cells[*column]))
    }

    /// The first column of the row's own text in a line whose head carries `head_sign`: after the
    /// repeated sign of a signed line; otherwise its first non-blank, even a `+` or `-` of a URL.
    fn line_start(&self, gutter_end: usize, head_sign: Option<char>) -> Option<usize> {
        let first = self.first_after(gutter_end)?;
        if head_sign.is_none() {
            // When: `head_sign` is None, a leading `+` or `-` is the line's text, not a sign.
            return Some(first);
        }
        let (_, last) = self.text?;
        (first + 1..=last).find(|column| !hardwrap_blank(self.cells[*column]))
    }

    /// Whether the row continues a numbered line whose head carries `head_sign`: it soft-wraps
    /// nowhere, writes nothing into the gutter, holds text, and, under a signed head, opens with
    /// the same plain sign.
    fn continues(&self, gutter_end: usize, head_sign: Option<char>) -> bool {
        if self.soft_wrapped || !self.blank_through(gutter_end) || self.text.is_none() {
            // When: the row soft_wrapped, writes into the gutter, or is blank, it ends the line.
            return false;
        }
        head_sign.is_none_or(|sign| {
            self.first_after(gutter_end).and_then(|column| self.plain_char(column)) == Some(sign)
        })
    }
}

/// Whether `cell` continues a wide character only when the cell before it, `lead`, is that
/// character's wide lead; any other wide continuation hides what the cell holds.
fn orphan_wide_cont(cell: &Cell, lead: Option<&&Cell>) -> bool {
    cell.flags.contains(CellFlags::WIDE_CONT)
        && !lead.is_some_and(|lead| lead.flags.contains(CellFlags::WIDE))
}

/// One cell's byte in a rebuilt line: plain ASCII text, blank padding, or text no URL may join.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineByte {
    /// A plain ASCII cell at `(row, column)`.
    Text(u64, u16),
    /// A blank cell, or the blank between rows the source line separated.
    Blank,
    /// Plain non-ASCII text: no URL character, so it ends a URL cleanly.
    Prose,
    /// A cell with combining extras, OSC 8 ownership or an orphan wide continuation: it may hide
    /// URL text, so a URL token that holds it never opens.
    Unsafe,
}

/// Whether `byte` of a rebuilt line, holding `character`, can be part of one URL token: URL body
/// text, or an unsafe cell that may hide some.
fn token_byte(byte: LineByte, character: char) -> bool {
    match byte {
        LineByte::Text(..) => {
            character.is_ascii_alphanumeric() || "-._~!$&*+,;=:/?#[]@%()".contains(character)
        }
        LineByte::Unsafe => true,
        LineByte::Blank | LineByte::Prose => false,
    }
}

/// The byte of a cell that is not plain: an ASCII cell, or one with extras or an OSC 8 link, may
/// hide URL text and is unsafe; a plain wide non-ASCII character ends a URL cleanly.
fn cell_line_byte(cell: &Cell) -> LineByte {
    let has_extras = cell.extras().is_some_and(|extras| !extras.is_empty());
    let hides_text = cell.ch.is_ascii() || has_extras || cell.hyperlink().is_some();
    [LineByte::Prose, LineByte::Unsafe][usize::from(hides_text)]
}

/// One numbered line's geometry inside the box, shared by its rebuild and its decision.
struct BoxLine {
    gutter_end: usize,
    head_sign: Option<char>,
    /// The box's cut: the widest text column of its rows, which every wrapped row reaches.
    cut_end: usize,
    /// The box's hang column: the leftmost text start of its continuation rows.
    hang: Option<usize>,
    /// The line's head row.
    head_row: u64,
    /// The first and last rows rebuilt: the line's rows within `MAX_WRAPPED_PATH_ROWS` of the
    /// pointed row, since no URL this rule joins spans more.
    first_row: u64,
    last_row: u64,
    /// Whether the line may continue below `last_row`: past the rebuilt rows, past the displayed
    /// frame, or into a soft-wrapped row.
    open_below: bool,
}

/// Decide the URL under `pointed` in one numbered line of the box. A row that reaches the box's
/// cut, the widest text column of the box's rows, joins the next row directly, so a URL that
/// starts mid-row, ends mid-row, or wraps many times is found as `find_urls` finds it in the source
/// line. The URL token under the pointer is owned by this rule once it spans two rows: it opens
/// only when `find_urls` reads it whole, within `MAX_WRAPPED_PATH_ROWS` rows and
/// `MAX_LOGICAL_PATH_BYTES` bytes, with no unsafe cell and no unread row it may continue into;
/// otherwise it is `Incomplete`, so no prefix of it opens. A one-row token is `NotApplicable`.
pub(super) fn numbered_box_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane target lies under it.
        return HardwrapUri::NotApplicable;
    }
    // Reads stay within the head search above the pointer, the rebuilt rows, and the rows around
    // them that measure the box, each bounded by `MAX_WRAPPED_PATH_ROWS`.
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let top = pointed.row.saturating_sub(4 * reach).max(frame.view_top());
    let bottom = (pointed.row + 2 * reach + 1).min(frame.view_end());
    let mut rows = BoxRows {
        frame,
        pane,
        probe: pointed.col,
        top,
        rows: (0..bottom.saturating_sub(top)).map(|_| None).collect(),
    };
    let Some(head_row) = box_line_head(&mut rows, pointed.row) else {
        // When: no numbered row owns the pointed row, the pointer is not inside a box line.
        return HardwrapUri::NotApplicable;
    };
    let Some(line) = box_line(&mut rows, head_row, pointed.row) else {
        // When: `box_line` reads no line around `head_row`, the rows are not a box line.
        return HardwrapUri::NotApplicable;
    };
    box_line_uri(&mut rows, &line, pointed)
}

/// The numbered row whose line holds `pointed_row`. Walking up, a numbered row qualifies when its
/// gutter ends before the text of every row between it and the pointer; the qualifying row with
/// the smallest gutter end wins, the nearest on a tie, so a continuation whose text starts with
/// digits never heads a line its real head owns. The walk ends at a row outside the box.
fn box_line_head(rows: &mut BoxRows<'_, '_>, pointed_row: u64) -> Option<u64> {
    let reach = 2 * (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let mut leftmost_text = usize::MAX;
    let mut best: Option<(u64, usize)> = None;
    for row_number in (pointed_row.saturating_sub(reach)..=pointed_row).rev() {
        let Some(row) = rows.get(row_number) else {
            // When: the row is not displayed in this pane, the box ends at it.
            break;
        };
        let Some((first, _)) = row.text else {
            // When: the row has no text, the box ends at it.
            break;
        };
        let candidate = row.gutter_end.filter(|end| *end < leftmost_text);
        if let Some(end) = candidate {
            // When: `candidate` holds a gutter `end` left of every row below, the row may head it.
            let best_end = best.map(|(_, best_end)| best_end);
            if best_end == Some(end) {
                // When: `end` equals `best_end`, an earlier line's head starts here, so the walk ends.
                break;
            }
            // A first or strictly smaller qualifying gutter end is the nearest real head so far.
            best = best.filter(|(_, best_end)| *best_end < end).or(Some((row_number, end)));
        }
        if best.is_some_and(|(_, best_end)| first <= best_end) && candidate.is_none() {
            // When: an unnumbered row's text starts at or before the best gutter end, it is not
            // in the box.
            break;
        }
        if row.soft_wrapped {
            // When: the row soft_wrapped, the rows above it are another line of text.
            break;
        }
        leftmost_text = leftmost_text.min(first);
    }
    best.map(|(row_number, _)| row_number)
}

/// The geometry of the line headed by `head_row` around `pointed_row`; `None` when the head row
/// is not a box row.
fn box_line(rows: &mut BoxRows<'_, '_>, head_row: u64, pointed_row: u64) -> Option<BoxLine> {
    let head = rows.get(head_row)?;
    let gutter_end = head.gutter_end?;
    if head.soft_wrapped {
        // When: the head row soft_wrapped, the wrap-aware scanner owns it.
        return None;
    }
    let head_sign = match head.cells.get(gutter_end + 1) {
        Some(cell) if matches!(cell.ch, '+' | '-') => Some(cell.ch),
        _ => None,
    };
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    // The line: the head and every continuation below it, read no further than `reach` rows past
    // the pointer, since no URL this rule joins spans more rows than that.
    let mut line_end = head_row;
    let mut open_below = true;
    for row_number in head_row + 1..=pointed_row + reach + 1 {
        if !rows.frame.contains(row_number) {
            // When: the next row is outside the frame, only a view with rows below hides more.
            open_below = rows.frame.rows_below();
            break;
        }
        if row_number > pointed_row + reach {
            // When: `row_number` passes `reach` below the pointer, the line is unread beyond it.
            open_below =
                rows.get(row_number).is_some_and(|row| row.continues(gutter_end, head_sign));
            break;
        }
        let continues =
            rows.get(row_number).is_some_and(|row| row.continues(gutter_end, head_sign));
        if !continues {
            // When: the row `continues` nothing, the line ended above it unless it soft-wraps.
            open_below = rows.get(row_number).is_some_and(|row| row.soft_wrapped);
            break;
        }
        line_end = row_number;
    }
    let first_row = head_row.max(pointed_row.saturating_sub(reach));
    let last_row = line_end.min(pointed_row + reach);

    // The box: the contiguous rows around the rebuilt rows that are numbered rows or their hanging
    // continuations. Its widest text column is the cut every wrapped row reaches, and its
    // continuations' leftmost text start is the hang column every continuation's text starts at.
    let mut cut_end = 0;
    let mut hang = None::<usize>;
    let mut scan = |rows: &mut BoxRows<'_, '_>, row_number: u64| {
        let Some(row) = rows.get(row_number) else {
            // When: the row is not displayed in this pane, the box ends at it.
            return false;
        };
        let numbered = row.gutter_end == Some(gutter_end);
        let continuation = row.text.is_some() && row.blank_through(gutter_end);
        if row.soft_wrapped || !(numbered || continuation) {
            // When: the row soft_wrapped or is neither numbered nor a continuation, the box ends.
            return false;
        }
        cut_end = cut_end.max(row.text.map_or(0, |(_, last)| last));
        if let Some(start) = row.hang_start(gutter_end).filter(|_| continuation) {
            // A continuation's text start bounds the box's hang column.
            hang = Some(hang.map_or(start, |hang| hang.min(start)));
        }
        true
    };
    for row_number in first_row..=last_row {
        if !scan(rows, row_number) {
            // When: `scan` refuses a rebuilt row, the line is not one box line.
            return None;
        }
    }
    for row_number in (first_row.saturating_sub(reach)..first_row).rev() {
        if !scan(rows, row_number) {
            // When: `scan` reached a row outside the box, the box starts below it.
            break;
        }
    }
    for row_number in last_row + 1..=last_row + reach {
        if !scan(rows, row_number) {
            // When: `scan` reached a row outside the box, the box ends above it.
            break;
        }
    }
    Some(BoxLine {
        gutter_end,
        head_sign,
        cut_end,
        hang,
        head_row,
        first_row,
        last_row,
        open_below: open_below || last_row < line_end,
    })
}

/// Rebuild `line` from its rows and decide the URL token under `pointed` in it.
fn box_line_uri(rows: &mut BoxRows<'_, '_>, line: &BoxLine, pointed: AbsoluteCell) -> HardwrapUri {
    if rows
        .get(line.head_row)
        .and_then(|head| head.cells.get(line.gutter_end + 1))
        .is_some_and(|cell| matches!(cell.ch, '+' | '-') && !plain_cell(cell))
    {
        // When: the head's sign column holds a `+` or `-` that is not plain, which rows repeat
        // it, and so which rows continue the line, cannot be read.
        return HardwrapUri::Incomplete;
    }

    // Rebuild the rows: the head from its text start, each continuation from its own. A row that
    // reaches the cut joins the next row directly, as the box cut the source line there; a row that
    // stops short ended at a blank in the source, and a continuation that starts right of the hang
    // column began with one, so one blank separates those rows.
    let mut text = String::new();
    let mut bytes = Vec::new();
    let mut byte_rows = Vec::new();
    let mut truncated = false;
    'rows: for row_number in line.first_row..=line.last_row {
        let Some(row) = rows.get(row_number) else {
            // When: a line row cannot be read, the line cannot be rebuilt.
            return HardwrapUri::NotApplicable;
        };
        // The head's text follows its sign column; a continuation's follows its repeated sign.
        let head_text_from = line.gutter_end + usize::from(line.head_sign.is_some());
        let start_col = match row_number == line.head_row {
            true => row.first_after(head_text_from),
            false => row.line_start(line.gutter_end, line.head_sign),
        };
        let Some(start_col) = start_col else {
            // When: no `start_col` exists, a line row holds no text after its gutter.
            return HardwrapUri::NotApplicable;
        };
        let row_last = row.text.map_or(0, |(_, last)| last);
        if row_number > line.first_row
            && row_number > line.head_row
            && line.hang.is_some_and(|hang| start_col > hang)
        {
            // A continuation that starts right of the hang column began with a blank in the source.
            text.push(' ');
            bytes.push(LineByte::Blank);
            byte_rows.push(row_number);
        }
        for column in start_col..=row_last {
            if text.len() >= MAX_LOGICAL_LINE_BYTES {
                // When: the line passes `MAX_LOGICAL_LINE_BYTES`, the rest of it is not read.
                truncated = true;
                break 'rows;
            }
            let byte = match row.cells.get(column) {
                None => LineByte::Blank,
                Some(cell) if cell.flags.contains(CellFlags::WIDE_CONT) => {
                    // When: the cell's flags hold WIDE_CONT, it adds no text after its wide lead
                    // and hides what it holds without one.
                    let lead = column.checked_sub(1).and_then(|lead| row.cells.get(lead));
                    if !orphan_wide_cont(cell, lead) {
                        // When: `orphan_wide_cont` is false, the cell continues its wide lead.
                        continue;
                    }
                    LineByte::Unsafe
                }
                Some(cell) if !plain_cell(cell) => cell_line_byte(cell),
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
            byte_rows.push(row_number);
        }
        if row_number < line.last_row && row_last < line.cut_end {
            // A row above the last that stops before the cut ended at a blank in the source.
            text.push(' ');
            bytes.push(LineByte::Blank);
            byte_rows.push(row_number);
        }
    }
    let Some(pointed_byte) =
        bytes.iter().position(|byte| *byte == LineByte::Text(pointed.row, pointed.col))
    else {
        // When: no `pointed_byte` exists, the pointer is on padding, the gutter or unread text.
        return if truncated { HardwrapUri::Incomplete } else { HardwrapUri::NotApplicable };
    };
    let rebuilt = RebuiltLine { text: &text, bytes: &bytes, rows: &byte_rows, truncated };
    box_token_uri(rows.frame, line, &rebuilt, pointed_byte)
}

/// One rebuilt line: each byte's character (blanks, prose and unsafe cells as spaces), what each
/// byte is, the row each came from, and whether `MAX_LOGICAL_LINE_BYTES` cut the line short.
struct RebuiltLine<'line> {
    text: &'line str,
    bytes: &'line [LineByte],
    rows: &'line [u64],
    truncated: bool,
}

/// Decide the URL token around `pointed_byte` of `rebuilt`.
fn box_token_uri(
    frame: &FrameRows<'_>,
    line: &BoxLine,
    rebuilt: &RebuiltLine<'_>,
    pointed_byte: usize,
) -> HardwrapUri {
    let RebuiltLine { text, bytes, rows: byte_rows, truncated } = *rebuilt;
    let raw = text.as_bytes();
    let in_token = |index: usize| token_byte(bytes[index], raw[index] as char);
    let token_start =
        (0..pointed_byte).rev().find(|index| !in_token(*index)).map_or(0, |index| index + 1);
    let token_end =
        (pointed_byte..bytes.len()).find(|index| !in_token(*index)).unwrap_or(bytes.len());
    let row_of = |byte: &LineByte| match byte {
        LineByte::Text(row, _) => Some(*row),
        _ => None,
    };
    let token = &bytes[token_start..token_end];
    let multi_row = byte_rows[token_start] != byte_rows[token_end - 1];
    let token_text = &text[token_start..token_end];
    let scheme_at = token_text
        .to_ascii_lowercase()
        .find("http://")
        .into_iter()
        .chain(token_text.to_ascii_lowercase().find("https://"))
        .min();
    let Some(scheme_at) = scheme_at.filter(|_| multi_row) else {
        // When: no `scheme_at` exists or `multi_row` is false, the single-row rules decide.
        return HardwrapUri::NotApplicable;
    };
    if pointed_byte < token_start + scheme_at {
        // When: `pointed_byte` is before the scheme, the pointer is on text that leads the URL.
        return HardwrapUri::NotApplicable;
    }
    // From here the token is a URL the box broke across rows: it opens whole or not at all.
    let open_start = line.first_row > line.head_row && token_start == 0;
    let open_end = (line.open_below || truncated) && token_end == bytes.len();
    if open_start || open_end || token.contains(&LineByte::Unsafe) {
        // When: the token may run past an unread or undisplayed row, or holds an unsafe cell, its
        // visible text is not proven whole, so no prefix of it opens.
        return HardwrapUri::Incomplete;
    }
    let matches = sonicterm_cfg::url_scan::find_urls(text);
    let Some(found) = matches.iter().find(|found| (found.start..found.end).contains(&pointed_byte))
    else {
        // When: no `found` URL in `matches` covers the pointer, the token is no valid URL as a
        // whole, too long or malformed, so none of it opens.
        return HardwrapUri::Incomplete;
    };
    let url_bytes = &bytes[found.start..found.end];
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    for byte in url_bytes {
        let LineByte::Text(row, column) = *byte else {
            // When: a URL byte is not a cell, `find_urls` matched padding, which it never does.
            return HardwrapUri::Incomplete;
        };
        match spans.last_mut() {
            Some(span) if span.row == row && span.end_col == column => span.end_col += 1,
            _ => spans.push(AbsoluteCellSpan { row, start_col: column, end_col: column + 1 }),
        }
    }
    let later_scheme = found
        .url
        .match_indices("://")
        .skip(1)
        .any(|(index, _)| row_of(&url_bytes[index]) != row_of(&url_bytes[0]));
    if spans.len() > MAX_WRAPPED_PATH_ROWS
        || found.url.len() > MAX_LOGICAL_PATH_BYTES
        || later_scheme
        || text.get(found.start..found.end) != Some(found.url.as_str())
    {
        // When: the url passes MAX_WRAPPED_PATH_ROWS or MAX_LOGICAL_PATH_BYTES, or a later row
        // opens another scheme, two destinations or an unbounded one would open.
        return HardwrapUri::Incomplete;
    }
    if spans.len() < 2 {
        // When: the `spans` of the URL `find_urls` read hold one row, single-row rules decide.
        return HardwrapUri::NotApplicable;
    }
    if spans_join_across_pane_border(frame, &spans) {
        // When: `spans_join_across_pane_border` holds, the chain runs into the next pane's text.
        return HardwrapUri::Incomplete;
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
