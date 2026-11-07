//! Bounded reads of a pane's text, taken on a connection thread, never on the event loop.
//!
//! Each read makes exactly one bounded attempt at the pane's parser, visits at most
//! [`MAX_CELLS_PER_READ`] cells while holding it, and copies rows into owned strings before the
//! guard drops. Regex matching and serialization run only after that. A contended parser returns
//! what is known with `truncated` set, so an agent reading in a loop never stalls a frame.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sonicterm_grid::grid::{Grid, RowToken};
use sonicterm_types::CellFlags;
use sonicterm_vt::vt::Parser;

/// How long a read waits for the pane's parser; a miss returns a truncated, empty read.
pub(crate) const PARSER_WAIT: Duration = Duration::from_millis(5);
/// The most cells a read visits while holding the parser.
pub(crate) const MAX_CELLS_PER_READ: usize = 32_768;
/// The default and largest line counts a read returns.
pub(crate) const DEFAULT_LINES: usize = 120;
/// The largest line count a caller may ask for.
pub(crate) const MAX_LINES: usize = 1_000;
/// The default and largest byte counts a read returns.
pub(crate) const DEFAULT_BYTES: usize = 16 * 1024;
/// The largest byte count a caller may ask for.
pub(crate) const MAX_BYTES: usize = 64 * 1024;

/// Which part of a pane a read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadMode {
    /// The visible rows, one line per row, layout kept.
    Screen,
    /// The newest logical lines, soft wraps joined.
    Tail,
    /// The output of the last completed OSC 133 command on the primary screen.
    LastCommand,
}

/// Line and byte caps for one read, already clamped to the maximums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadLimits {
    /// The most lines returned.
    pub(crate) lines: usize,
    /// The most bytes of text returned.
    pub(crate) bytes: usize,
}

impl ReadLimits {
    /// Clamp a caller's requested caps; a missing value takes the default.
    pub(crate) fn clamped(lines: Option<u64>, bytes: Option<u64>) -> Self {
        let clamp = |value: Option<u64>, default: usize, max: usize| {
            value.map_or(default, |value| usize::try_from(value).unwrap_or(max).clamp(1, max))
        };
        Self {
            lines: clamp(lines, DEFAULT_LINES, MAX_LINES),
            bytes: clamp(bytes, DEFAULT_BYTES, MAX_BYTES),
        }
    }
}

/// One line a read returned, with the tokens of its first and last rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadLine {
    /// The line's text, trailing blanks trimmed.
    pub(crate) text: String,
    /// The stable token of the line's last row, which a caller waits past.
    pub(crate) row: RowToken,
    /// The stable token of the line's first row: a line that began at or before a caller's token
    /// is not new, even if output later extended it.
    pub(crate) first_row: RowToken,
}

/// Rows a read covers, oldest to newest, and where the newest one ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowSpan {
    /// The oldest row, absolute.
    pub(crate) start: u64,
    /// The newest row, absolute and inclusive.
    pub(crate) last: u64,
    /// Cells of `last` to read; `None` reads the whole row.
    pub(crate) last_cols: Option<u16>,
}

/// What a read copied out of the pane.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReadSnapshot {
    /// Lines, oldest first.
    pub(crate) lines: Vec<ReadLine>,
    /// The cursor's visible row and column.
    pub(crate) cursor: (u16, u16),
    /// Whether the alternate screen was active.
    pub(crate) alt_screen: bool,
    /// The pane's size in cells.
    pub(crate) size: (u16, u16),
    /// A cap, the cell budget or a busy parser cut the read short.
    pub(crate) truncated: bool,
    /// For `LastCommand`: the command's exit code.
    pub(crate) exit_code: Option<i32>,
    /// For `LastCommand`: the output's first row already left history.
    pub(crate) output_truncated: bool,
    /// For `LastCommand`: no completed command was found.
    pub(crate) no_command: bool,
    /// The parser could not be taken within [`PARSER_WAIT`].
    pub(crate) busy: bool,
    /// The oldest retained row on the active screen when the read was taken; `None` for a busy
    /// read. A wait on a pane with no text yet starts scanning there.
    pub(crate) oldest_row: Option<RowToken>,
    /// The row output is written to next, the cursor's, when the read was taken; `None` for a
    /// busy read. Rows above it can no longer change in appended output.
    pub(crate) writable_row: Option<RowToken>,
    /// What the newest shell marker said about the running shell; `None` while the parser was busy.
    pub(crate) shell_state: Option<sonicterm_vt::vt::ShellIntegrationState>,
}

