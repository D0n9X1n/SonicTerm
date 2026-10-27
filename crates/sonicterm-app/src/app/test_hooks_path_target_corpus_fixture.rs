//! Reference hard-wrap parser and deterministic equivalence corpus.

use super::*;

// The hard-wrap pre-pass as it was before each row was read once: every wrapper collected its own
// row and searched a growing suffix. It is kept only as the equivalence oracle and the cost
// baseline for the shared-view pre-pass.
pub(super) fn reference_hardwrap_uri_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
) -> HardwrapUri {
    let first =
        pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    for row_number in (first..=pointed.row).rev() {
        let Some(row) = frame.row(row_number) else {
            // When: row_at_abs has no row_number, that evicted row cannot open a visible candidate.
            continue;
        };
        // The frame's last row may wrap into a row below it that is not displayed, so on a
        // scrolled-back view that boundary is unknown and the logical scanner, which fails closed
        // there, owns it.
        let next_row_number = row_number + 1;
        let wraps_below = if frame.contains(next_row_number) {
            frame.row(next_row_number).is_some_and(|next| next.soft_wrapped_from_previous())
        } else {
            // When: the next row is outside the frame, only a scrolled-back view has one at all.
            frame.rows_below()
        };
        if row.soft_wrapped_from_previous() || wraps_below {
            // When: soft_wrapped_from_previous holds, or may hold, at either boundary, the logical scanner owns it.
            continue;
        }
        let mut result = HardwrapUri::NotApplicable;
        hardwrap_work::add(row.len());
        for (index, cell) in row.iter().enumerate() {
            let closer = match cell.ch {
                '(' => ')',
                '[' => ']',
                _ => {
                    // When: cell.ch opens no supported wrapper, this column cannot delimit a URI.
                    continue;
                }
            };
            let start = index + 1;
            if !reference_opens_http_scheme(row, start) {
                // When: opens_http_scheme finds neither https:// nor http:// at start, this wrapper opens ordinary prose.
                continue;
            }
            let found = reference_hardwrap_uri_candidate(frame, pointed, row_number, start, closer);
            // When: matches! excludes NotApplicable for found, retain this wrapper's claim on pointed.
            if !matches!(found, HardwrapUri::NotApplicable) {
                if !matches!(result, HardwrapUri::NotApplicable) {
                    // When: matches! excludes NotApplicable for result, another wrapper already owns pointed.
                    return HardwrapUri::Incomplete;
                }
                result = found;
            }
        }
        if !matches!(result, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for result, the nearest owning wrapper decides pointed.
            return result;
        }
    }
    HardwrapUri::NotApplicable
}

/// Whether `row`'s cells from column `start` spell `https://` or `http://`, read into a fixed
/// buffer so the pre-pass allocates nothing for the many wrappers that open ordinary prose.
pub(super) fn reference_opens_http_scheme(row: &Row, start: usize) -> bool {
    let mut scheme = ['\0'; 8];
    for (offset, slot) in scheme.iter_mut().enumerate() {
        let Some(cell) = row.get(start + offset) else {
            // When: the row ends before eight cells, the remaining slots stay empty.
            break;
        };
        *slot = cell.ch;
    }
    let spells =
        |prefix: &str| prefix.chars().zip(scheme).all(|(expected, found)| expected == found);
    spells("https://") || spells("http://")
}

