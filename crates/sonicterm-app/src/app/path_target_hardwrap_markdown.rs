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
            .filter(|cell| !box_blank(cell) && plain_cell(cell))
            .map(|cell| cell.ch)
    }

    /// Whether the row holds no text at or before column `end`.
    fn blank_through(&self, end: usize) -> bool {
        self.text.is_none_or(|(first, _)| first > end)
    }

    /// The number of columns the row's text spans, a bound on the bytes it adds to a line.
    fn text_len(&self) -> usize {
        self.text.map_or(0, |(first, last)| last + 1 - first)
    }
}

/// Whether `cell` is plain text: single-width, without combining extras and outside any OSC 8 link.
fn plain_cell(cell: &Cell) -> bool {
    !unsafe_path_cell(&cell) && cell.hyperlink().is_none()
}

/// Whether `cell` is padding: a plain blank. A blank that carries combining extras or an OSC 8
/// link is text the rule cannot read, so it is never trimmed away as padding.
fn box_blank(cell: &Cell) -> bool {
    hardwrap_blank(cell) && plain_cell(cell)
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
        let non_blank = |column: &usize| !box_blank(cells[*column]);
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
        (first.max(gutter_end + 1)..=last).find(|column| !box_blank(self.cells[*column]))
    }

    /// The first column of the row's text after the gutter end, skipping one plain `+` or `-`
    /// that opens it. It measures the box's hang column, where a diff sign is gutter, not text.
    fn hang_start(&self, gutter_end: usize) -> Option<usize> {
        let first = self.first_after(gutter_end)?;
        let signed = self.plain_char(first).is_some_and(|sign| matches!(sign, '+' | '-'));
        let start = first + usize::from(signed);
        let (_, last) = self.text?;
        (start..=last).find(|column| !box_blank(self.cells[*column]))
    }

    /// The first column of the row's own text in a line whose head carries `head_sign`: after a
    /// signed line's repeated plain sign; otherwise its first non-blank, even a `+` or `-` of a URL
    /// or a cell the rule cannot read. `None` when the row holds no text after its sign.
    fn line_start(&self, gutter_end: usize, head_sign: Option<char>) -> Option<usize> {
        let first = self.first_after(gutter_end)?;
        if head_sign.is_none() || self.plain_char(first) != head_sign {
            // When: `head_sign` is None or the row's `first` cell is not that plain sign, the row's
            // text starts at `first`.
            return Some(first);
        }
        let (_, last) = self.text?;
        (first + 1..=last).find(|column| !box_blank(self.cells[*column]))
    }

    /// Whether the row continues a numbered line whose head carries `head_sign`: it soft-wraps
    /// nowhere, writes nothing into the gutter, holds text, and, under a signed head, opens with
    /// the same plain sign or with a cell the rule cannot read, which the rebuild keeps unsafe.
    fn continues(&self, gutter_end: usize, head_sign: Option<char>) -> bool {
        if self.soft_wrapped || !self.blank_through(gutter_end) || self.text.is_none() {
            // When: the row soft_wrapped, writes into the gutter, or is blank, it ends the line.
            return false;
        }
        let first = self.first_after(gutter_end).and_then(|column| self.cells.get(column));
        head_sign.is_none_or(|sign| {
            first.is_some_and(|cell| !plain_cell(cell) || (cell.ch == sign && !box_blank(cell)))
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

/// Where the numbered row that heads the pointed row's line is.
enum LineHead {
    /// This displayed row heads the line.
    Row(u64),
    /// No displayed row heads it: the walk reached the frame's top inside the box, so the head
    /// may lie above the frame.
    AboveFrame,
    /// The pointed row is not inside a box line.
    Outside,
}

/// One numbered line's geometry inside the box, shared by its rebuild and its decision.
struct BoxLine {
    gutter_end: usize,
    head_sign: Option<char>,
    /// The box's cut: the widest text column of its rows, which every wrapped row reaches.
    cut_end: usize,
    /// The box's hang column: the leftmost text start of its continuation rows.
    hang: Option<usize>,
    /// The line's head row, when it lies within the rebuilt rows.
    head_row: Option<u64>,
    /// The first and last rows rebuilt: the line's rows around the pointed row, within
    /// `MAX_WRAPPED_PATH_ROWS` rows and `MAX_LOGICAL_PATH_BYTES` bytes of it, since no URL this
    /// rule joins spans more.
    first_row: u64,
    last_row: u64,
    /// Whether the line may continue above `first_row` or below `last_row` into rows the rebuild
    /// did not read or the frame does not display.
    open_above: bool,
    open_below: bool,
}

/// Decide the URL under `pointed` in one numbered line of the box. A row that reaches the box's
/// cut, the widest text column of the box's rows, joins the next row directly, so a URL that
/// starts mid-row, ends mid-row, or wraps many times is found as `find_urls` finds it in the source
/// line. The URL token under the pointer is owned by this rule once it spans two rows or may run
/// into rows the rebuild did not read: it opens only when `find_urls` reads it whole, within
/// `MAX_WRAPPED_PATH_ROWS` rows and `MAX_LOGICAL_PATH_BYTES` bytes, with no unsafe cell; otherwise
/// it is `Incomplete`, so no prefix of it opens. A one-row token is `NotApplicable`.
pub(super) fn numbered_box_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane target lies under it.
        return HardwrapUri::NotApplicable;
    }
    // Reads stay within the displayed frame: the head search may reach its top, and the rows
    // below the pointer that rebuild and measure the line are bounded by `MAX_WRAPPED_PATH_ROWS`.
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let top = frame.view_top();
    let bottom = (pointed.row + 2 * reach + 1).min(frame.view_end());
    let mut rows = BoxRows {
        frame,
        pane,
        probe: pointed.col,
        top,
        rows: (0..bottom.saturating_sub(top)).map(|_| None).collect(),
    };
    let head = match box_line_head(&mut rows, pointed.row) {
        LineHead::Row(head_row) => {
            // When: `box_line_head` found the `head_row`, its gutter and sign come from it.
            let Some(head) = rows.get(head_row) else {
                // When: the head row cannot be read, no line can be rebuilt.
                return HardwrapUri::NotApplicable;
            };
            let Some(gutter_end) = head.gutter_end.filter(|_| !head.soft_wrapped) else {
                // When: the head row has no gutter or soft-wraps, it heads no box line.
                return HardwrapUri::NotApplicable;
            };
            let sign_cell = head.cells.get(gutter_end + 1);
            if sign_cell.is_some_and(|cell| matches!(cell.ch, '+' | '-') && !plain_cell(cell)) {
                // When: the `sign_cell` holds a `+` or `-` that is not `plain_cell`, which rows
                // repeat it, and so which rows continue the line, cannot be read.
                return HardwrapUri::Incomplete;
            }
            let sign = sign_cell.map(|cell| cell.ch).filter(|sign| matches!(sign, '+' | '-'));
            (Some(head_row), gutter_end, sign)
        }
        LineHead::AboveFrame => {
            // When: the walk met the frame's top first, `AboveFrame` reads the box below instead.
            let Some((gutter_end, sign)) = headless_line(&mut rows, pointed.row) else {
                // When: no numbered row below shows the box's gutter, the rows are not a box line.
                return HardwrapUri::NotApplicable;
            };
            (None, gutter_end, sign)
        }
        LineHead::Outside => {
            // When: no numbered row owns the pointed row, the pointer is not inside a box line.
            return HardwrapUri::NotApplicable;
        }
    };
    let Some(line) = box_line(&mut rows, head, pointed.row) else {
        // When: `box_line` reads no line around the pointed row, the rows are not a box line.
        return HardwrapUri::NotApplicable;
    };
    box_line_uri(&mut rows, &line, pointed)
}

/// The numbered row whose line holds `pointed_row`. Walking up, a numbered row qualifies when its
/// gutter ends before the text of every row between it and the pointer; the qualifying row with
/// the smallest gutter end wins, the nearest on a tie, so a continuation whose text starts with
/// digits never heads a line its real head owns. The walk ends at a row outside the box, or at the
/// frame's top, above which the head may lie.
fn box_line_head(rows: &mut BoxRows<'_, '_>, pointed_row: u64) -> LineHead {
    let mut leftmost_text = usize::MAX;
    let mut best: Option<(u64, usize)> = None;
    let top = rows.frame.view_top();
    for row_number in (top..=pointed_row).rev() {
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
        let outside = best.is_some_and(|(_, best_end)| first <= best_end) && candidate.is_none();
        if outside || row.soft_wrapped {
            // When: an unnumbered row's text starts at or before the best gutter end, or the row
            // soft_wrapped, the rows above it are not this box line.
            break;
        }
        leftmost_text = leftmost_text.min(first);
        if best.is_none() && leftmost_text <= 1 {
            // When: `leftmost_text` is at most 1, no gutter above can end left of it.
            return LineHead::Outside;
        }
        if row_number == top {
            // When: the walk reached the frame's top with no head, the head may lie above it.
            return best.map_or(LineHead::AboveFrame, |(head_row, _)| LineHead::Row(head_row));
        }
    }
    best.map_or(LineHead::Outside, |(head_row, _)| LineHead::Row(head_row))
}

/// The gutter end and sign of a box line whose head lies above the frame, read from the box below:
/// the first numbered row past the pointer, through rows that continue it, whose gutter ends left
/// of every one of their text starts. The sign is the plain `+` or `-` every row of the line opens
/// with, when one does. `None` when no such row shows the box.
fn headless_line(rows: &mut BoxRows<'_, '_>, pointed_row: u64) -> Option<(usize, Option<char>)> {
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let top = rows.frame.view_top().max(pointed_row.saturating_sub(reach));
    let mut leftmost_text = usize::MAX;
    let mut line_rows = SmallVec::<[u64; 8]>::new();
    for row_number in top..=pointed_row + 2 * reach {
        let row = rows.get(row_number)?;
        let (first, _) = row.text?;
        if row.soft_wrapped {
            // When: the row soft_wrapped, it is not part of the box's hard-broken rows.
            return None;
        }
        if row_number > pointed_row && row.gutter_end.is_some_and(|end| end < leftmost_text) {
            // When: a numbered row below ends its gutter left of the line's text, it shows the box.
            let gutter_end = row.gutter_end?;
            let signs = line_rows.iter().map(|line_row| {
                rows.get(*line_row).and_then(|row| {
                    row.first_after(gutter_end).and_then(|column| row.plain_char(column))
                })
            });
            let signs = signs.collect::<SmallVec<[Option<char>; 8]>>();
            let sign = signs.first().copied().flatten().filter(|sign| {
                matches!(sign, '+' | '-') && signs.iter().all(|other| *other == Some(*sign))
            });
            return Some((gutter_end, sign));
        }
        leftmost_text = leftmost_text.min(first);
        line_rows.push(row_number);
    }
    None
}

/// The geometry of the line around `pointed_row` whose head is `head_row` (or above the frame),
/// with gutter end `gutter_end` and sign `head_sign`; `None` when the pointed row is not in it.
fn box_line(
    rows: &mut BoxRows<'_, '_>,
    (head_row, gutter_end, head_sign): (Option<u64>, usize, Option<char>),
    pointed_row: u64,
) -> Option<BoxLine> {
    let reach = (MAX_WRAPPED_PATH_ROWS - 1) as u64;
    let line_row = |rows: &mut BoxRows<'_, '_>, row_number: u64| {
        Some(row_number) == head_row
            || rows.get(row_number).is_some_and(|row| row.continues(gutter_end, head_sign))
    };
    if !line_row(rows, pointed_row) {
        // When: `line_row` refuses the pointed row, the line does not hold it.
        return None;
    }
    // The line's rows above the pointer, up to its head, `reach` rows or `MAX_LOGICAL_PATH_BYTES`
    // bytes, past which no URL under the pointer can reach.
    let mut first_row = pointed_row;
    let mut open_above = false;
    let mut above_bytes = 0;
    while Some(first_row) != head_row {
        let past_bound = pointed_row - first_row >= reach || above_bytes > MAX_LOGICAL_PATH_BYTES;
        if first_row == rows.frame.view_top() || past_bound {
            // When: the walk reached the frame's top or a bound, the line continues unread above.
            open_above = true;
            break;
        }
        if !line_row(rows, first_row - 1) {
            // When: `line_row` refuses the row above `first_row`, the pointer is in no line.
            return None;
        }
        first_row -= 1;
        above_bytes += rows.get(first_row).map_or(0, BoxRow::text_len);
    }
    // The line's rows below the pointer, within the same bounds.
    let mut last_row = pointed_row;
    let mut below_bytes = 0;
    let open_below = loop {
        let below = last_row + 1;
        if !rows.frame.contains(below) {
            // When: the next row is outside the frame, only a view with rows below hides more.
            break rows.frame.rows_below();
        }
        if !line_row(rows, below) {
            // When: `line_row` refuses the row `below`, the line ends here unless it soft-wraps.
            break rows.get(below).is_some_and(|row| row.soft_wrapped);
        }
        if below - pointed_row > reach || below_bytes > MAX_LOGICAL_PATH_BYTES {
            // When: `below` passes a bound, the line continues unread below.
            break true;
        }
        last_row = below;
        below_bytes += rows.get(below).map_or(0, BoxRow::text_len);
    };

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
        head_row: head_row.filter(|head_row| *head_row == first_row),
        first_row,
        last_row,
        open_above,
        open_below,
    })
}

