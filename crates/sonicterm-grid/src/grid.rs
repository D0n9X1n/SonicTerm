//! Terminal screen grid: cells, attributes, scrollback.

use std::collections::{HashSet, VecDeque};

// Value types live in `sonicterm-types` so non-engine crates can use them
// without depending on this crate. Re-exported here for source compatibility:
// every existing `use sonicterm_grid::grid::{Cell, CellFlags, Color, Pos}` keeps
// compiling unchanged.
pub use sonicterm_types::{Cell, CellFlags, Color, Pos, UnderlineStyle};
// Governor accounting type, consumed rather than republished: the grid reports
// what it retains, it does not own the resource contract.
use sonicterm_types::ResourceAmount;

use crate::hyperlink::HyperlinkId;
use crate::line::{Line, MAX_LINE_CONTENT_SEQ};

/// A row of cells.
///
/// The public type alias is `Line`. Public accessors
/// (`row`, `row_mut`, `rows_iter`, `scrollback_row`, `scrollback_iter`,
/// `row_at_abs`) return `&Line` / `&mut Line` directly. Callers index cells
/// via `Line::Index<usize>` / `Index<Range<usize>>`, iterate via
/// `Line::iter`, and use `Line::len`. A row may be stored flat, as clusters or
/// trimmed; the read API is the same for each.
pub type Row = Line;

/// Maximum column or row count accepted by a grid allocation.
pub const MAX_GRID_AXIS: u16 = 4096;
/// Maximum visible + scrollback cells retained by one grid (roughly 24 MiB at
/// 24 bytes/cell, excluding container overhead).
pub const MAX_GRID_CELLS: u32 = 1024 * 1024;
/// Maximum visible cells in one primary or alternate screen.
pub const MAX_VISIBLE_GRID_CELLS: u32 = MAX_GRID_CELLS / 2;
/// Maximum UTF-8 bytes of zero-width cluster data retained by one cell.
pub const MAX_CELL_EXTRAS_BYTES: usize = 64;

/// A grid's retained storage split by the region holding it.
///
/// The three parts are disjoint and sum to [`Grid::retained_amount`], so a
/// caller may charge them to separate `ResourceClass` rows without changing
/// the total charged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridRegionAmounts {
    /// The on-screen rows, their prompt regions, and their rare-attribute
    /// boxes.
    pub visible: ResourceAmount,
    /// Scrollback history.
    pub history: ResourceAmount,
    /// The saved primary screen held while an alternate screen is active.
    ///
    /// Named for the `ResourceClass` it maps to. What is stored is the
    /// *primary* screen the alternate displaced — the alternate screen itself
    /// is the live `visible`/`history` pair while it is showing.
    pub alternate: ResourceAmount,
}

impl GridRegionAmounts {
    /// Sum of the three regions, equal to [`Grid::retained_amount`].
    #[must_use]
    pub fn total(&self) -> ResourceAmount {
        [self.visible, self.history, self.alternate].into_iter().fold(
            ResourceAmount::default(),
            |acc, part| ResourceAmount {
                bytes: acc.bytes.saturating_add(part.bytes),
                items: acc.items.saturating_add(part.items),
            },
        )
    }
}

/// Scrolls between retained-budget checks on the scroll path.
///
/// The check walks every row, so running it per scroll would make a long
/// `cat` quadratic in history depth. This bounds the overshoot to roughly this
/// many rows of content — kilobytes at ordinary widths — while keeping
/// scrolling linear.
const ROWS_BETWEEN_BUDGET_CHECKS: usize = 512;

/// Smallest aggregate capacity excess worth a reallocation pass.
///
/// Below this, walking every row to reclaim it costs more than the memory is
/// worth. Sized so an ordinary screen-sized overshoot is ignored and a
/// scrollback-sized one is not.
const CAPACITY_EXCESS_FLOOR_BYTES: usize = 512 * 1024;

/// Excess must exceed live bytes divided by this before rows are compacted.
///
/// Four gives a 25% band. The hysteresis is the point: a user dragging a
/// window edge emits a continuous stream of ±1 resizes, and a threshold
/// tight enough to fire on each one would reallocate every row per frame.
const CAPACITY_EXCESS_DIVISOR: usize = 4;

