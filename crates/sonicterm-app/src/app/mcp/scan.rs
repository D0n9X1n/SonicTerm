//! `wait_for`'s forward scan: after its first look at the tail, a wait reads every row written
//! since, oldest first, from a cursor naming the next row it still has to inspect, so output that
//! scrolled past the tail between two polls is still matched. A scan is bounded like a read; a
//! backlog larger than one scan is finished over later polls. Rows that left the screen's retained
//! range before they were scanned are reported, so a timeout never claims they held no match.
//!
//! This scans retained terminal content, not an output journal: rows rewritten in place, erased,
//! or lost across a resize or screen switch between polls may go unseen.

use sonicterm_grid::grid::{Grid, RowToken};

use super::read::{row_cells_text_capped, MAX_BYTES, MAX_CELLS_PER_READ};

/// One logical line a scan copied, soft wraps joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedLine {
    /// The line's text, trailing blanks trimmed.
    pub(crate) text: String,
    /// The token of the line's first row.
    pub(crate) first_row: RowToken,
    /// The token of the line's last row.
    pub(crate) row: RowToken,
    /// The line passed [`MAX_BYTES`] or one scan's cell budget, so `text` is only its start. A cut
    /// prefix is not the line: a regex such as `Z$` could match the prefix and not the line, so
    /// a cut line is never matched.
    pub(crate) cut: bool,
}

/// Where the next scan starts, and how far the wait has looked.
///
/// The two differ: a scan rereads the newest line holding text, since output may still extend it,
/// so it `resume`s on that line's first row; but every row below `frontier` was examined and can
/// no longer change, so only rows at or past `frontier` that leave history count as missed.
/// Output is written at the terminal's cursor, so `frontier` never passes the cursor's row: a
/// blank row at or below it was seen blank but may still be written. This is conservative: blank
/// rows below the cursor that scroll away before a scan reaches them are reported missed, even
/// if nothing was written on them. A row that continues a
/// soft-wrapped line is never a line of its own; a scan entering on one skips the rest of that
/// line, whose start lies before it (before `after_row`, evicted, or already reported cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanCursor {
    /// The first row the next scan reads.
    pub(crate) resume: RowToken,
    /// The first row no scan has examined yet, on `resume`'s screen.
    pub(crate) frontier: u64,
}

impl ScanCursor {
    /// A cursor that both reads from and has examined nothing at or past `row`.
    pub(crate) fn at(row: RowToken) -> Self {
        Self { resume: row, frontier: row.line }
    }
}

/// A wait's cursor on each screen. A screen switch leaves the other screen's cursor in place, so
/// returning to that screen resumes where its scan stopped instead of skipping or rescanning it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScanCursors {
    /// The primary screen's cursor, once the wait has looked at it.
    pub(crate) primary: Option<ScanCursor>,
    /// The alternate screen's cursor, once the wait has looked at it.
    pub(crate) alternate: Option<ScanCursor>,
    /// The primary screen's last scan stopped at its budget, so rows there are still unscanned.
    pub(crate) primary_backlog: bool,
    /// The alternate screen's last scan stopped at its budget.
    pub(crate) alternate_backlog: bool,
}

impl ScanCursors {
    /// The cursor for the screen `alt` names.
    pub(crate) fn get(&self, alt: bool) -> Option<ScanCursor> {
        // When: `alt` names the alternate screen its cursor is returned, else the primary one.
        if alt {
            self.alternate
        } else {
            self.primary
        }
    }

    /// Set the cursor for the screen `alt` names.
    pub(crate) fn set(&mut self, alt: bool, cursor: Option<ScanCursor>) {
        // When: `alt` names the alternate screen its cursor is replaced, else the primary one.
        if alt {
            self.alternate = cursor;
        } else {
            self.primary = cursor;
        }
    }

    /// Record whether the last scan of the screen `alt` names stopped at its budget.
    pub(crate) fn set_backlog(&mut self, alt: bool, backlog: bool) {
        // When: `alt` names the alternate screen its flag is replaced, else the primary one.
        if alt {
            self.alternate_backlog = backlog;
        } else {
            self.primary_backlog = backlog;
        }
    }

    /// Whether the screen `alt` names still has rows its scan has not reached.
    pub(crate) fn backlog(&self, alt: bool) -> bool {
        // When: `alt` names the alternate screen its flag is read, else the primary one.
        if alt {
            self.alternate_backlog
        } else {
            self.primary_backlog
        }
    }

    /// Whether either screen still has rows its scan has not reached; a screen switch leaves the
    /// other screen's backlog standing, so it still counts.
    pub(crate) fn any_backlog(&self) -> bool {
        self.primary_backlog || self.alternate_backlog
    }

    /// Whether the wait has looked at any screen yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.primary.is_none() && self.alternate.is_none()
    }
}

