//! In-page search (Cmd+F). Pure-data engine: a [`SearchState`] holds the
//! current query plus the precomputed list of [`MatchRange`]s, and exposes
//! cursor-style `next` / `prev` navigation. The renderer reads from this to
//! draw highlight quads and a status line; the app dispatches keystrokes
//! into [`SearchState::input_char`] / [`SearchState::backspace`] while
//! search is active instead of forwarding them to the pty.
//!
//! Coordinate system: [`MatchRange::row`] is an **absolute** row index that
//! treats scrollback as rows `0..scrollback_len` and the visible viewport
//! as rows `scrollback_len..scrollback_len+rows`. When there's no
//! scrollback the absolute coordinates collapse onto the visible grid, so
//! callers that don't care about scrollback can ignore the distinction.

use regex::Regex;
use sonicterm_grid::grid::{CellFlags, Grid, Row};
use unicode_normalization::UnicodeNormalization;

use crate::text_edit::{apply_edit, normalize_cursor, TextEdit};

/// A single contiguous match on one row, in **absolute** row + visible
/// column coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchRange {
    /// Absolute row: scrollback rows are `0..scrollback_len`, visible
    /// rows are `scrollback_len..scrollback_len+rows`.
    pub row: u32,
    pub col_start: u16,
    /// Exclusive end column (one past the last char of the match).
    pub col_end: u16,
}

/// Search mode — substring (literal) or regex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SearchMode {
    #[default]
    Substring,
    Regex,
}

/// Live search state for a single tab.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    cursor: usize,
    pub matches: Vec<MatchRange>,
    /// Index into `matches` of the "current" focused match, or `None`.
    pub current: Option<usize>,
    pub case_sensitive: bool,
    pub mode: SearchMode,
    /// Number of scrollback rows the grid had when matches were computed.
    /// Used to translate absolute `MatchRange::row` back into a visible
    /// row when rendering, and to detect when a match lives in scrollback.
    pub scrollback_len: u32,
    /// Visible rows captured at refresh time.
    pub visible_rows: u16,
    /// Last grid revision matches were computed against. Lets callers
    /// (the app loop) skip recomputation when the grid hasn't changed.
    pub last_revision: u64,
    /// When [`Self::current`] points to a match in scrollback (or off
    /// screen), this records the absolute row the viewport should center
    /// on. The app/renderer reads this to drive viewport scrolling.
    /// `None` means no scroll request is pending.
    pub requested_scroll_row: Option<u32>,
    /// Last regex compile error, if any (so the UI can show it).
    pub regex_error: Option<String>,
    /// Forces the next revision check to rescan, whatever the revision says.
    ///
    /// Revisions are per-grid counters, so equality means "this grid has not
    /// changed" only while the grid stays the same one. When the search is
    /// pointed at a different grid — the surviving pane after the searched
    /// pane closed — the two counters are unrelated, and an accidental match
    /// skips the rescan and leaves the dead pane's matches on screen.
    needs_rescan: bool,
    pane_id: Option<u64>,
    screen_epoch: u64,
    scrollback_evicted: u64,
    /// Matcher for the query, mode, and case setting it was built from.
    matcher: Option<PreparedMatcher>,
    /// Grid shape and content stamp of the last scan, for incremental refresh.
    scanned: Option<ScanStamp>,
    /// Cumulative scan work, for diagnostics and deterministic tests.
    work: SearchWork,
}

/// Cumulative search work since a [`SearchState`] was created, so tests and
/// diagnostics can bound rescan cost without timing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchWork {
    /// Whole-history scans: query, mode, or case changes and identity resets.
    pub full_scans: u64,
    /// Rows scanned by full and incremental scans together.
    pub rows_scanned: u64,
    /// Literal scalar comparisons.
    pub comparisons: u64,
    /// Matcher compilations; a rescan with unchanged settings reuses the matcher.
    pub matcher_builds: u64,
    /// Row-buffer growths; buffers are reused, so this follows row width, not cell count.
    pub scratch_growths: u64,
}

/// Grid shape and content stamp a scan ran against, so a later refresh can
/// rescan only the rows whose content changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScanStamp {
    content_seq: u64,
    cols: u16,
    rows: u16,
    history: usize,
}

