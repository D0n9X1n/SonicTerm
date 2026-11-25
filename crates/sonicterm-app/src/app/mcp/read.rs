//! Bounded reads of a pane's text, taken on a connection thread, never on the event loop.
//!
//! Each read makes exactly one bounded attempt at the pane's parser, visits at most
//! [`MAX_CELLS_PER_READ`] cells while holding it, and copies rows into owned strings before the
//! guard drops. Regex matching and serialization run only after that. A contended parser returns
//! what is known with `truncated` set, so an agent reading in a loop never stalls a frame.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sonicterm_grid::grid::{CommandSpan, Grid, RowToken};
use sonicterm_types::CellFlags;
use sonicterm_vt::vt::Parser;

/// How long a read waits for the pane's parser; a miss returns a truncated, empty read.
pub(crate) const PARSER_WAIT: Duration = Duration::from_millis(5);
/// The most cells a read visits while holding the parser.
pub(crate) const MAX_CELLS_PER_READ: usize = 32_768;
// A search call rereads one overlap row before the rows it owns, so two of the widest rows must
// fit one call's cells; otherwise the overlap alone could spend the budget and the cursor stall.
const _: () = assert!(2 * sonicterm_grid::grid::MAX_GRID_AXIS as usize <= MAX_CELLS_PER_READ);
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
    /// The output of a completed OSC 133 command on the primary screen: region `seq`, or the last
    /// one when it is absent.
    LastCommand {
        /// The region whose output is read, a `seq` from `command_history`.
        seq: Option<u64>,
    },
    /// Logical lines of the active screen that begin before `before`, or the newest lines when it
    /// is absent, oldest first, soft wraps joined: one page of the retained history.
    History {
        /// The row the page ends before: the `first_row` of the previous page.
        before: Option<RowToken>,
    },
    /// A history page with one reread row of newer context when `before` splits a logical line.
    Search {
        /// Match starts belong strictly before this row; the row itself is context only.
        before: Option<RowToken>,
    },
}

/// Why a read could not start where its caller asked: a history page's cursor, or a command's
/// `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryRefusal {
    /// The token names a row of the screen that is not active.
    OtherScreen,
    /// The token's row left history; paging restarts from the newest line.
    Evicted,
    /// The command `seq` names is no longer retained: its rows left history or newer prompts
    /// pushed it out.
    CommandEvicted,
}

impl HistoryRefusal {
    /// The refusal's error code in results.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OtherScreen => "other_screen",
            Self::Evicted | Self::CommandEvicted => "evicted",
        }
    }

    /// A sentence explaining the refusal to the agent.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::OtherScreen => "before_row belongs to the other screen",
            Self::Evicted => "before_row left history; start again from the newest line",
            Self::CommandEvicted => "seq is no longer retained; list the commands again",
        }
    }
}