/// Clamp requested grid dimensions to a non-zero, memory-bounded shape.
///
/// Columns are bounded first, then rows are reduced to keep the total cell
/// count within [`MAX_GRID_CELLS`]. This preserves wide-terminal behavior
/// while preventing malformed window/config dimensions from allocating
/// multi-gigabyte grids.
#[must_use]
pub fn bounded_grid_size(cols: u64, rows: u64) -> (u16, u16) {
    let cols = cols.clamp(1, u64::from(MAX_GRID_AXIS)) as u16;
    let row_cap =
        (MAX_VISIBLE_GRID_CELLS / u32::from(cols)).clamp(1, u32::from(MAX_GRID_AXIS)) as u16;
    let rows = rows.clamp(1, u64::from(row_cap)) as u16;
    (cols, rows)
}

/// Clamp retained scrollback rows so visible cells plus history share one
/// per-pane memory budget.
#[must_use]
pub fn bounded_scrollback_rows(cols: u16, visible_rows: u16, requested_rows: usize) -> usize {
    let cols = u64::from(cols.max(1));
    let visible_cells = cols.saturating_mul(u64::from(visible_rows));
    let remaining_cells = u64::from(MAX_GRID_CELLS).saturating_sub(visible_cells);
    let row_budget = remaining_cells / cols;
    requested_rows.min(usize::try_from(row_budget).unwrap_or(usize::MAX))
}

/// A single shell prompt region recorded from OSC 133 markers. Rows are
/// expressed in **scrollback-absolute coordinates** — `scrollback_len() +
/// visible_row` at the time the marker was emitted — so the region remains
/// addressable after content scrolls into history. Callers convert back to
/// a visible row by subtracting the current `scrollback_len()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptRegion {
    /// Absolute row of the prompt start (OSC 133 ; A).
    pub start_row: u64,
    /// Absolute row where the command output ended (OSC 133 ; D), if any.
    pub end_row: Option<u64>,
    /// Exit code reported by `OSC 133 ; D ; <code>`, if any.
    pub exit_code: Option<i32>,
}

/// Maximum number of prompt regions retained per grid. Older ones are
/// discarded FIFO — terminals only need scroll-by-prompt access to the
/// recent past.
pub const PROMPT_REGION_LIMIT: usize = 256;

/// A set of visible row slots, as a bitset; the rows a presented frame drew, for
/// [`Grid::clear_dirty_rows`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowSet {
    words: Vec<u64>,
}

impl RowSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add row slot `row`.
    pub fn insert(&mut self, row: usize) {
        let word = row / 64;
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1 << (row % 64);
    }

    /// Whether row slot `row` is in the set.
    pub fn contains(&self, row: usize) -> bool {
        self.words.get(row / 64).is_some_and(|word| word & (1 << (row % 64)) != 0)
    }

    /// The row slots in the set, ascending.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(word_index, word)| {
            (0..64).filter(move |bit| word & (1 << bit) != 0).map(move |bit| word_index * 64 + bit)
        })
    }

    /// Whether the set holds no row.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }
}

impl FromIterator<usize> for RowSet {
    fn from_iter<Rows: IntoIterator<Item = usize>>(rows: Rows) -> Self {
        let mut set = RowSet::new();
        for row in rows {
            set.insert(row);
        }
        set
    }
}