impl SearchState {
    /// Create an idle search state: empty query, no matches, substring mode,
    /// case-insensitive, and no pending scroll request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind revisions to a pane identity before querying or refreshing its grid.
    pub fn bind_pane(&mut self, pane_id: u64) {
        if self.pane_id != Some(pane_id) {
            // Equal revision numbers cannot preserve matches across pane identity changes.
            self.pane_id = Some(pane_id);
            self.invalidate_for_new_grid();
        }
    }

    /// Select the first match in or after this viewport, or the last preceding match, without scrolling.
    pub fn anchor_to_viewport(&mut self, view_top: u64) {
        let index = self.matches.partition_point(|m| u64::from(m.row) < view_top);
        self.current = if index < self.matches.len() {
            Some(index)
        } else {
            // When: index reaches matches.len(), only earlier results exist, so focus the last one without wrapping.
            self.matches.len().checked_sub(1)
        };
        self.requested_scroll_row = None;
    }

    /// Point this search at a different grid than the one it last scanned.
    ///
    /// Called when the pane a search was running against goes away and focus
    /// lands on a survivor. The matches, their coordinates, and the recorded
    /// revision all describe a grid that no longer exists, and the revision
    /// check cannot detect that on its own: two unrelated grids can sit at the
    /// same counter, and when they do the rescan is skipped and the dead
    /// pane's highlights are drawn over the survivor's text.
    pub fn invalidate_for_new_grid(&mut self) {
        self.needs_rescan = true;
        self.current = None;
        self.requested_scroll_row = None;
    }

    /// Index window `[start, end)` into [`Self::matches`] whose rows intersect
    /// the viewport `[view_top_abs, view_top_abs + rows)`.
    ///
    /// `matches` is built scrollback-rows-then-visible-rows, both ascending,
    /// so it is sorted ascending by `row`; this is a binary-search bound. The
    /// renderer iterates only this window instead of all matches every frame,
    /// keeping per-frame highlight cost O(visible matches) rather than
    /// O(total matches) — the latter scales with scrollback depth for a
    /// common query and stutters on deep history. Returned
    /// indices stay valid against the full `matches` slice, so the caller can
    /// still compare each against `self.current`.
    #[must_use]
    pub fn visible_match_range(&self, view_top_abs: u64, rows: u16) -> (usize, usize) {
        let top = view_top_abs;
        let bottom = view_top_abs.saturating_add(u64::from(rows));
        let start = self.matches.partition_point(|m| u64::from(m.row) < top);
        let end = self.matches.partition_point(|m| u64::from(m.row) < bottom);
        (start, end)
    }