pub(super) fn reference_hardwrap_uri_candidate(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    first_row: u64,
    first_col: usize,
    closer: char,
) -> HardwrapUri {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    let mut joined = String::new();
    let mut indent = None;
    let mut authority_end = None;
    let refusal = |spans: &[AbsoluteCellSpan]| {
        // When: a span contains pointed and spans_join_across_pane_border is false, suppress that fragment's truncated prefix.
        if spans.iter().any(|span| span.contains(pointed))
            && !spans_join_across_pane_border(frame, spans)
        {
            HardwrapUri::Incomplete
        } else {
            HardwrapUri::NotApplicable
        }
    };
    for offset in 0..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        // When: row_number reaches view_end, the chain leaves the viewport and cannot be proven complete.
        if row_number >= frame.view_end() {
            return refusal(&spans);
        }
        let Some(row) = frame.row(row_number) else {
            // When: row_at_abs has no row_number, the chain cannot be continued or proven.
            return refusal(&spans);
        };
        if row.soft_wrapped_from_previous() {
            // When: soft_wrapped_from_previous holds, the wrap-aware scanner owns this boundary.
            return refusal(&spans);
        }
        let cells = row.iter().collect::<Vec<_>>();
        hardwrap_work::add(cells.len());
        let start = if offset == 0 {
            first_col
        } else {
            // When: offset is past zero, start skips the continuation indent rather than first_col.
            cells.iter().position(|cell| cell.ch != ' ').unwrap_or(cells.len())
        };
        let close = cells
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, cell)| cell.ch == closer)
            .map(|(col, _)| col);
        if offset == 0 && close.is_some() {
            // When: close exists at offset zero, single-row detection stays authoritative.
            return HardwrapUri::NotApplicable;
        }
        let end = close.unwrap_or(usize::from(frame.grid().cols));
        let Some(body) = cells.get(start..end).filter(|body| !body.is_empty()) else {
            // When: cells hold no non-empty body between start and end, this row adds no fragment.
            return refusal(&spans);
        };
        if body.iter().any(|cell| {
            unsafe_path_cell(cell)
                || cell.hyperlink().is_some()
                || !cell.ch.is_ascii()
                || cell.ch.is_whitespace()
                || cell.ch.is_control()
                || matches!(cell.ch, '(' | ')' | '[' | ']')
        }) {
            // When: any body cell is unsafe, hyperlinked, non-ascii, blank, control, or a wrapper,
            // the row carries prose rather than a continuation of this candidate.
            return refusal(&spans);
        }
        if offset > 0 {
            // When: offset is past zero, a continuation row must open no scheme and respect the indent.
            let prefix = body.iter().take(8).map(|cell| cell.ch).collect::<String>();
            if prefix.starts_with("https://") || prefix.starts_with("http://") {
                // When: prefix opens its own scheme, that independent URI keeps its destination.
                return refusal(&spans);
            }
            if start > MAX_HARDWRAP_INDENT {
                // When: start passes MAX_HARDWRAP_INDENT, the row is too deep to be one continued line.
                return refusal(&spans);
            }
        }
        if offset > 0 && *indent.get_or_insert(start) != start {
            // When: start differs from the recorded indent, the preceding fragment does not own this row.
            return refusal(&spans);
        }
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: start as u16,
            end_col: end as u16,
        });
        if joined.len() + body.len() > MAX_LOGICAL_PATH_BYTES {
            // When: joined plus body passes MAX_LOGICAL_PATH_BYTES, stop before admitting more text.
            return refusal(&spans);
        }
        joined.extend(body.iter().map(|cell| cell.ch));
        if offset == 0 {
            // When: offset is zero, joined must prove a complete authority before the margin cut.
            let scheme_len = if joined.starts_with("https://") { 8 } else { 7 };
            authority_end = joined[scheme_len..]
                .find(['/', '?', '#'])
                .filter(|index| *index > 0 && joined.as_bytes()[scheme_len + index] == b'/')
                .map(|index| scheme_len + index);
            if authority_end.is_none() {
                // When: authority_end is absent, no path slash followed the host, so invent no suffix.
                return refusal(&spans);
            }
        }
        if joined.matches("://").count() != 1 {
            // When: joined holds other than one :// separator, two destinations were concatenated.
            return refusal(&spans);
        }
        if close.is_some() {
            // When: close exists, the wrapper terminates and joined must validate as one whole URI.
            if unsafe_path_cell(&cells[end])
                || cells[end].hyperlink().is_some()
                || cells[end + 1..].iter().take_while(|cell| !cell.ch.is_whitespace()).any(|cell| {
                    unsafe_path_cell(cell) || !matches!(cell.ch, '.' | ',' | ';' | ':' | '!' | '?')
                })
            {
                // When: cells after end continue the token, the apparent closer may be internal URI text.
                return refusal(&spans);
            }
            let matches = sonicterm_cfg::url_scan::find_urls(&joined);
            let Some(found) = matches.first().filter(|found| {
                matches.len() == 1
                    && found.start == 0
                    && found.end == joined.len()
                    && found.url == joined
                    && authority_end.is_some()
            }) else {
                // When: find_urls does not return exactly joined as one whole match, reject the prefix.
                return refusal(&spans);
            };
            if !spans.iter().any(|span| span.contains(pointed)) {
                // When: no span contains pointed, this proven chain does not own the pointer.
                return HardwrapUri::NotApplicable;
            }
            if spans_join_across_pane_border(frame, &spans) {
                // When: spans_join_across_pane_border holds, the chain runs into the next pane's text.
                return HardwrapUri::NotApplicable;
            }
            return HardwrapUri::Complete(LogicalTargetCandidate {
                target: DetectedTarget::Uri(found.url.clone()),
                spans,
                joined: false,
            });
        }
    }
    refusal(&spans)
}