/// Terminal grid with scrollback.
#[derive(Debug)]
pub struct Grid {
    /// Number of columns in the visible region.
    pub cols: u16,
    /// Number of rows in the visible region.
    pub rows: u16,
    /// Visible region: `rows` rows of `cols` cells.
    ///
    /// `VecDeque` (rather than `Vec`) so that `scroll_up`/`scroll_down`
    /// are O(1) amortized — a single `pop_front` + `push_back` rotates
    /// the ring-buffer head rather than memmove-ing every row. At 200×
    /// rows this turns a 200-row × N-cell memcpy into a pointer bump.
    /// The indexing API is unchanged: `VecDeque` implements `Index<usize>`
    /// and `IntoIterator`, so all existing callers compile untouched.
    visible: VecDeque<Line>,
    /// Scrollback buffer (oldest at front).
    scrollback: VecDeque<Line>,
    /// User-requested history depth before applying the total cell budget.
    scrollback_requested_limit: usize,
    scrollback_limit: usize,
    /// Cursor position within the visible region.
    pub cursor: Pos,
    /// Default attributes used for new cells.
    pub default: Cell,
    /// Saved primary screen when the alt screen is active.
    alt_screen: Option<Box<Grid>>,
    /// Per-grid wrapping revision used for coarse renderer change detection.
    ///
    /// Mutation paths that affect rendered terminal state advance it. Dirty-only
    /// invalidation and metadata maintenance may leave it unchanged.
    revision: u64,
    /// Monotonic sequence for cell-content changes only.
    ///
    /// Unlike `revision` and `dirty_rows`, cursor motion and presentation-only
    /// invalidation do not advance this. Selections record the current value and
    /// ask which rows changed afterward, matching WezTerm's selection seqno
    /// policy without clearing on a blinking cursor or theme redraw.
    content_seq: u64,
    /// Monotonic identity of the active primary or alternate screen incarnation.
    ///
    /// Every real screen transition advances it, including a complete
    /// primary-to-alternate-to-primary round trip whose restored cells compare
    /// equal. Selection fingerprints retain this value so buffer ABA cannot make
    /// replacement screen identity look unchanged.
    screen_epoch: u64,
    /// Resizes that changed the bounded rows or columns; see [`Grid::size_generation`].
    size_generation: u64,
    /// Last content sequence for each visible row. This follows row identity
    /// during primary-screen scrolling, where `scrollback_len()` advances with
    /// the text, but screen-position scrolls (alternate screen and DECSTBM
    /// regions) restamp the affected positions.
    row_content_seq: VecDeque<u64>,
    /// Rows pushed into scrollback since the budget was last checked.
    ///
    /// The budget check walks every row, so running it per scroll would make
    /// `cat largefile` quadratic in history depth. Counting rows and checking
    /// at a boundary makes it amortized: one walk per this many scrolls.
    rows_since_budget_check: usize,
    /// Monotonically increasing count of rows dropped from scrollback.
    ///
    /// Evicting a row destroys the only references its cells held, so this is
    /// the signal that per-cell garbage may now exist. Consumers that scan the
    /// grid to reclaim something — the hyperlink registry does — compare it
    /// against the value at their last scan to tell "nothing has changed since
    /// I last looked" from "the grid has moved on". Never decremented, so a
    /// missed bump costs eagerness, never correctness.
    scrollback_evicted: u64,
    /// Rows scrolled off the top of the full screen over this grid's lifetime; see
    /// [`Grid::lines_scrolled`].
    lines_scrolled: u64,
    /// The last alternate screen's final scroll count, so the next one starts past it; see
    /// [`Grid::lines_scrolled`].
    alt_scroll_floor: u64,
    /// Rows dropped only because retained bytes passed the grid's byte budget; see
    /// [`Grid::budget_trimmed_rows`].
    budget_trimmed_rows: u64,
    /// `budget_trimmed_rows` when scrollback was last cleared; see [`Grid::budget_trimmed_at_clear`].
    budget_trimmed_cleared: u64,
    /// Row-storage buffers the scroll path created or grew; see [`Grid::row_storage_allocs`].
    row_storage_allocs: u64,
    /// Per-row dirty bitset. `true` means the row has been mutated since
    /// the last `clear_dirty()` and the renderer must re-shape it; `false`
    /// means the renderer may reuse its cached span data for that row.
    /// `Vec<bool>` is fine at terminal row counts (~40 typical, ~200 max)
    /// — a BitSet has worse cache behavior at this scale.
    dirty_rows: Vec<bool>,
    /// Advances on every write that sets a `dirty_rows` bit, even one already set; clearing never
    /// advances it. A renderer that presented a frame compares it to tell "the dirt I drew" from
    /// dirt written after the frame was assembled.
    dirty_generation: u64,
    /// Per row, the `dirty_generation` its dirty bit was last set at. A renderer that assembled at
    /// generation `g` may clear exactly the rows stamped at or before `g`; a row set after that
    /// carries a newer stamp and stays dirty.
    dirty_stamps: Vec<u64>,
    /// Prompt regions recorded from OSC 133. Oldest first.
    prompts: VecDeque<PromptRegion>,
    autowrap: bool,
    pending_wrap: bool,
}