    /// Digest of everything the search overlay draws for the viewport rows
    /// `[view_top_abs, view_top_abs + rows)`, for the renderer's frame identity.
    ///
    /// Covers the visible match slice and its offset, the focused index and
    /// range, the mode, case sensitivity, query, caret, and match count, so a
    /// toggle that moves highlights without changing the count still repaints.
    /// It is computed from the public fields on every call rather than cached,
    /// so in-place edits and replacement states are always reflected, and its
    /// cost is bounded by the visible matches, not by retained history.
    #[must_use]
    pub fn presentation_hash(&self, view_top_abs: u64, rows: u16) -> u64 {
        use std::hash::{Hash, Hasher};
        let (start, end) = self.visible_match_range(view_top_abs, rows);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut hash);
        self.cursor().hash(&mut hash);
        self.matches.len().hash(&mut hash);
        self.current.hash(&mut hash);
        self.current_match().hash(&mut hash);
        self.mode.hash(&mut hash);
        self.case_sensitive.hash(&mut hash);
        start.hash(&mut hash);
        // `get` keeps a hand-edited, unsorted match list from panicking on crossed bounds.
        self.matches.get(start..end).hash(&mut hash);
        hash.finish()
    }

    /// Caret position within [`Self::query`] as a UTF-8 byte offset, clamped
    /// into range and moved back onto a character boundary before it is used.
    #[must_use]
    pub fn cursor(&self) -> usize {
        normalize_cursor(&self.query, self.cursor)
    }

    /// Replace the whole query, park the caret at its end, and rescan `grid`.
    ///
    /// Focus resets: the rescan goes through [`Self::refresh`], so no match is
    /// current afterwards and no scroll is requested.
    pub fn set_query(&mut self, query: impl Into<String>, grid: &Grid) {
        self.query = query.into();
        self.cursor = self.query.len();
        self.refresh(grid);
    }

    /// Insert one typed character at the caret and rescan `grid`.
    ///
    /// The search field is single-line, so newline input is dropped rather
    /// than stored; the caret and query are left untouched in that case.
    pub fn input_char(&mut self, ch: char, grid: &Grid) {
        if matches!(ch, '\r' | '\n') {
            // When: ch matches a line break, which a single-line search field
            // cannot hold; drop the keystroke instead of inserting it.
            return;
        }
        let cursor = self.cursor();
        self.query.insert(cursor, ch);
        self.cursor = cursor + ch.len_utf8();
        self.refresh(grid);
    }

    /// Insert a committed string at the caret and rescan `grid`; the app feeds
    /// this from IME commit text.
    ///
    /// Controls are stripped from single-line input; text containing only
    /// controls leaves the query and caret unchanged.
    pub fn input_str(&mut self, text: &str, grid: &Grid) {
        let committed: String = text.chars().filter(|ch| !ch.is_control()).collect();
        if committed.is_empty() {
            // When: committed is empty after control filtering, preserve the query and caret.
            return;
        }
        let cursor = self.cursor();
        self.query.insert_str(cursor, &committed);
        self.cursor = cursor + committed.len();
        self.refresh(grid);
    }

    /// Insert multi-character key text at the caret as one edit and rescan
    /// `grid` once, rather than once per character.
    ///
    /// Line breaks are dropped as [`Self::input_char`] drops them; text holding
    /// only line breaks leaves the query, caret, and matches untouched.
    pub fn input_key_text(&mut self, text: &str, grid: &Grid) {
        let accepted: String = text.chars().filter(|ch| !matches!(ch, '\r' | '\n')).collect();
        if accepted.is_empty() {
            // When: accepted is empty once line breaks are dropped, so there is no edit to apply.
            return;
        }
        let cursor = self.cursor();
        self.query.insert_str(cursor, &accepted);
        self.cursor = cursor + accepted.len();
        self.refresh(grid);
    }

    /// Apply one caret movement or deletion to the query, moving the caret and
    /// rescanning `grid` only when the edit actually changed the text.
    ///
    /// Pure caret moves keep the existing matches, so navigation through the
    /// query does not disturb the highlight set.
    pub fn apply_text_edit(&mut self, edit: TextEdit, grid: &Grid) {
        let outcome = apply_edit(&mut self.query, self.cursor, edit);
        self.cursor = outcome.cursor;
        if outcome.changed {
            self.refresh(grid);
        }
    }

    /// Delete the character before the caret, rescanning `grid` only when that
    /// removed something.
    pub fn backspace(&mut self, grid: &Grid) {
        self.apply_text_edit(TextEdit::DeleteBackward, grid);
    }

    /// Toggle case sensitivity (Cmd+I) and recompute.
    pub fn toggle_case_sensitive(&mut self, grid: &Grid) {
        self.case_sensitive = !self.case_sensitive;
        self.refresh(grid);
    }

    /// Toggle between substring and regex matching (Cmd+R) and recompute.
    pub fn toggle_regex(&mut self, grid: &Grid) {
        self.mode = match self.mode {
            SearchMode::Substring => SearchMode::Regex,
            SearchMode::Regex => SearchMode::Substring,
        };
        self.refresh(grid);
    }

    /// Re-scan matches only if `grid.revision()` differs from the last
    /// scan. Preserves the user's "current" match across rescans: tries to
    /// re-find the same (row, col_start) entry; if it's gone, snaps to the
    /// nearest preceding match (or the first one when nothing precedes).
    /// Returns `true` if a rescan happened.
    ///
    /// On the same screen, pane, and settings the rescan revisits only rows
    /// whose content changed or that are new, and rebases rows past eviction;
    /// identity changes, resizes, and setting changes rescan every row.
    pub fn maybe_refresh_for_revision(&mut self, grid: &Grid) -> bool {
        let same_screen = self.screen_epoch == grid.screen_epoch();
        if !self.needs_rescan
            && same_screen
            && grid.revision() == self.last_revision
            && grid.scrollback_evicted() == self.scrollback_evicted
        {
            // When: revision, screen and eviction identity still match, cached matches remain valid without rescanning.
            return false;
        }
        let removed = grid.scrollback_evicted().saturating_sub(self.scrollback_evicted);
        let anchor =
            self.current_match().filter(|_| same_screen && !self.needs_rescan).and_then(|mut m| {
                let row = u64::from(m.row).checked_sub(removed)?;
                m.row = u32::try_from(row).ok()?;
                Some(m)
            });
        let incremental = same_screen
            && !self.needs_rescan
            && grid.scrollback_evicted() >= self.scrollback_evicted
            && self.rescan_changed_rows(grid, removed);
        if !incremental {
            // Identity, shape, or setting changes cannot reuse row matches, so rescan every row.
            self.scan_all(grid);
        }
        self.record_scan(grid);
        self.current = if self.matches.is_empty() {
            None
        } else if let Some(a) = anchor {
            // When: anchor recorded the focused match from before the rescan;
            // keep the user on that same entry where it survived.
            let key = (a.row, a.col_start);
            let index = self.matches.partition_point(|m| (m.row, m.col_start) < key);
            if self.matches.get(index).is_some_and(|m| (m.row, m.col_start) == key) {
                Some(index)
            } else {
                // When: the anchored row and col_start no longer appear in
                // matches, so fall back to the nearest preceding entry.
                Some(index.saturating_sub(1))
            }
        } else {
            // When: matches exist but anchor was empty, so nothing was focused
            // before the rescan and nothing becomes focused now.
            None
        };
        self.requested_scroll_row = None;
        true
    }

    /// Recompute every match against `grid` unconditionally and drop the focus.
    ///
    /// Records the scrollback depth, visible height, and revision the scan ran
    /// against, and clears any earlier regex error. Unlike
    /// [`Self::maybe_refresh_for_revision`] it does not preserve the focused
    /// match: `current` and the pending scroll request are both reset.
    pub fn refresh(&mut self, grid: &Grid) {
        self.scan_all(grid);
        self.record_scan(grid);
        self.current = None;
        self.requested_scroll_row = None;
    }

    /// Cumulative scan work since this state was created.
    #[must_use]
    pub fn work(&self) -> SearchWork {
        self.work
    }

    /// The matcher for the current query, mode, and case setting, reusing the
    /// cached one when none of them changed.
    fn take_matcher(&mut self) -> PreparedMatcher {
        match self.matcher.take() {
            Some(matcher) if matcher.is_for(&self.query, self.mode, self.case_sensitive) => matcher,
            _ => {
                self.work.matcher_builds = self.work.matcher_builds.wrapping_add(1);
                PreparedMatcher::new(&self.query, self.mode, self.case_sensitive)
            }
        }
    }

    /// Rescan every retained row, recording any regex compile error.
    fn scan_all(&mut self, grid: &Grid) {
        let matcher = self.take_matcher();
        self.regex_error = matcher.error().map(str::to_owned);
        self.matches = matcher.scan_grid(grid, &mut self.work);
        self.matcher = Some(matcher);
    }

    /// Record the grid identity and shape the current matches describe.
    fn record_scan(&mut self, grid: &Grid) {
        self.needs_rescan = false;
        self.screen_epoch = grid.screen_epoch();
        self.scrollback_evicted = grid.scrollback_evicted();
        self.scrollback_len = grid.scrollback_len() as u32;
        self.visible_rows = grid.rows;
        self.last_revision = grid.revision();
        self.scanned = Some(ScanStamp {
            content_seq: grid.content_seq(),
            cols: grid.cols,
            rows: grid.rows,
            history: grid.scrollback_len(),
        });
    }

    /// Update matches by rescanning only rows whose content changed since the
    /// last scan, plus new rows, after dropping and rebasing evicted history.
    ///
    /// Returns `false`, leaving matches untouched, when the last scan cannot be
    /// reused: none recorded, other settings, a resized grid, history that
    /// shrank without an eviction record, or evictions on an alternate screen,
    /// which trim the saved primary's history and move no displayed row. Row
    /// stamps follow content into history, so an unchanged row keeps its
    /// matches at its rebased row.
    fn rescan_changed_rows(&mut self, grid: &Grid, removed: u64) -> bool {
        let Some(stamp) = self.scanned else {
            // When: scanned holds no stamp yet, so there are no row matches to reuse.
            return false;
        };
        let history = grid.scrollback_len();
        let total = history + usize::from(grid.rows);
        let old_total = stamp.history + usize::from(stamp.rows);
        let kept = old_total.saturating_sub(usize::try_from(removed).unwrap_or(usize::MAX));
        // Alternate-screen evictions trim the saved primary's history; no displayed row moved.
        let rebasable = removed == 0 || !grid.is_alt();
        let shape_matches =
            rebasable && stamp.cols == grid.cols && stamp.rows == grid.rows && kept <= total;
        let (query, mode, case_sensitive) = (&self.query, self.mode, self.case_sensitive);
        let Some(matcher) = self.matcher.take_if(|matcher| {
            shape_matches && matcher.can_match() && matcher.is_for(query, mode, case_sensitive)
        }) else {
            // When: take_if finds no reusable matcher or shape_matches is false, so the caller rescans every row.
            return false;
        };
        // A reusable matcher compiled cleanly, so no regex error stands.
        self.regex_error = None;
        let shift = u32::try_from(removed).unwrap_or(u32::MAX);
        self.matches.retain_mut(|m| match m.row.checked_sub(shift) {
            Some(row) => {
                m.row = row;
                true
            }
            None => false,
        });
        let seq = stamp.content_seq;
        let mut changed: Vec<usize> = grid
            .scrollback_rows_changed_since(seq)
            .filter_map(|row| usize::try_from(row).ok())
            .chain(grid.visible_rows_changed_since(seq).map(|row| history + row))
            .filter(|&row| row < kept)
            .chain(kept..total)
            .collect();
        changed.sort_unstable();
        changed.dedup();
        let Some(&first) = changed.first() else {
            // When: changed lists no row, so the rebased matches already describe the grid.
            self.matcher = Some(matcher);
            return true;
        };
        let split = self.matches.partition_point(|m| (m.row as usize) < first);
        let mut old = self.matches.split_off(split).into_iter().peekable();
        let mut text = RowText::default();
        for row in changed {
            while let Some(m) = old.next_if(|m| (m.row as usize) < row) {
                self.matches.push(m);
            }
            // Matches recorded for a changed row are stale; its rescan replaces them.
            while old.next_if(|m| m.row as usize == row).is_some() {}
            if let Some(line) = grid.row_at_abs(row as u64) {
                matcher.scan_row(line, row as u32, &mut text, &mut self.work, &mut self.matches);
            }
        }
        self.matches.extend(old);
        self.matcher = Some(matcher);
        true
    }

    /// Focus the next match, wrapping past the last one back to the first, and
    /// request a scroll to the row it lands on.
    ///
    /// Starts at the first match when nothing is focused yet.
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            // When: matches holds nothing to step onto, so clear the focus and
            // withdraw any pending scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
        self.update_scroll_request();
    }

    /// Focus the previous match, wrapping past the first one back to the last,
    /// and request a scroll to the row it lands on.
    ///
    /// Starts at the last match when nothing is focused yet.
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            // When: matches offers no earlier entry to step back onto; drop the
            // focus and the pending scroll request together.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
        self.update_scroll_request();
    }

    /// Focus the match closest to cell (`row`, `col`) and request a scroll to
    /// it.
    ///
    /// Row distance dominates; column distance breaks ties only among matches
    /// already on `row`, measured to the nearest column inside the match.
    pub fn select_nearest(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches has no entry to compare against the given cell, so
            // leave the focus and scroll request cleared.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = self
            .matches
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| {
                let row_dist = m.row.abs_diff(row);
                let col_dist = if row_dist == 0 {
                    nearest_col_in_match(m, col).abs_diff(col)
                } else {
                    // When: row_dist is nonzero, so the row gap alone ranks this
                    // match and the column distance is left at 0 unmeasured.
                    0
                };
                (row_dist, col_dist)
            })
            .map(|(i, _)| i);
        self.update_scroll_request();
    }

    /// Focus the first match that starts strictly after cell (`row`, `col`),
    /// wrapping to the first match when none does, and request a scroll to it.
    pub fn next_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches contains no entry after the given cell or anywhere
            // else, so clear the focus and the scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current =
            self.matches.iter().position(|m| (m.row, m.col_start) > (row, col)).or(Some(0));
        self.update_scroll_request();
    }

    /// Focus the last match that starts strictly before cell (`row`, `col`),
    /// wrapping to the final match when none does, and request a scroll to it.
    pub fn prev_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches contains no entry before the given cell, and none
            // to wrap onto either; clear the focus and scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = self
            .matches
            .iter()
            .rposition(|m| (m.row, m.col_start) < (row, col))
            .or_else(|| self.matches.len().checked_sub(1));
        self.update_scroll_request();
    }

    /// The currently focused match, or `None` when nothing is focused or the
    /// stored index no longer addresses an entry in `matches`.
    pub fn current_match(&self) -> Option<MatchRange> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// "N of M" indicator label. `0 of 0` when there are no matches.
    pub fn count_label(&self) -> String {
        let total = self.matches.len();
        let cur = self.current.map(|i| i + 1).unwrap_or(0);
        format!("{cur} of {total}")
    }

    /// True if the given match lives in scrollback (above the viewport).
    pub fn is_in_scrollback(&self, m: &MatchRange) -> bool {
        m.row < self.scrollback_len
    }

    /// Translate an absolute match row into a visible-row index, or `None`
    /// when the match is in scrollback (off the viewport).
    pub fn match_visible_row(&self, m: &MatchRange) -> Option<u16> {
        let visible_start = self.scrollback_len;
        if m.row < visible_start {
            // When: m sits above visible_start in scrollback history, so it has
            // no on-screen row to report.
            return None;
        }
        let r = m.row - visible_start;
        if r < self.visible_rows as u32 {
            Some(r as u16)
        } else {
            // When: r lands past visible_rows, below the viewport captured at
            // the last refresh, so there is no visible index for it.
            None
        }
    }

    fn update_scroll_request(&mut self) {
        self.requested_scroll_row = self.current_match().map(|m| m.row);
    }
}