/// A deterministic xorshift generator, so the equivalence corpus is the same on every run.
pub(super) struct CorpusRng(u64);

impl CorpusRng {
    pub(super) fn next_value(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(super) fn below(&mut self, bound: usize) -> usize {
        (self.next_value() % bound as u64) as usize
    }
}

/// Fragments a random prose row is built from: wrappers, schemes, blanks, wide and combining-free
/// non-ASCII text, a box-drawing border and an OSC 8 link.
pub(super) const CORPUS_FRAGMENTS: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "(https://",
    "[http://",
    "ex.co/",
    "/",
    "a",
    "bb",
    " ",
    "  ",
    ".",
    "?",
    "é",
    "\u{754c}",
    "\u{2502}",
    "\x1b]8;;https://h.test/\x1b\\ln\x1b]8;;\x1b\\",
];

/// One random prose row of at most `cols` cells' worth of fragments.
pub(super) fn corpus_prose_line(rng: &mut CorpusRng, cols: u16) -> String {
    let fragment_count = rng.below(usize::from(cols) / 2);
    (0..fragment_count).map(|_| CORPUS_FRAGMENTS[rng.below(CORPUS_FRAGMENTS.len())]).collect()
}

/// A hard-wrapped bracketed URL: a first row filled to the edge, one or two indented continuation
/// rows and a closer, sometimes with an unequal indent or one character mutated into a break.
pub(super) fn corpus_chain_lines(rng: &mut CorpusRng, cols: u16) -> Vec<String> {
    let width = usize::from(cols);
    let (opener, closer) = if rng.below(2) == 0 { ('(', ')') } else { ('[', ']') };
    let head = ["", "x ", "see "][rng.below(3)];
    let scheme = ["https://ex.co/", "http://ex.co/", "https://ex.co"][rng.below(3)];
    let mut first = format!("{head}{opener}{scheme}");
    while first.len() < width {
        first.push('a');
    }
    let mut lines = vec![first];
    let continuation_count = 1 + rng.below(2);
    let indent = rng.below(3);
    for index in 0..continuation_count {
        let row_indent = if rng.below(8) == 0 { indent + 1 } else { indent };
        let mut line = " ".repeat(row_indent);
        if index + 1 < continuation_count {
            while line.len() < width {
                line.push('b');
            }
        } else {
            let body_len = 1 + rng.below(width - row_indent - 2);
            line.push_str(&"c".repeat(body_len));
            line.push(closer);
            line.push_str(["", " done", ".", "x", ")"][rng.below(5)]);
            line.truncate(width);
        }
        lines.push(line);
    }
    if rng.below(4) == 0 {
        // Mutate one character of one row into a break or a wrapper.
        let line_index = rng.below(lines.len());
        let mut chars = lines[line_index].chars().collect::<Vec<_>>();
        let position = rng.below(chars.len());
        chars[position] = ['(', ' ', ')', 'é', '/', ']'][rng.below(6)];
        lines[line_index] = chars.into_iter().collect();
    }
    lines
}