/// What one scan found and where the next one starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanStep {
    /// Logical lines examined, oldest first, including cut ones and the newest, still-open one.
    pub(crate) lines: Vec<ScannedLine>,
    /// Where the next scan starts; `None` when the active screen is not the cursor's.
    pub(crate) next: Option<ScanCursor>,
    /// Rows before the cursor left the retained range before they were scanned.
    pub(crate) missed: bool,
    /// The scan stopped at its cell budget with rows still waiting.
    pub(crate) more: bool,
}

/// The token of the newest row on the active screen.
pub(crate) fn newest_row(grid: &Grid) -> RowToken {
    let last = grid.scrollback_len() as u64 + u64::from(grid.rows).saturating_sub(1);
    grid.row_token(last)
}

/// Scan forward from `cursor` within [`MAX_CELLS_PER_READ`] cells. Every logical line is read
/// from its own first row. The newest line is examined as it stands, so a prompt with no newline
/// after it can match, and the cursor stays on its first row so it is read again once extended.
/// Every call makes progress: a line too long for one scan is reported cut and its remaining rows
/// are skipped. The caller holds the parser.
/// The first row output may still be written to: the cursor's row, which every row above it
/// precedes, on the active screen.
pub(crate) fn writable_from(grid: &Grid) -> u64 {
    grid.row_token(grid.cursor_absolute_row()).line
}

pub(crate) fn scan_from(grid: &Grid, cursor: ScanCursor) -> ScanStep {
    scan_within(grid, cursor, MAX_CELLS_PER_READ)
}

/// [`scan_from`] within a budget of `budget` cells. Every call makes progress: it advances its
/// cursor, reaches the newest row, or reports a row it cannot fit as cut and moves past it, so a
/// wait can never stall on one position. Tests pass a small budget to reach that case, which no
/// grid within `MAX_GRID_AXIS` columns can otherwise reach.
pub(crate) fn scan_within(grid: &Grid, cursor: ScanCursor, budget: usize) -> ScanStep {
    let mut step = ScanStep { lines: Vec::new(), next: None, missed: false, more: false };
    if cursor.resume.alt != grid.is_alt() {
        // When: the cursor names the other screen than `is_alt`, it no longer applies.
        return step;
    }
    let oldest = grid.row_token(0).line;
    let newest = newest_row(grid).line;
    // Only rows no scan examined count as missed; examined history may leave freely.
    step.missed = cursor.frontier < oldest;
    // Rows below the retained range are gone; the scan resumes at the oldest kept row.
    let start_line = cursor.resume.line.max(oldest);
    let at = |line| RowToken { line, alt: cursor.resume.alt };
    let mut resume = at(start_line);
    let mut cells = budget;
    let mut pending: Option<Pending> = None;
    // The first row of the newest line that holds text. Rows below the output are blank screen
    // rows, each a line of its own; the cursor must not pass them, or text written onto the
    // still-open line is never read again.
    let mut last_text: Option<RowToken> = None;
    // The row after the last row reported cut for being wider than a whole scan: no scan can read
    // it, so a scan holding no text resumes past it rather than on it.
    let mut past_cut: Option<u64> = None;
    // An empty range when `start_line` is past `newest`: nothing new was written yet.
    for line in start_line..=newest {
        let abs = line - oldest;
        let Some(row) = grid.row_at_abs(abs) else {
            // When: `row_at_abs` has no row at `abs`, the retained range ended early.
            break;
        };
        let wraps = row.soft_wrapped_from_previous();
        // A row that wraps from a line whose start the scan never read (before the cursor, evicted,
        // or already reported cut) belongs to that line; it is only tested, so it costs one cell.
        if wraps && pending.is_none() {
            // When: the row `wraps` and nothing is `pending`, it continues an unread line: skipped.
            cells = cells.saturating_sub(1);
            resume = at(line + 1);
            if cells == 0 {
                // When: the `cells` budget ran out while skipping, the next scan continues it.
                step.more = true;
                break;
            }
            continue;
        }
        if !wraps {
            // A row that does not wrap from the one above starts a line and closes the pending one.
            if let Some(done) = pending.take() {
                let done = done.close();
                if done.cut {
                    // A cut line is never matched, so rereading it cannot help; the next scan
                    // resumes past its last row instead.
                    past_cut = Some(done.row.line + 1);
                } else if !done.text.is_empty() {
                    // When: the closed line holds `text`, it is the newest reread point so far.
                    last_text = Some(done.first_row);
                }
                step.lines.push(done);
            }
            resume = at(line);
        }
        if row.len() > budget && pending.is_none() {
            // When: one row is wider than a whole scan's `budget`, no scan can ever read it; it is
            // reported cut and skipped with its continuation rows, so the scan still advances.
            let token = grid.row_token(abs);
            step.lines.push(ScannedLine {
                text: String::new(),
                first_row: token,
                row: token,
                cut: true,
            });
            resume = at(line + 1);
            past_cut = Some(line + 1);
            cells = cells.saturating_sub(1);
            if cells == 0 {
                // When: the `cells` budget ran out on cut rows, the next scan continues past them.
                step.more = true;
                break;
            }
            continue;
        }
        if row.len() > cells {
            // When: the row is wider than the `cells` left, this scan ends here.
            step.more = true;
            if let Some(open) = pending.take() {
                if open.first_row.line == start_line {
                    // The line began this scan and still outgrew it, so it can never fit one
                    // scan: it is reported cut, and the next scan skips its remaining rows.
                    let mut done = open.close();
                    done.cut = true;
                    step.lines.push(done);
                    resume = at(line);
                } else {
                    // When: an earlier line ended this scan, the next scan rereads `open` whole.
                    resume = open.first_row;
                }
            }
            break;
        }
        cells -= row.len();
        let token = grid.row_token(abs);
        pending.get_or_insert_with(|| Pending::new(token)).push(row, token);
    }
    if !step.more {
        // When: the scan reached `newest` without stopping (`!step.more`), every row was examined.
        if let Some(open) = pending {
            // The newest line is examined as it stands; output may still extend it.
            let open = open.close();
            if open.cut {
                // The open line is already cut, and more output cannot make it matchable, so the
                // next scan resumes past its last row.
                past_cut = Some(open.row.line + 1);
            } else if !open.text.is_empty() {
                // When: the open line holds `text`, output may still extend it: it is reread.
                last_text = Some(open.first_row);
            }
            step.lines.push(open);
        }
        // The next scan starts on the newest line holding text, which output may still extend,
        // and covers the blank rows after it, where output is written; with no text yet, it starts
        // where this one did. It never starts on or before a cut line, which no scan can match.
        let floor = past_cut.unwrap_or(start_line).max(start_line);
        resume = last_text.filter(|text| text.line >= floor).unwrap_or_else(|| at(floor));
        // Every row through `newest` was examined; those above the cursor can no longer change.
        let frontier = cursor.frontier.max(writable_from(grid));
        step.next = Some(ScanCursor { resume, frontier });
        return step;
    }
    // A scan stopped at its budget examined nothing whole at or past where the next one resumes;
    // coverage already established is kept, so a rescan never makes examined rows count as missed.
    let frontier = cursor.frontier.max(resume.line.min(writable_from(grid)));
    step.next = Some(ScanCursor { resume, frontier });
    step
}