fn nearest_col_in_match(m: &MatchRange, col: u16) -> u16 {
    col.clamp(m.col_start, m.col_end.saturating_sub(1))
}

/// Search both scrollback and visible rows of `grid` for literal `query`.
/// Returns matches with absolute row coordinates (see module docs).
///
/// Each lead cell contributes its character followed by its zero-width
/// extras. The query and each cell's cluster are compared in NFC form, then
/// lowercased unless `case_sensitive`, so canonically equivalent text matches.
pub fn find_in_grid(grid: &Grid, query: &str, case_sensitive: bool) -> Vec<MatchRange> {
    let matcher = PreparedMatcher::new(query, SearchMode::Substring, case_sensitive);
    matcher.scan_grid(grid, &mut SearchWork::default())
}

/// Regex variant. Returns `Err(msg)` with the compile error if `pattern`
/// isn't a valid regex (the caller stores this and shows it in the UI).
///
/// The haystack is each lead cell's character followed by its zero-width
/// extras, unnormalized; a match inside a cell's cluster highlights the whole
/// cell, and matches that start in the same cell share one range.
pub fn find_regex_in_grid(
    grid: &Grid,
    pattern: &str,
    case_sensitive: bool,
) -> Result<Vec<MatchRange>, String> {
    let matcher = PreparedMatcher::new(pattern, SearchMode::Regex, case_sensitive);
    if let Some(error) = matcher.error() {
        // When: matcher.error() reports a compile failure, return it for the UI instead of matches.
        return Err(error.to_owned());
    }
    Ok(matcher.scan_grid(grid, &mut SearchWork::default()))
}