/// Rebuild `line` from its rows and decide the URL token under `pointed` in it.
fn box_line_uri(rows: &mut BoxRows<'_, '_>, line: &BoxLine, pointed: AbsoluteCell) -> HardwrapUri {
    // Rebuild the rows: the head from its text start, each continuation from its own. A row that
    // reaches the cut joins the next row directly, as the box cut the source line there; a row that
    // stops short ended at a blank in the source, and a continuation that starts right of the hang
    // column began with one, so one blank separates those rows. A row with no text after its sign
    // adds nothing but that blank.
    let mut text = String::new();
    let mut bytes = Vec::new();
    let mut byte_rows = Vec::new();
    for row_number in line.first_row..=line.last_row {
        let Some(row) = rows.get(row_number) else {
            // When: a line row cannot be read, the line cannot be rebuilt.
            return HardwrapUri::NotApplicable;
        };
        // The head's text follows its sign column; a continuation's follows its repeated sign.
        let head_text_from = line.gutter_end + usize::from(line.head_sign.is_some());
        let start_col = match Some(row_number) == line.head_row {
            true => row.first_after(head_text_from),
            false => row.line_start(line.gutter_end, line.head_sign),
        };
        let row_last = row.text.map_or(0, |(_, last)| last);
        let continuation = Some(row_number) != line.head_row && row_number > line.first_row;
        if continuation && start_col.zip(line.hang).is_some_and(|(start, hang)| start > hang) {
            // A continuation that starts right of the hang column began with a blank in the source.
            text.push(' ');
            bytes.push(LineByte::Blank);
            byte_rows.push(row_number);
        }
        for column in start_col.unwrap_or(row_last + 1)..=row_last {
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
                Some(cell) if box_blank(cell) => LineByte::Blank,
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
        // When: no `pointed_byte` exists, the pointer is on padding, the gutter or a sign.
        return HardwrapUri::NotApplicable;
    };
    let rebuilt = RebuiltLine { text: &text, bytes: &bytes, rows: &byte_rows };
    box_token_uri(rows.frame, line, &rebuilt, pointed_byte)
}

/// One rebuilt line: each byte's character (blanks, prose and unsafe cells as spaces), what each
/// byte is, and the row each came from.
struct RebuiltLine<'line> {
    text: &'line str,
    bytes: &'line [LineByte],
    rows: &'line [u64],
}