/// A logical line a scan is building.
struct Pending {
    /// Text so far, without the newest row's trailing blanks.
    text: String,
    /// Blank cells that ended the newest row; they belong inside the line only if a wrap follows.
    trailing_blanks: usize,
    /// At least one row was appended, even a blank one, so a following row continues it.
    has_row: bool,
    /// The line passed [`MAX_BYTES`] of text.
    cut: bool,
    /// The line's first row.
    first_row: RowToken,
    /// The line's newest row so far.
    row: RowToken,
}

impl Pending {
    /// An empty line starting at `first_row`.
    fn new(first_row: RowToken) -> Self {
        Self {
            text: String::new(),
            trailing_blanks: 0,
            has_row: false,
            cut: false,
            first_row,
            row: first_row,
        }
    }

    /// Append `row`, whose token is `token`; past [`MAX_BYTES`] of text the line is marked cut.
    fn push(&mut self, row: &sonicterm_grid::line::Line, token: RowToken) {
        self.row = token;
        if self.cut {
            // When: the line is already `cut`, its remaining rows add nothing.
            return;
        }
        if self.has_row {
            // When: a row was already appended (`has_row`), even a blank one, the row above
            // wrapped into this one, so its trailing blanks sit inside the line.
            let room = MAX_BYTES.saturating_sub(self.text.len());
            if self.trailing_blanks > room {
                // When: the joined blanks pass the `room` left, the line is cut here.
                self.cut = true;
                return;
            }
            self.text.extend(std::iter::repeat_n(' ', self.trailing_blanks));
        }
        self.has_row = true;
        let room = MAX_BYTES.saturating_sub(self.text.len());
        let copy = row_cells_text_capped(row, row.len(), room);
        self.text.push_str(&copy.text);
        self.trailing_blanks = copy.trailing_blanks;
        self.cut |= copy.cut;
    }

    /// The finished line, its end trimmed.
    fn close(self) -> ScannedLine {
        let text = self.text.trim_end().to_owned();
        ScannedLine { text, first_row: self.first_row, row: self.row, cut: self.cut }
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod scan_tests;

#[cfg(test)]
#[path = "scan_wait_tests.rs"]
mod scan_wait_tests;