/// Append `scalars`, lowercased unless `case_sensitive`, so the needle and
/// every cell cluster fold the same way after NFC.
fn push_folded(scalars: impl Iterator<Item = char>, case_sensitive: bool, out: &mut Vec<char>) {
    if case_sensitive {
        out.extend(scalars);
    } else {
        // When: case_sensitive is off, so fold each NFC scalar to lowercase to
        // meet clusters folded the same way.
        out.extend(scalars.flat_map(char::to_lowercase));
    }
}

/// Append `range`, folding it into the previous range when both start in the
/// same lead cell: cell coordinates cannot tell two matches in one cluster apart.
fn push_merged(out: &mut Vec<MatchRange>, range: MatchRange) {
    match out.last_mut() {
        Some(last) if last.row == range.row && last.col_start == range.col_start => {
            last.col_end = last.col_end.max(range.col_end);
        }
        _ => out.push(range),
    }
}

/// A matcher compiled once per query, mode, and case setting and reused by
/// every rescan until one of them changes.
#[derive(Debug, Clone)]
struct PreparedMatcher {
    query: String,
    mode: SearchMode,
    case_sensitive: bool,
    kind: MatcherKind,
}

#[derive(Debug, Clone)]
enum MatcherKind {
    /// The query is empty or folds to nothing, so nothing can match.
    Empty,
    /// A literal needle in NFC form with its failure table.
    Literal(LiteralNeedle),
    /// A compiled regex, case-folded through a `(?i)` prefix when insensitive.
    Regex(Regex),
    /// A regex that failed to compile, with the message the UI shows.
    Invalid(String),
}