/// Line and byte caps for one read, already clamped to the maximums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadLimits {
    /// The most lines returned.
    pub(crate) lines: usize,
    /// The byte budget; an otherwise empty history page admits one whole row even past it.
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
    /// The line's text, with trailing blanks trimmed only at its real logical end.
    pub(crate) text: String,
    /// Byte offset where the newest row starts; search excludes match starts in that row when
    /// it was reread solely as context beyond the caller's cursor.
    pub(crate) newest_row_start: usize,
    /// The stable token of the line's last row, which a caller waits past.
    pub(crate) row: RowToken,
    /// The stable token of the line's first row: a line that began at or before a caller's token
    /// is not new, even if output later extended it.
    pub(crate) first_row: RowToken,
    /// The text is only part of its logical line: the line alone outgrew the read, the read ended
    /// inside it, or its start left history.
    pub(crate) cut: bool,
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
    /// The first row of the oldest line the read covered, returned or skipped as blank; the next
    /// history page ends before it. `None` when the read covered no row.
    pub(crate) first_row: Option<RowToken>,
    /// The read reached the oldest retained row of the screen it read, so no older page remains.
    pub(crate) at_start: bool,
    /// For `History`, `Search` or `LastCommand` with a `seq`: the cursor or `seq` could not start a read, and
    /// nothing was read.
    pub(crate) refused: Option<HistoryRefusal>,
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
    let tail =
        Walk { join_wraps: true, skip_blank_tail: true, keep_cut: false, search_overlap: false };
    match mode {
        ReadMode::Screen => {
            let first = grid.scrollback_len() as u64;
            let last = first + u64::from(grid.rows).saturating_sub(1);
            let span = RowSpan { start: first, last, last_cols: None };
            let walk = Walk {
                join_wraps: false,
                skip_blank_tail: false,
                keep_cut: false,
                search_overlap: false,
            };
            collect_rows(grid, span, walk, &mut budget).fill(&mut snapshot);
        }
        ReadMode::Tail => {
            let span = RowSpan { start: 0, last: newest_abs(grid), last_cols: None };
            collect_rows(grid, span, tail, &mut budget).fill(&mut snapshot);
        }
        ReadMode::History { before } | ReadMode::Search { before } => {
            match history_end(grid, before) {
                Err(refusal) => snapshot.refused = Some(refusal),
                // `history_end` found no row before `before`: the page is empty, at the start.
                Ok(None) => snapshot.at_start = true,
                Ok(Some(last)) => {
                    // Blank rows under the cursor are skipped only on the first page; once paging,
                    // a blank line is part of the history like any other.
                    let overlap = matches!(mode, ReadMode::Search { before: Some(_) })
                        && grid
                            .row_at_abs(last + 1)
                            .is_some_and(|row| row.soft_wrapped_from_previous());
                    let walk = Walk {
                        skip_blank_tail: before.is_none(),
                        keep_cut: true,
                        search_overlap: overlap,
                        ..tail
                    };
                    let last = last + u64::from(overlap);
                    let span = RowSpan { start: 0, last, last_cols: None };
                    collect_rows(grid, span, walk, &mut budget).fill(&mut snapshot);
                }
            }
        }
        ReadMode::LastCommand { seq } => match command_span(grid, seq) {
            Err(refusal) => snapshot.refused = Some(refusal),
            Ok(None) => snapshot.no_command = true,
            Ok(Some(span)) => {
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
                    let walk = Walk { skip_blank_tail: false, ..tail };
                    collect_rows(primary, rows, walk, &mut budget).fill(&mut snapshot);
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
    let walk =
        Walk { join_wraps: true, skip_blank_tail: false, keep_cut: false, search_overlap: false };
    let collected = collect_rows(grid, span, walk, &mut budget);
    (collected.lines.into_iter().map(|line| line.text).collect(), budget.exhausted())
}

/// The span `LastCommand` reads: region `seq`'s, or the last completed command's when absent.
/// `Ok(None)` when no such command finished yet; a `seq` the primary screen no longer retains is
/// refused, so an agent can tell a forgotten command from one still running.
fn command_span(grid: &Grid, seq: Option<u64>) -> Result<Option<CommandSpan>, HistoryRefusal> {
    let Some(seq) = seq else {
        // When: no `seq` was given, the last completed command is read.
        return Ok(grid.last_completed_command());
    };
    if let Some(span) = grid.completed_command(seq) {
        // When: `completed_command` found region `seq` with C and D, its span is read.
        return Ok(Some(span));
    }
    let retained = grid.primary_screen().prompts().any(|region| region.seq == seq);
    if retained {
        Ok(None)
    } else {
        // When: no retained region has `seq`, it was evicted (or never named a primary prompt).
        Err(HistoryRefusal::CommandEvicted)
    }
}

/// The absolute index of the newest row on the active screen.
fn newest_abs(grid: &Grid) -> u64 {
    grid.scrollback_len() as u64 + u64::from(grid.rows).saturating_sub(1)
}

/// The newest row a history page reads: the row before `before`, or the newest row when it is
/// absent; `Ok(None)` when `before` is the oldest retained row, so nothing older remains.
fn history_end(grid: &Grid, before: Option<RowToken>) -> Result<Option<u64>, HistoryRefusal> {
    let Some(before) = before else {
        // When: no `before` token was given, the first page ends at the newest row.
        return Ok(Some(newest_abs(grid)));
    };
    if before.alt != grid.is_alt() {
        // When: `before.alt` names the screen that is not active, its rows are not this screen's.
        return Err(HistoryRefusal::OtherScreen);
    }
    match grid.resolve_row(before) {
        Some(abs) => Ok(abs.checked_sub(1)),
        // The token's line is older than the oldest retained row: its row was evicted.
        None if before.line < grid.row_token(0).line => Err(HistoryRefusal::Evicted),
        // A token past the newest row holds nothing newer back, so the page ends at the newest row.
        None => Ok(Some(newest_abs(grid))),
    }
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

/// How [`collect_rows`] walks a span.
#[derive(Debug, Clone, Copy)]
struct Walk {
    /// A row soft-wrapped from the one above joins it into one logical line.
    join_wraps: bool,
    /// Blank lines below the newest text are skipped, at the cost of their cells.
    skip_blank_tail: bool,
    /// The newest row is reread search context; it cannot prevent copying the first owned row.
    search_overlap: bool,
    /// A limit that stops the walk before any line was returned leaves the rows already copied
    /// whole as one cut line, and lines the span's ends split are marked cut, so a history page
    /// always makes progress and never passes a fragment off as a whole line.
    keep_cut: bool,
}

/// What [`collect_rows`] copied and how far back it reached.
struct Collected {
    /// Lines, oldest first.
    lines: Vec<ReadLine>,
    /// The first row of the oldest line covered, returned or skipped as blank.
    covered: Option<RowToken>,
    /// The oldest covered row is the oldest retained row of the grid walked.
    at_start: bool,
}

impl Collected {
    /// Move the lines and how far back they reach into `snapshot`.
    fn fill(self, snapshot: &mut ReadSnapshot) {
        snapshot.lines = self.lines;
        snapshot.first_row = self.covered;
        snapshot.at_start = self.at_start;
    }
}

/// One row of the logical line being built.
struct PendingRow {
    /// The row's text, without its trailing blanks.
    text: String,
    /// Blank cells after `text`; they sit inside the line when a wrapped row follows.
    blanks: usize,
    /// The row's absolute index.
    abs: u64,
}

/// Copy `span`'s rows, newest first, into lines (returned oldest first) until a limit runs out.
/// `walk` says whether soft wraps join, whether blank lines below the newest text are skipped, and
/// whether a cut line is kept. Every row's cells are charged to the budget before it is copied, so
/// the parser is held for at most [`MAX_CELLS_PER_READ`] cells whatever the screen holds.
fn collect_rows(grid: &Grid, span: RowSpan, walk: Walk, budget: &mut Budget) -> Collected {
    let mut lines: Vec<ReadLine> = Vec::new();
    // Rows of the logical line being built, newest first.
    let mut pending: Vec<PendingRow> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut seen_text = !walk.skip_blank_tail;
    let mut covered: Option<u64> = None;
    // A span ending before a row that wraps from it ends inside a logical line, so the newest line
    // returned is only that line's start.
    let mut newest_cut = walk.keep_cut
        && grid.row_at_abs(span.last + 1).is_some_and(|row| row.soft_wrapped_from_previous());
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
        // Row tokens cannot resume inside a row: an empty history page admits one whole row,
        // even past its byte cap. Every later row stays within the remaining room.
        let context_row = walk.search_overlap && abs == span.last;
        let only_context = walk.search_overlap && pending.len() == 1 && pending[0].abs == span.last;
        let room = if walk.keep_cut
            && lines.is_empty()
            && !context_row
            && (pending.is_empty() || only_context)
        {
            usize::MAX
        } else {
            // When: this is context or an additional row, reserve the pending text and its newline.
            budget.bytes.saturating_sub(pending_bytes + 1)
        };
        let copy = row_cells_text_capped(row, width, room);
        if context_row && (copy.cut || copy.text.len() + copy.trailing_blanks + 1 > budget.bytes) {
            // When: the context row exhausts the byte budget, discard it so older owned rows advance.
            // shortcut: overlap is omitted when its row leaves no byte room for an owned row.
            newest_cut = true;
            continue;
        }
        if only_context && pending_bytes + copy.text.len() + copy.trailing_blanks + 1 > budget.bytes
        {
            // Context leaves no room for the first owned row; keep that row instead of stalling.
            pending.clear();
            pending_bytes = 0;
            newest_cut = true;
        }
        // A row older than one already pending sits inside the wrapped line, so its trailing
        // blanks will be joined and are charged now, before any of them is copied; only the
        // newest row's padding stays free unless the logical line continues beyond the page.
        let joined_blanks =
            if pending.is_empty() && !newest_cut { 0 } else { copy.trailing_blanks };
        pending_bytes += copy.text.len() + joined_blanks;
        if copy.cut || pending_bytes + 1 > budget.bytes {
            // When: the row was `cut` or passes the byte budget, stop before it unless this empty
            // history page admitted the whole row to preserve progress without losing its suffix.
            budget.bytes = 0;
            if walk.keep_cut && lines.is_empty() && pending.is_empty() {
                pending.push(PendingRow { text: copy.text, blanks: copy.trailing_blanks, abs });
            }
            break;
        }
        pending.push(PendingRow { text: copy.text, blanks: copy.trailing_blanks, abs });
        let wraps = row.soft_wrapped_from_previous();
        if walk.join_wraps && wraps && abs > span.start {
            // When: `join_wraps` is on and the row `wraps` from one inside the span, keep building.
            continue;
        }
        // The span's oldest row continuing an older one means the line's start lies outside it.
        let starts_outside = walk.keep_cut && wraps;
        pending_bytes = 0;
        let (mut line, oldest) = join_rows(grid, &mut pending, newest_cut);
        if !seen_text && line.text.is_empty() {
            // When: no text was `seen_text` yet, a blank line below the newest output is skipped.
            covered = Some(oldest);
            continue;
        }
        seen_text = true;
        let cost = line.text.len() + 1;
        if cost > budget.bytes || budget.lines == 0 {
            // When: the line's `cost` passes the byte budget or no `lines` remain, it is not copied.
            budget.bytes = 0;
            break;
        }
        budget.bytes -= cost;
        budget.lines -= 1;
        line.cut = starts_outside || std::mem::take(&mut newest_cut);
        covered = Some(oldest);
        lines.push(line);
    }
    if walk.keep_cut && lines.is_empty() && !pending.is_empty() {
        // A limit stopped the walk inside the first line: the whole rows already copied are
        // returned cut as the line's newest part, so the next page continues inside it.
        let (mut line, oldest) = join_rows(grid, &mut pending, newest_cut);
        line.cut = true;
        covered = Some(oldest);
        lines.push(line);
    }
    lines.reverse();
    Collected {
        lines,
        covered: covered.map(|abs| grid.row_token(abs)),
        at_start: covered == Some(0),
    }
}

/// Join `pending` (newest first) oldest first into one line and empty it; also return the line's
/// oldest row. Interior trailing blanks survive, including the newest row's when `newest_cut`
/// means the logical line continues past this page. Only a real line end is trimmed.
fn join_rows(grid: &Grid, pending: &mut Vec<PendingRow>, newest_cut: bool) -> (ReadLine, u64) {
    let newest = pending.first().map_or(0, |part| part.abs);
    let oldest = pending.last().map_or(newest, |part| part.abs);
    let newest_row_start: usize =
        pending.iter().skip(1).map(|part| part.text.len() + part.blanks).sum();
    let mut text = String::new();
    let newest_index = pending.len().saturating_sub(1);
    for (index, part) in pending.drain(..).rev().enumerate() {
        text.push_str(&part.text);
        if index < newest_index || newest_cut {
            text.extend(std::iter::repeat_n(' ', part.blanks));
        }
    }
    if !newest_cut {
        // The newest row ends the logical line, so only its real trailing padding is removed.
        text.truncate(text.trim_end().len());
    }
    // Trimming a blank newest row can also remove blanks from the preceding row.
    let newest_row_start = newest_row_start.min(text.len());
    let line = ReadLine {
        text,
        newest_row_start,
        row: grid.row_token(newest),
        first_row: grid.row_token(oldest),
        cut: false,
    };
    (line, oldest)
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
    row_cells_text_from(row, 0, width, room)
}

/// [`row_cells_text_capped`] for the cells from `start_col` up to `width`, so a typed command is
/// read from the column where its input began.
pub(crate) fn row_cells_text_from(
    row: &sonicterm_grid::line::Line,
    start_col: usize,
    width: usize,
    room: usize,
) -> RowCopy {
    let mut text = String::new();
    let mut blanks = 0usize;
    let cells = row.iter().take(width).skip(start_col);
    for cell in cells.filter(|cell| !cell.flags.contains(CellFlags::WIDE_CONT)) {
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

/// A pane's OSC 7501 records, oldest change first: typed state, kind and progress, plus id, app,
/// title and message only under the user's `program_status_text` opt-in. Untrusted display data.
pub(crate) fn program_status_json(
    status: &sonicterm_vt::vt::ProgramStatus,
    status_text: bool,
) -> serde_json::Value {
    let records = status.records().map(|record| {
        let mut entry = serde_json::json!({
            "state": record.state().as_str(),
            "kind": record.kind().map(sonicterm_vt::vt::StatusKind::as_str),
            "progress": record.progress(),
        });
        if status_text {
            // The user opted in, so the record's program-supplied text is listed too.
            entry["id"] = serde_json::json!(record.id());
            entry["app"] = serde_json::json!(record.app());
            entry["title"] = serde_json::json!(record.title());
            entry["msg"] = serde_json::json!(record.msg());
        }
        entry
    });
    serde_json::Value::Array(records.collect())
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;

#[cfg(test)]
#[path = "read_history_tests.rs"]
mod read_history_tests;