/// Decide the URL token around `pointed_byte` of `rebuilt`.
fn box_token_uri(
    frame: &FrameRows<'_>,
    line: &BoxLine,
    rebuilt: &RebuiltLine<'_>,
    pointed_byte: usize,
) -> HardwrapUri {
    let RebuiltLine { text, bytes, rows: byte_rows } = *rebuilt;
    let raw = text.as_bytes();
    let in_token = |index: usize| token_byte(bytes[index], raw[index] as char);
    if !in_token(pointed_byte) {
        // When: `in_token` is false at the pointer, it is on a delimiter, not on URL text.
        return HardwrapUri::NotApplicable;
    }
    let token_start =
        (0..pointed_byte).rev().find(|index| !in_token(*index)).map_or(0, |index| index + 1);
    let token_end =
        (pointed_byte..bytes.len()).find(|index| !in_token(*index)).unwrap_or(bytes.len());
    let token = &bytes[token_start..token_end];
    let open_start = line.open_above && token_start == 0;
    let open_end = line.open_below && token_end == bytes.len();
    let token_text = &text[token_start..token_end];
    let lowered = token_text.to_ascii_lowercase();
    let scheme_at = lowered.find("http://").into_iter().chain(lowered.find("https://")).min();
    if (open_start || open_end) && scheme_at.is_none_or(|at| pointed_byte >= token_start + at) {
        // When: `open_start` or `open_end` holds, the token may run into rows the rebuild did not
        // read, so its extent and scheme are unknown and no prefix of it opens.
        return HardwrapUri::Incomplete;
    }
    let multi_row = byte_rows[token_start] != byte_rows[token_end - 1];
    let Some(scheme_at) = scheme_at.filter(|_| multi_row) else {
        // When: no `scheme_at` exists or `multi_row` is false, the single-row rules decide.
        return HardwrapUri::NotApplicable;
    };
    if pointed_byte < token_start + scheme_at {
        // When: `pointed_byte` is before the scheme, the pointer is on text that leads the URL.
        return HardwrapUri::NotApplicable;
    }
    // From here the token is a URL the box broke across rows: it opens whole or not at all.
    if token.contains(&LineByte::Unsafe) {
        // When: the token holds an unsafe cell, its visible text is not proven whole.
        return HardwrapUri::Incomplete;
    }
    let matches = sonicterm_cfg::url_scan::find_urls(text);
    let Some(found) = matches.iter().find(|found| (found.start..found.end).contains(&pointed_byte))
    else {
        // When: no `found` URL in `matches` covers the pointer, the token is no valid URL as a
        // whole, too long or malformed, so none of it opens.
        return HardwrapUri::Incomplete;
    };
    let row_of = |byte: &LineByte| match byte {
        LineByte::Text(row, _) => Some(*row),
        _ => None,
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