impl PreparedMatcher {
    fn new(query: &str, mode: SearchMode, case_sensitive: bool) -> Self {
        let kind = match mode {
            _ if query.is_empty() => MatcherKind::Empty,
            SearchMode::Substring => match LiteralNeedle::new(query, case_sensitive) {
                Some(needle) => MatcherKind::Literal(needle),
                None => MatcherKind::Empty,
            },
            SearchMode::Regex => {
                let prefix = if case_sensitive { "" } else { "(?i)" };
                match Regex::new(&format!("{prefix}{query}")) {
                    Ok(re) => MatcherKind::Regex(re),
                    Err(error) => MatcherKind::Invalid(error.to_string()),
                }
            }
        };
        Self { query: query.to_owned(), mode, case_sensitive, kind }
    }

    /// Whether a cached matcher was built for exactly these search settings.
    fn is_for(&self, query: &str, mode: SearchMode, case_sensitive: bool) -> bool {
        self.mode == mode && self.case_sensitive == case_sensitive && self.query == query
    }

    /// Whether any row can match, so a scan has work to do.
    fn can_match(&self) -> bool {
        matches!(self.kind, MatcherKind::Literal(_) | MatcherKind::Regex(_))
    }

    /// The regex compile error the UI shows, if compilation failed.
    fn error(&self) -> Option<&str> {
        match &self.kind {
            MatcherKind::Invalid(error) => Some(error.as_str()),
            _ => None,
        }
    }