impl Grid {
    /// Create a new grid with the given column/row count and default settings.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = bounded_grid_size(u64::from(cols), u64::from(rows));
        let scrollback_requested_limit = 1_000;
        let scrollback_limit = bounded_scrollback_rows(cols, rows, scrollback_requested_limit);
        let visible = (0..rows).map(|_| make_row(cols)).collect();
        Self {
            cols,
            rows,
            visible,
            scrollback: VecDeque::new(),
            scrollback_requested_limit,
            scrollback_limit,
            cursor: Pos::default(),
            default: Cell::default(),
            alt_screen: None,
            revision: 0,
            content_seq: 0,
            screen_epoch: 0,
            size_generation: 0,
            row_content_seq: vec![0; rows as usize].into(),
            scrollback_evicted: 0,
            lines_scrolled: 0,
            alt_scroll_floor: 0,
            budget_trimmed_rows: 0,
            budget_trimmed_cleared: 0,
            row_storage_allocs: 0,
            rows_since_budget_check: 0,
            // A freshly created grid is fully dirty: the renderer has
            // never seen it. Once it does its first walk and calls
            // clear_dirty(), the flags drop to all-false.
            dirty_rows: vec![true; rows as usize],
            dirty_generation: 0,
            dirty_stamps: vec![0; rows as usize],
            prompts: VecDeque::new(),
            autowrap: true,
            pending_wrap: false,
        }
    }

    /// Enable or disable automatic wrapping at the right grid edge.
    pub fn set_autowrap(&mut self, enabled: bool) {
        if self.autowrap == enabled {
            // When: `self.autowrap == enabled`, preserve pending-wrap state and revision.
            return;
        }
        self.autowrap = enabled;
        self.pending_wrap = false;
        self.bump();
    }

    /// Return whether automatic wrapping at the right grid edge is enabled.
    pub fn autowrap(&self) -> bool {
        self.autowrap
    }

    /// Return whether the next printable character must first wrap to a new row.
    pub fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }

    fn clear_pending_wrap(&mut self) {
        self.pending_wrap = false;
    }

    /// Borrow a visible row.
    #[inline]
    pub fn row(&self, row: u16) -> &Row {
        &self.visible[row as usize]
    }

    /// Mutably borrow a visible row.
    ///
    /// Returning `&mut Row` permits arbitrary cell changes, so conservatively
    /// record a content change and revoke both wrap boundaries before handing the borrow out.
    #[inline]
    pub fn row_mut(&mut self, row: u16) -> &mut Row {
        self.mark_row(row);
        self.content_changed_row(row);
        self.revoke_successor_wrap(row);
        self.visible[row as usize].set_soft_wrapped_from_previous(false);
        self.bump();
        &mut self.visible[row as usize]
    }

    /// Iterate visible rows.
    pub fn rows_iter(&self) -> impl Iterator<Item = &Row> {
        self.visible.iter()
    }

    /// Borrow a scrollback row by index (0 = oldest). Returns `None` if out
    /// of range.
    #[inline]
    pub fn scrollback_row(&self, index: usize) -> Option<&Row> {
        self.scrollback.get(index)
    }

    /// Iterate scrollback rows from oldest to newest.
    pub fn scrollback_iter(&self) -> impl Iterator<Item = &Row> {
        self.scrollback.iter()
    }

    /// Rows dropped from scrollback over this grid's lifetime.
    ///
    /// Monotone. Evicting a row destroys the only references its cells held,
    /// so a change in this value is the signal that per-cell garbage may now
    /// exist — which is what lets a reclaimer distinguish "I already scanned
    /// and found nothing" from "the grid has moved on since I looked".
    pub fn scrollback_evicted(&self) -> u64 {
        self.scrollback_evicted
    }

    /// Rows scrolled off the top of the full screen over this grid's lifetime.
    ///
    /// It counts every full-screen scroll whether the row entered scrollback or was dropped, so an
    /// inline image anchored at a viewport row moves up by the difference between this value and
    /// the one at its capture. A scroll of a proper sub-region (DECSTBM margins) does not count.
    /// Each screen keeps its own count: an alternate screen counts from at least
    /// `ALT_SCREEN_SCROLL_GAP` past the primary's and every earlier alternate screen's, and leaving
    /// it restores the primary's count. An image anchored on one screen is therefore far above the
    /// other screen's view, or, with a mark past the count, is not drawn there at all.
    pub fn lines_scrolled(&self) -> u64 {
        self.lines_scrolled
    }

    /// How far an alternate screen's scroll count starts past every count used before it, so an
    /// image anchored on one screen is never drawn on the other.
    const ALT_SCREEN_SCROLL_GAP: u64 = 1 << 40;

    /// Scroll the full screen up by `row_count` rows, as that many line feeds at the bottom would.
    /// Rows past what the screen and history can hold are counted without being stored, since
    /// storing them would only evict them again; every row that stays in history is stored, so
    /// [`Grid::lines_scrolled`] keeps matching it.
    pub fn scroll_up_counting(&mut self, row_count: u64, fill: Cell) {
        let held = u64::from(self.rows).saturating_add(self.scrollback_limit as u64);
        let mut stored = row_count.min(held);
        while stored > 0 {
            let step = u16::try_from(stored.min(u64::from(self.rows.max(1)))).unwrap_or(1);
            self.scroll_up_with(step, fill.clone());
            stored -= u64::from(step);
        }
        self.lines_scrolled = self.lines_scrolled.saturating_add(row_count - row_count.min(held));
    }

    /// Scrollback rows dropped because the grid's retained bytes passed its byte budget, over this
    /// grid's lifetime. Monotone. At-capacity scrolling, a clear, a resize and a lowered limit do
    /// not count, so a change means history was lost to memory, not to the row limit.
    pub fn budget_trimmed_rows(&self) -> u64 {
        self.budget_trimmed_rows
    }

    /// [`Grid::budget_trimmed_rows`] as it was when scrollback was last cleared (`CSI 3 J`): trims
    /// up to that count left no history to miss.
    pub fn budget_trimmed_at_clear(&self) -> u64 {
        self.budget_trimmed_cleared
    }

    /// Insert every hyperlink id any cell this grid owns still references.
    ///
    /// Cells carry a plain `Copy` [`HyperlinkId`] with no refcount and no
    /// back-pointer to the registry, so the only way to know whether an
    /// interned link is still reachable is to look at every cell. Scrolling a
    /// row out of scrollback drops its ids with no notification, which is why
    /// the registry cannot tell on its own what has become garbage.
    ///
    /// Reaches the saved primary screen as well as the live one: while the alt
    /// screen is active, `alt_screen` holds the *primary* the user will return
    /// to. Collecting only the visible grid and its scrollback would omit
    /// every link on the screen behind the alt buffer, and a sweep using that
    /// set would break links that reappear the moment a full-screen program
    /// exits.
    ///
    /// This is the caller's to schedule, not a per-frame cost: it is
    /// `O(visible + scrollback)` and runs under the pane lock. It does not
    /// force compressed scrollback rows back to their flat form.
    pub fn collect_live_hyperlinks(&self, live: &mut HashSet<HyperlinkId>) {
        let mut collect = |row: &Row| {
            for cell in row.iter() {
                if let Some(hid) = cell.hyperlink() {
                    live.insert(hid);
                }
            }
        };
        self.visible.iter().for_each(&mut collect);
        self.scrollback.iter().for_each(&mut collect);
        if let Some(primary) = self.alt_screen.as_deref() {
            primary.collect_live_hyperlinks(live);
        }
    }

    /// Borrow the row at scrollback-absolute index `abs`. Returns `None`
    /// if `abs` lies past the bottom of the visible region. Rows inside
    /// scrollback come from the saved backing store; rows ≥ `scrollback_len`
    /// come from the live visible buffer.
    ///
    /// Used by the renderer when the viewport is scrolled away from the
    /// live bottom (e.g. after `ScrollToPrevPrompt`) so the displayed
    /// rows come from history rather than the live shell output.
    #[inline]
    pub fn row_at_abs(&self, abs: u64) -> Option<&Row> {
        let scrollback_rows = self.scrollback.len() as u64;
        if abs < scrollback_rows {
            self.scrollback.get(abs as usize)
        } else {
            // When: `abs >= scrollback_rows`, resolve the absolute row against the visible buffer.
            let visible_row = (abs - scrollback_rows) as usize;
            self.visible.get(visible_row)
        }
    }

    /// Absolute row of the cursor (= `scrollback_len() + cursor.row`). Used
    /// by OSC 133 marker recording so prompt regions survive scrolling.
    #[inline]
    pub fn cursor_absolute_row(&self) -> u64 {
        self.scrollback.len() as u64 + self.cursor.row as u64
    }

    /// Record an OSC 133 `A` (prompt-start) marker at the cursor row.
    /// Coalesces consecutive markers on the same row so a shell that emits
    /// the marker more than once per prompt doesn't bloat the buffer.
    pub fn record_prompt_start(&mut self) {
        let row = self.cursor_absolute_row();
        if matches!(self.prompts.back(), Some(prompt) if prompt.start_row == row && prompt.end_row.is_none())
        {
            // When: `matches!` finds an open prompt at `row`, coalesce the marker.
            return;
        }
        if self.prompts.len() >= PROMPT_REGION_LIMIT {
            self.prompts.pop_front();
        }
        self.prompts.push_back(PromptRegion { start_row: row, end_row: None, exit_code: None });
    }

    /// Record an OSC 133 `D` (command-end) marker. Updates the most recent
    /// prompt region in place; a stray `D` without a prior `A` is ignored.
    pub fn record_prompt_end(&mut self, exit_code: Option<i32>) {
        let row = self.cursor_absolute_row();
        if let Some(last) = self.prompts.back_mut() {
            last.end_row = Some(row);
            last.exit_code = exit_code;
        }
    }

    /// All recorded prompt regions in chronological order.
    pub fn prompts(&self) -> impl Iterator<Item = &PromptRegion> {
        self.prompts.iter()
    }

    /// Number of recorded prompt regions.
    pub fn prompts_len(&self) -> usize {
        self.prompts.len()
    }

    /// Visible-region row of a prompt region, if it currently lies inside
    /// the visible window. Used by the renderer to draw the gutter caret.
    pub fn prompt_visible_row(&self, prompt: &PromptRegion) -> Option<u16> {
        let scrollback = self.scrollback.len() as u64;
        let rel = prompt.start_row.checked_sub(scrollback)?;
        if rel < self.rows as u64 {
            Some(rel as u16)
        } else {
            // When: `rel >= self.rows`, the prompt has no visible row.
            None
        }
    }

    /// Find the prompt whose absolute start row is the largest one strictly
    /// less than `from_absolute_row`. Used by the "scroll to previous
    /// prompt" action.
    pub fn prompt_before(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts.iter().rev().find(|prompt| prompt.start_row < from_absolute_row)
    }

    /// Find the prompt whose absolute start row is the smallest one strictly
    /// greater than `from_absolute_row`. Used by the "scroll to next
    /// prompt" action.
    pub fn prompt_after(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts.iter().find(|prompt| prompt.start_row > from_absolute_row)
    }
}

fn compact_scrollback_capacity(scrollback: &mut VecDeque<Line>) {
    let len = scrollback.len();
    if scrollback.capacity() > len && (len == 0 || scrollback.capacity() >= len.saturating_mul(2)) {
        scrollback.shrink_to_fit();
    }
}

fn make_row(cols: u16) -> Line {
    Line::flat_filled(cols as usize, Cell::default())
}

#[path = "grid_dirty.rs"]
mod grid_dirty;

#[path = "grid_screens.rs"]
mod grid_screens;

#[path = "grid_write.rs"]
mod grid_write;

#[path = "grid_scroll.rs"]
mod grid_scroll;

#[path = "grid_erase.rs"]
mod grid_erase;

#[path = "grid_history.rs"]
mod grid_history;

#[path = "grid_retention.rs"]
mod grid_retention;

#[cfg(test)]
#[path = "grid_test_support.rs"]
mod grid_test_support;

#[cfg(test)]
#[path = "grid_tests.rs"]
mod grid_tests;
