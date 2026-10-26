//! Reusable literal and regex matchers with counted scalar-to-cell lookups.

use super::*;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

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
pub(super) struct PreparedMatcher {
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
    pub(super) fn new(query: &str, mode: SearchMode, case_sensitive: bool) -> Self {
        let kind = match mode {
            _ if query.is_empty() => MatcherKind::Empty,
            SearchMode::Substring => match LiteralNeedle::new(query, case_sensitive) {
                Some(needle) => MatcherKind::Literal(needle),
                None => MatcherKind::Empty,
            },
            SearchMode::Regex => {
                let prefix = if case_sensitive { "" } else { "(?i)" };
                match Regex::new(&format!("{prefix}{query}")) {
                    Ok(regex) => MatcherKind::Regex(regex),
                    Err(error) => MatcherKind::Invalid(error.to_string()),
                }
            }
        };
        Self { query: query.to_owned(), mode, case_sensitive, kind }
    }

    /// Whether a cached matcher was built for exactly these search settings.
    pub(super) fn is_for(&self, query: &str, mode: SearchMode, case_sensitive: bool) -> bool {
        self.mode == mode && self.case_sensitive == case_sensitive && self.query == query
    }

    /// Whether any row can match, so a scan has work to do.
    pub(super) fn can_match(&self) -> bool {
        matches!(self.kind, MatcherKind::Literal(_) | MatcherKind::Regex(_))
    }

    /// The regex compile error the UI shows, if compilation failed.
    pub(super) fn error(&self) -> Option<&str> {
        match &self.kind {
            MatcherKind::Invalid(error) => Some(error.as_str()),
            _ => None,
        }
    }

    /// Scan every retained row, scrollback first, in document order.
    pub(super) fn scan_grid(&self, grid: &Grid, work: &mut SearchWork) -> Vec<MatchRange> {
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
    pub(super) fn scan_row(
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
            MatcherKind::Regex(regex) => {
                text.fill_raw(row);
                scan_regex(regex, text, abs_row, work, out);
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
        let mut matched = 0;
        for (index, character) in chars.iter().enumerate().skip(1) {
            while matched > 0 && *character != chars[matched] {
                matched = fail[matched - 1];
            }
            if *character == chars[matched] {
                matched += 1;
            }
            fail[index] = matched;
        }
        Some(Self { chars, fail })
    }

    /// Append this needle's non-overlapping matches in `text` to `out`,
    /// resuming at the lead after each match so matches never share a cell,
    /// in time linear in the row's scalars. `work` counts every comparison and
    /// every lookup from a matched scalar to its lead cell.
    fn scan(&self, text: &RowText, abs_row: u32, work: &mut SearchWork, out: &mut Vec<MatchRange>) {
        let pattern = &self.chars;
        let mut position = 0;
        let mut matched = 0;
        while position < text.scalars.len() {
            work.comparisons = work.comparisons.wrapping_add(1);
            if text.scalars[position] == pattern[matched] {
                position += 1;
                matched += 1;
                if matched == pattern.len() {
                    let last = text.owner.at(position - 1, work);
                    let first = text.owner.at(position - matched, work);
                    out.push(text.span(abs_row, first, last));
                    // Resume at the next lead with an empty state so matches never share a cell.
                    position = text.starts.get(last + 1, work).unwrap_or(text.scalars.len());
                    matched = 0;
                }
            } else if matched > 0 {
                // When: the scalar at `position` breaks a partial match of `matched` scalars, so fall back along fail without advancing `position`.
                matched = self.fail[matched - 1];
            } else {
                // When: the scalar at `position` matches no pattern prefix and `matched` is zero, so advance past it.
                position += 1;
            }
        }
    }
}

/// Index lists that scans may read only through counted lookups.
mod counted {
    use super::SearchWork;

    /// Indices whose every read adds one to `lookup_probes`. There is no iterator, so a
    /// search through the list costs one probe per entry it visits.
    #[derive(Default)]
    pub(super) struct CountedIndex(Vec<usize>);

    impl CountedIndex {
        /// Allocated slots, for row-buffer growth accounting.
        pub(super) fn capacity(&self) -> usize {
            self.0.capacity()
        }

        /// Drop every entry and keep the allocation.
        pub(super) fn clear(&mut self) {
            self.0.clear();
        }

        /// Append one entry.
        pub(super) fn push(&mut self, value: usize) {
            self.0.push(value);
        }

        /// Grow or shrink to `len` entries, filling new ones with `value`.
        pub(super) fn resize(&mut self, len: usize, value: usize) {
            self.0.resize(len, value);
        }

        /// The entry at `index`, counting one probe; panics when `index` is out of range.
        pub(super) fn at(&self, index: usize, work: &mut SearchWork) -> usize {
            work.lookup_probes = work.lookup_probes.wrapping_add(1);
            self.0[index]
        }

        /// The entry at `index`, or `None` past the end, counting one probe.
        pub(super) fn get(&self, index: usize, work: &mut SearchWork) -> Option<usize> {
            work.lookup_probes = work.lookup_probes.wrapping_add(1);
            self.0.get(index).copied()
        }
    }
}

/// Reusable per-row buffers mapping searchable scalars and regex bytes back to
/// the lead cells that own them, so a scan allocates per row width, not per cell.
#[derive(Default)]
pub(super) struct RowText {
    /// Lead cells in column order: start column and whether it leads a wide pair.
    leads: Vec<(u16, bool)>,
    /// Literal mode: NFC-folded scalars.
    scalars: Vec<char>,
    /// Regex mode: the raw haystack.
    haystack: String,
    /// Lead index for each scalar (literal) or haystack byte (regex).
    owner: counted::CountedIndex,
    /// Literal mode: index of each lead's first scalar.
    starts: counted::CountedIndex,
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

/// Append `regex`'s matches in `text` to `out`, each widened to the lead cells its
/// bytes belong to, counting those lookups in `work`; matches that start in one
/// lead share a range.
fn scan_regex(
    regex: &Regex,
    text: &RowText,
    abs_row: u32,
    work: &mut SearchWork,
    out: &mut Vec<MatchRange>,
) {
    for regex_match in regex.find_iter(&text.haystack) {
        if regex_match.start() == regex_match.end() {
            // When: regex_match spans zero bytes, so there is nothing to highlight and no
            // end cell to look up at regex_match.end() - 1.
            continue;
        }
        let first = text.owner.at(regex_match.start(), work);
        let last = text.owner.at(regex_match.end() - 1, work);
        push_merged(out, text.span(abs_row, first, last));
    }
}

#[cfg(test)]
#[path = "search_matcher_tests.rs"]
mod search_matcher_tests;