    /// Scan every retained row, scrollback first, in document order.
    fn scan_grid(&self, grid: &Grid, work: &mut SearchWork) -> Vec<MatchRange> {
        let mut out = Vec::new();
        if !self.can_match() {
            // When: can_match is false for an empty query or invalid regex, so no row is visited.
            return out;
        }
        work.full_scans = work.full_scans.wrapping_add(1);
        let mut text = RowText::default();
        for (abs, row) in grid.scrollback_iter().chain(grid.rows_iter()).enumerate() {
            self.scan_row(row, abs as u32, &mut text, work, &mut out);
        }
        out
    }

    /// Append one row's matches, reusing `text`'s buffers across rows.
    fn scan_row(
        &self,
        row: &Row,
        abs_row: u32,
        text: &mut RowText,
        work: &mut SearchWork,
        out: &mut Vec<MatchRange>,
    ) {
        let capacity = text.capacity();
        match &self.kind {
            MatcherKind::Literal(needle) => {
                text.fill_literal(row, self.case_sensitive);
                needle.scan(text, abs_row, work, out);
            }
            MatcherKind::Regex(re) => {
                text.fill_raw(row);
                scan_regex(re, text, abs_row, out);
            }
            MatcherKind::Empty | MatcherKind::Invalid(_) => {
                // When: kind is Empty or Invalid, nothing can match; scan callers check can_match first.
                return;
            }
        }
        work.rows_scanned = work.rows_scanned.wrapping_add(1);
        if text.capacity() > capacity {
            work.scratch_growths = work.scratch_growths.wrapping_add(1);
        }
    }
}

/// A literal query in NFC form, folded like the haystack, with its KMP
/// failure table so each row scan is linear in the row's scalars.
#[derive(Debug, Clone)]
struct LiteralNeedle {
    chars: Vec<char>,
    fail: Vec<usize>,
}

impl LiteralNeedle {
    fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        let mut chars = Vec::new();
        push_folded(query.nfc(), case_sensitive, &mut chars);
        if chars.is_empty() {
            // When: chars folded to nothing, so there is no needle to match.
            return None;
        }
        let mut fail = vec![0; chars.len()];
        let mut k = 0;
        for (i, ch) in chars.iter().enumerate().skip(1) {
            while k > 0 && *ch != chars[k] {
                k = fail[k - 1];
            }
            if *ch == chars[k] {
                k += 1;
            }
            fail[i] = k;
        }
        Some(Self { chars, fail })
    }

    /// Append this needle's non-overlapping matches in `text` to `out`,
    /// resuming at the lead after each match so matches never share a cell,
    /// in time linear in the row's scalars.
    fn scan(&self, text: &RowText, abs_row: u32, work: &mut SearchWork, out: &mut Vec<MatchRange>) {
        let pattern = &self.chars;
        let mut i = 0;
        let mut k = 0;
        while i < text.scalars.len() {
            work.comparisons = work.comparisons.wrapping_add(1);
            if text.scalars[i] == pattern[k] {
                i += 1;
                k += 1;
                if k == pattern.len() {
                    let last = text.owner[i - 1];
                    out.push(text.span(abs_row, text.owner[i - k], last));
                    // Resume at the next lead with an empty state so matches never share a cell.
                    i = text.starts.get(last + 1).copied().unwrap_or(text.scalars.len());
                    k = 0;
                }
            } else if k > 0 {
                // When: the scalar at i breaks a partial match of k scalars, so fall back along fail without advancing i.
                k = self.fail[k - 1];
            } else {
                // When: the scalar at i matches no pattern prefix and k is zero, so advance i past it.
                i += 1;
            }
        }
    }
}