/// Read `mode` from `parser` within `limits`, holding the parser for one bounded attempt only.
// Only the pane parser is taken, once; no image store or other lock is held with it.
pub(crate) fn read_pane(
    parser: &Arc<Mutex<Parser>>,
    mode: ReadMode,
    limits: ReadLimits,
) -> ReadSnapshot {
    let Some(guard) = parser.try_lock_for(PARSER_WAIT) else {
        // When: `try_lock_for` misses within `PARSER_WAIT`, the read returns busy and truncated.
        return ReadSnapshot { truncated: true, busy: true, ..ReadSnapshot::default() };
    };
    let mut read = snapshot(guard.grid(), mode, limits);
    read.shell_state = Some(guard.shell_integration_state());
    read
}

/// Copy `mode` out of `grid` within `limits`; the caller holds the parser.
pub(crate) fn snapshot(grid: &Grid, mode: ReadMode, limits: ReadLimits) -> ReadSnapshot {
    let mut snapshot = ReadSnapshot {
        cursor: (grid.cursor.row, grid.cursor.col),
        alt_screen: grid.is_alt(),
        size: (grid.cols, grid.rows),
        oldest_row: Some(grid.row_token(0)),
        writable_row: Some(grid.row_token(grid.cursor_absolute_row())),
        ..ReadSnapshot::default()
    };
    let mut budget = Budget { cells: MAX_CELLS_PER_READ, bytes: limits.bytes, lines: limits.lines };
    match mode {
        ReadMode::Screen => {
            let first = grid.scrollback_len() as u64;
            let last = first + u64::from(grid.rows).saturating_sub(1);
            let span = RowSpan { start: first, last, last_cols: None };
            snapshot.lines = collect_rows(grid, span, false, false, &mut budget);
        }
        ReadMode::Tail => {
            let last = grid.scrollback_len() as u64 + u64::from(grid.rows).saturating_sub(1);
            let span = RowSpan { start: 0, last, last_cols: None };
            snapshot.lines = collect_rows(grid, span, true, true, &mut budget);
        }
        ReadMode::LastCommand => match grid.last_completed_command() {
            None => snapshot.no_command = true,
            Some(span) => {
                snapshot.exit_code = span.exit_code;
                snapshot.output_truncated = span.output_truncated;
                // The span indexes the primary screen, which is saved away while `is_alt`.
                // D at column 0 opens an empty row that holds none of the output.
                let last =
                    span.end_row.and_then(|row| row.checked_sub(u64::from(span.end_col == 0)));
                let last_cols = (span.end_col > 0).then_some(span.end_col);
                // A C row that left history leaves the oldest kept row as the start, so the
                // surviving output is still returned, marked `output_truncated`.
                let start = span.output_start_row.unwrap_or(0);
                let rows = last.map(|last| (start, last)).filter(|(start, last)| start <= last);
                if let Some((start, last)) = rows {
                    let rows = RowSpan { start, last, last_cols };
                    let primary = grid.primary_screen();
                    snapshot.lines = collect_rows(primary, rows, true, false, &mut budget);
                }
            }
        },
    }
    snapshot.truncated |= budget.exhausted();
    snapshot
}

/// The text of `span`, oldest first, soft wraps joined, within `limits` and the cell budget; also
/// whether a limit cut it short. An agent run reads its command's output with it.
pub(crate) fn collect_span(grid: &Grid, span: RowSpan, limits: ReadLimits) -> (Vec<String>, bool) {
    let mut budget = Budget { cells: MAX_CELLS_PER_READ, bytes: limits.bytes, lines: limits.lines };
    let lines = collect_rows(grid, span, true, false, &mut budget);
    (lines.into_iter().map(|line| line.text).collect(), budget.exhausted())
}

/// What a read may still spend.
struct Budget {
    /// Cells it may still visit.
    cells: usize,
    /// Bytes of text it may still return.
    bytes: usize,
    /// Lines it may still return.
    lines: usize,
}

impl Budget {
    /// Whether any limit ran out.
    fn exhausted(&self) -> bool {
        self.cells == 0 || self.bytes == 0 || self.lines == 0
    }
}