/// Assert the shared-view pre-pass decides every cell of `grid`'s live frame as the per-wrapper
/// reference does, including cells a row holds past the grid's edge, and return how many cells the
/// reference resolved `Complete` and `Incomplete`.
pub(super) fn assert_hardwrap_cells_match_reference(grid: &Grid, label: &str) -> (usize, usize) {
    let view_top = grid.scrollback_evicted() + grid.scrollback_len() as u64;
    let (mut complete_count, mut incomplete_count) = (0usize, 0usize);
    for row in view_top..view_top + u64::from(grid.rows) {
        let row_len = FrameRows::new(grid, view_top).row(row).map_or(0, Row::len);
        // Walk past `cols` so a row longer than the grid is compared at every cell it holds.
        let col_end = row_len.max(usize::from(grid.cols)) as u16;
        for col in 0..col_end {
            let pointed = AbsoluteCell { row, col };
            let expected = reference_hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed);
            assert_eq!(
                hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed),
                expected,
                "{label}: cell {row}:{col}"
            );
            match expected {
                HardwrapUri::Complete(_) => complete_count += 1,
                HardwrapUri::Incomplete => incomplete_count += 1,
                HardwrapUri::NotApplicable => {}
            }
        }
    }
    (complete_count, incomplete_count)
}

/// A parser over a primary-screen grid `cols` wide holding `lines` as hard line breaks, so no row
/// is soft-wrapped.
pub(super) fn hardwrap_grid(cols: u16, lines: &[String]) -> sonicterm_vt::vt::Parser {
    let rows = lines.len() as u16 + 1;
    let mut parser = sonicterm_vt::vt::Parser::new(Grid::new(cols, rows));
    parser.advance(lines.join("\r\n").as_bytes());
    parser
}

/// Append `text` to visible row `row` past its current length, as a row left longer than the grid
/// by a resize that did not reflow it.
pub(super) fn extend_row_past_grid(grid: &mut Grid, row: u16, text: &str) {
    let line = grid.row_mut(row);
    let old_len = line.len();
    line.resize(old_len + text.chars().count(), Cell::default());
    for (cell, character) in line.iter_mut().skip(old_len).zip(text.chars()) {
        *cell = Cell::plain(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// Cells walked by `pre_pass` for one primary-screen row holding `pattern` repeated to `cols`
/// columns, pointed at its first cell.
pub(super) fn hardwrap_cells_walked(
    pattern: &str,
    cols: u16,
    pre_pass: fn(&FrameRows<'_>, AbsoluteCell) -> HardwrapUri,
) -> u64 {
    let row_text = pattern.chars().cycle().take(usize::from(cols)).collect::<String>();
    let mut parser = sonicterm_vt::vt::Parser::new(Grid::new(cols, 2));
    parser.advance(row_text.as_bytes());
    let grid = parser.grid();
    let pointed = AbsoluteCell { row: 0, col: 0 };
    let (_, walked) = hardwrap_work::measure(|| pre_pass(&FrameRows::new(grid, 0), pointed));
    walked
}

/// Assert that doubling the row width at most about doubles the pre-pass's work on `pattern`,
/// while the per-wrapper reference grows about fourfold, which proves the measure can tell them apart.
pub(super) fn assert_hardwrap_work_is_linear(pattern: &str) {
    let narrow = hardwrap_cells_walked(pattern, 400, hardwrap_uri_in);
    let wide = hardwrap_cells_walked(pattern, 800, hardwrap_uri_in);
    assert!(narrow > 0, "{pattern:?}: the measure counted no work");
    assert!(wide * 2 <= narrow * 5, "{pattern:?}: {narrow} cells at 400 columns, {wide} at 800");
    let reference_narrow = hardwrap_cells_walked(pattern, 400, reference_hardwrap_uri_in);
    let reference_wide = hardwrap_cells_walked(pattern, 800, reference_hardwrap_uri_in);
    assert!(
        reference_wide >= reference_narrow * 3,
        "{pattern:?}: the reference walked {reference_narrow} then {reference_wide} cells"
    );
}

#[cfg(test)]
#[path = "test_hooks_path_target_corpus_fixture_tests.rs"]
mod test_hooks_path_target_corpus_fixture_tests;