/// Reusable per-row buffers mapping searchable scalars and regex bytes back to
/// the lead cells that own them, so a scan allocates per row width, not per cell.
#[derive(Default)]
struct RowText {
    /// Lead cells in column order: start column and whether it leads a wide pair.
    leads: Vec<(u16, bool)>,
    /// Literal mode: NFC-folded scalars.
    scalars: Vec<char>,
    /// Regex mode: the raw haystack.
    haystack: String,
    /// Lead index for each scalar (literal) or haystack byte (regex).
    owner: Vec<usize>,
    /// Literal mode: index of each lead's first scalar.
    starts: Vec<usize>,
}

impl RowText {
    fn capacity(&self) -> usize {
        self.leads.capacity()
            + self.scalars.capacity()
            + self.haystack.capacity()
            + self.owner.capacity()
            + self.starts.capacity()
    }

    fn clear(&mut self) {
        self.leads.clear();
        self.scalars.clear();
        self.haystack.clear();
        self.owner.clear();
        self.starts.clear();
    }

    /// Fill with each lead cell's character and zero-width extras in NFC,
    /// lowercased unless `case_sensitive`; wide continuation cells carry no text.
    fn fill_literal(&mut self, row: &Row, case_sensitive: bool) {
        self.clear();
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: cell is the trailing half of a wide glyph, whose text belongs to its lead.
                continue;
            }
            let lead = self.leads.len();
            self.leads.push((col as u16, cell.flags.contains(CellFlags::WIDE)));
            self.starts.push(self.scalars.len());
            match cell.extras() {
                // A lone ASCII scalar is already in NFC.
                None if cell.ch.is_ascii() => {
                    push_folded(std::iter::once(cell.ch), case_sensitive, &mut self.scalars);
                }
                extras => {
                    let cluster =
                        std::iter::once(cell.ch).chain(extras.unwrap_or_default().chars());
                    push_folded(cluster.nfc(), case_sensitive, &mut self.scalars);
                }
            }
            self.owner.resize(self.scalars.len(), lead);
        }
    }

    /// Fill with each lead cell's raw character and zero-width extras for the
    /// regex engine; nothing is normalized and continuation cells carry no text.
    fn fill_raw(&mut self, row: &Row) {
        self.clear();
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: cell is the trailing half of a wide glyph, whose text belongs to its lead.
                continue;
            }
            let lead = self.leads.len();
            self.leads.push((col as u16, cell.flags.contains(CellFlags::WIDE)));
            self.haystack.push(cell.ch);
            self.haystack.push_str(cell.extras().unwrap_or_default());
            self.owner.resize(self.haystack.len(), lead);
        }
    }

    /// Columns covered by leads `first..=last` on `abs_row`, including the
    /// continuation column when `last` leads a wide pair.
    fn span(&self, abs_row: u32, first: usize, last: usize) -> MatchRange {
        let (col_start, _) = self.leads[first];
        let (last_col, last_wide) = self.leads[last];
        MatchRange { row: abs_row, col_start, col_end: last_col + 1 + u16::from(last_wide) }
    }
}

/// Append `re`'s matches in `text` to `out`, each widened to the lead cells its
/// bytes belong to; matches that start in one lead share a range.
fn scan_regex(re: &Regex, text: &RowText, abs_row: u32, out: &mut Vec<MatchRange>) {
    for m in re.find_iter(&text.haystack) {
        if m.start() == m.end() {
            // When: m spans zero bytes, so there is nothing to highlight and no
            // end cell to look up at m.end() - 1.
            continue;
        }
        push_merged(out, text.span(abs_row, text.owner[m.start()], text.owner[m.end() - 1]));
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