/// Copy `span`'s rows, newest first, into lines (returned oldest first) until a limit runs out.
/// With `join_wraps`, a row soft-wrapped from the one above joins it into one logical line; with
/// `skip_blank_tail`, blank lines below the newest text are skipped, at the cost of their cells.
/// Every row's cells are charged to the budget before it is copied, so the parser is held for at
/// most [`MAX_CELLS_PER_READ`] cells whatever the screen holds.
fn collect_rows(
    grid: &Grid,
    span: RowSpan,
    join_wraps: bool,
    skip_blank_tail: bool,
    budget: &mut Budget,
) -> Vec<ReadLine> {
    let mut lines: Vec<ReadLine> = Vec::new();
    // Rows of the logical line being built, newest first, and the token of its newest row.
    // Each pending row's text and the blank cells after it, newest first.
    let mut pending: Vec<(String, usize)> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut pending_row: Option<RowToken> = None;
    let mut seen_text = !skip_blank_tail;
    for abs in (span.start..=span.last).rev() {
        let Some(row) = grid.row_at_abs(abs) else {
            // When: `row_at_abs` has no row at `abs`, the range ended early.
            break;
        };
        // Only the newest row can end early, at the column where D arrived.
        let last_cols = span.last_cols.filter(|_| abs == span.last);
        let width = last_cols.map_or(row.len(), usize::from);
        if width > budget.cells {
            // When: the row's `width` is more than the `cells` left, the read stops before it.
            budget.cells = 0;
            break;
        }
        budget.cells -= width;
        pending_row.get_or_insert(grid.row_token(abs));
        // Copying stops at the bytes the read may still return, so a long wrapped line never
        // allocates past the budget under the parser lock.
        let room = budget.bytes.saturating_sub(pending_bytes);
        let copy = row_cells_text_capped(row, width, room);
        // A row older than one already pending sits inside the wrapped line, so its trailing
        // blanks will be joined and are charged now, before any of them is copied; only the
        // newest row's padding stays free.
        let joined_blanks = if pending.is_empty() { 0 } else { copy.trailing_blanks };
        pending_bytes += copy.text.len() + joined_blanks;
        let cut = copy.cut || pending_bytes > budget.bytes;
        pending.push((copy.text, copy.trailing_blanks));
        if cut {
            // When: the row was `cut` at the byte budget, the read stops; the partial line is dropped.
            budget.bytes = 0;
            break;
        }
        if join_wraps && row.soft_wrapped_from_previous() && abs > span.start {
            // When: `soft_wrapped_from_previous` joins this row to the one above, keep building.
            continue;
        }
        // Rows were gathered newest first. Joined oldest first, every row but the newest keeps
        // its trailing blanks, which sit inside the wrapped line; the line's own end is trimmed.
        let mut text = String::new();
        let newest = pending.len().saturating_sub(1);
        for (index, (part, blanks)) in pending.drain(..).rev().enumerate() {
            text.push_str(&part);
            if index < newest {
                text.extend(std::iter::repeat_n(' ', blanks));
            }
        }
        pending_bytes = 0;
        let text = text.trim_end().to_owned();
        let row = pending_row.take().expect("set with the first pending row");
        if !seen_text && text.is_empty() {
            // When: no text was `seen_text` yet, a blank line below the newest output is skipped.
            continue;
        }
        seen_text = true;
        let cost = text.len() + 1;
        if cost > budget.bytes || budget.lines == 0 {
            // When: the line's `cost` passes the byte budget or no `lines` remain, it is not copied.
            budget.bytes = 0;
            break;
        }
        budget.bytes -= cost;
        budget.lines -= 1;
        lines.push(ReadLine { text, row, first_row: grid.row_token(abs) });
    }
    lines.reverse();
    lines
}

/// A row's copied text: its cells up to the last non-blank one, and the blank cells after it.
pub(crate) struct RowCopy {
    /// The text, without the row's trailing blanks.
    pub(crate) text: String,
    /// Blank cells after `text`; they count only when a wrapped continuation follows.
    pub(crate) trailing_blanks: usize,
    /// Copying stopped before passing the byte room.
    pub(crate) cut: bool,
}

/// The text of a row's first `width` cells: each cell's character and the zero-width marks it
/// carries, wide characters once, blanks kept between text. Copying stops before `room` bytes of
/// text would be passed. Trailing blanks are counted, not copied, so a row of padding costs
/// nothing and the caller decides whether a wrap keeps them.
pub(crate) fn row_cells_text_capped(
    row: &sonicterm_grid::line::Line,
    width: usize,
    room: usize,
) -> RowCopy {
    let mut text = String::new();
    let mut blanks = 0usize;
    for cell in row.iter().take(width).filter(|cell| !cell.flags.contains(CellFlags::WIDE_CONT)) {
        let character = if cell.ch == '\0' { ' ' } else { cell.ch };
        let extras = cell.extras().unwrap_or("");
        if character == ' ' && extras.is_empty() {
            // When: the `character` is a blank with no `extras`, it waits for text, so padding is free.
            blanks += 1;
            continue;
        }
        if text.len() + blanks + character.len_utf8() + extras.len() > room {
            // When: this cell would pass the `room` left, copying stops and the row is cut.
            return RowCopy { text, trailing_blanks: 0, cut: true };
        }
        text.extend(std::iter::repeat_n(' ', blanks));
        blanks = 0;
        text.push(character);
        text.push_str(extras);
    }
    RowCopy { text, trailing_blanks: blanks, cut: false }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;
