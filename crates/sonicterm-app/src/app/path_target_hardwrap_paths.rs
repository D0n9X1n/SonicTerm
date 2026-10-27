//! Bounded hard-wrap path candidates and ordered fragment mapping.

use super::*;

impl HardwrapRowToken {
    /// Whether the token fills its row to the pane's right edge, within the measured margin.
    pub(super) fn reaches_edge(self, right: usize) -> bool {
        !self.trailing_text
            && right.saturating_sub(self.token_end) <= usize::from(HARDWRAP_EDGE_MARGIN)
    }
}

/// Whether `cell` is empty: a space or a never-written cell, but not a wide character's
/// continuation, which is part of the text before it.
pub(super) fn hardwrap_blank(cell: &Cell) -> bool {
    !cell.flags.contains(CellFlags::WIDE_CONT) && matches!(cell.ch, ' ' | '\0')
}

/// The column just after one bullet marker starting at `column`, or `column` when none does.
/// Wide-cell continuations inside a marker are skipped, so a two-cell `⏺` still matches.
pub(super) fn after_bullet(cells: &[&Cell], column: usize, right: usize) -> usize {
    let after_marker = |marker: &str| {
        let mut at = column;
        for expected in marker.chars() {
            if at >= right || cells.get(at).is_none_or(|cell| cell.ch != expected) {
                // When: the cell at `at` is not `expected`, this marker does not start here.
                return None;
            }
            at += 1;
            while at < right
                && cells.get(at).is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT))
            {
                at += 1;
            }
        }
        Some(at)
    };
    if let Some(after) = HARDWRAP_BULLETS.iter().find_map(|marker| after_marker(marker)) {
        // When: a listed bullet starts the content, the token begins directly after it.
        return after;
    }
    let digits = (column..right)
        .take_while(|at| cells.get(*at).is_some_and(|cell| cell.ch.is_ascii_digit()))
        .count();
    let numbered = (1..=3).contains(&digits)
        && cells.get(column + digits).is_some_and(|cell| cell.ch == '.')
        && cells.get(column + digits + 1).is_some_and(|cell| cell.ch == ' ');
    if !numbered {
        // When: `numbered` is false, no `1. ` marker starts the content, so the token begins at `column`.
        return column;
    }
    column + digits + 2
}

/// The token of one row inside the pane `[left, right)`: its first text, where the token starts
/// (after one bullet when `bullet` allows it), where it ends, and whether more text follows it.
/// `None` when the row's pane segment holds no text.
pub(super) fn hardwrap_row_token(
    cells: &[&Cell],
    left: usize,
    right: usize,
    bullet: bool,
) -> Option<HardwrapRowToken> {
    let first_text = (left..right)
        .find(|column| cells.get(*column).is_some_and(|cell| !hardwrap_blank(cell)))?;
    let token_start = if bullet {
        after_bullet(cells, first_text, right)
    } else {
        // When: `bullet` is false, a continuation row's token starts at its first text.
        first_text
    };
    let token_end = (token_start..right)
        .find(|column| cells.get(*column).is_none_or(|cell| hardwrap_body_break(cell)))
        .unwrap_or(right);
    let trailing_text = (token_end..right)
        .any(|column| cells.get(column).is_some_and(|cell| !hardwrap_blank(cell)));
    Some(HardwrapRowToken { first_text, token_start, token_end, trailing_text })
}

/// Read absolute `row_number` inside `pane`, probing the pane's bounds at column `probe`.
/// `None` when the row is not displayed, its pane at `probe` has other bounds, or it holds no text.
pub(super) fn hardwrap_pane_row<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    row_number: u64,
    pane: (u16, u16),
    probe: u16,
    bullet: bool,
) -> Option<HardwrapPaneRow> {
    if pane_columns_at(frame, row_number, probe) != pane {
        // When: this row's pane at `probe` has other bounds, it belongs to another pane layout.
        return None;
    }
    let view = rows.view(frame, row_number)?;
    let token = hardwrap_row_token(&view.cells, usize::from(pane.0), usize::from(pane.1), bullet)?;
    let text = view.cells[token.token_start..token.token_end].iter().map(|cell| cell.ch).collect();
    Some(HardwrapPaneRow { token, text, soft_wrapped: view.row.soft_wrapped_from_previous() })
}

/// Whether `text` starts with a drive prefix such as `C:\` or `C:/`.
pub(super) fn starts_with_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// Whether `text`, an opening row's token, can open a chain of `kind`. A URL needs one of the
/// `scheme://` prefixes `find_urls` accepts and a path after its authority, so a host alone never
/// opens a chain. A path needs an absolute, home or drive start; UNC and network paths
/// (`\\host`, `//host`) never open one.
pub(super) fn hardwrap_opens(kind: HardwrapKind, text: &str) -> bool {
    match kind {
        HardwrapKind::Uri => {
            // When: `kind` is `Uri`, an allow-listed `scheme://` and a path must open the token.
            let Some(scheme) = ["https://", "http://", "file://"].into_iter().find(|scheme| {
                text.get(..scheme.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
            }) else {
                // When: no allow-listed `scheme://` prefix starts `text`, it opens no URL chain.
                return false;
            };
            let rest = &text[scheme.len()..];
            // A file URI's authority may be empty, so any path slash proves its path.
            if scheme == "file://" {
                rest.contains('/')
            } else {
                // When: `scheme` is http(s), a non-empty authority must precede the path slash.
                rest.find(['/', '?', '#'])
                    .is_some_and(|index| index > 0 && rest.as_bytes()[index] == b'/')
            }
        }
        HardwrapKind::Path => {
            // An absolute, home or drive start with a separator opens a path chain.
            let bytes = text.as_bytes();
            let posix =
                bytes.first() == Some(&b'/') && bytes.get(1).is_some_and(|byte| *byte != b'/');
            posix || text.starts_with("~/") || starts_with_drive(text)
        }
    }
}

/// Whether a continuation row's token starts a new token of `kind` instead of continuing one. A
/// path may continue with `/`, since programs often break before a separator.
pub(super) fn hardwrap_starts_new(kind: HardwrapKind, text: &str) -> bool {
    match kind {
        HardwrapKind::Uri => text.contains("://"),
        HardwrapKind::Path => {
            // A scheme, home, drive or UNC start begins another target.
            text.contains("://")
                || text.starts_with("~/")
                || starts_with_drive(text)
                || text.starts_with("\\\\")
        }
    }
}

/// `Incomplete` when an owned chain covering `pointed` cannot be proven, so its prefix stays
/// inert; `NotApplicable` otherwise, so the single-row result stands.
pub(super) fn hardwrap_unproven(
    spans: &[AbsoluteCellSpan],
    owned: bool,
    pointed: AbsoluteCell,
) -> HardwrapChain {
    if !(owned && spans.iter().any(|span| span.contains(pointed))) {
        // When: `owned` is false or no span covers `pointed`, the single-row result stands.
        return HardwrapChain::NotApplicable;
    }
    HardwrapChain::Incomplete
}

/// Follow the chain of `kind` opening at `opener_row` down the screen and decide it for
/// `pointed`. The chain is owned once a continuation row starts at the token's start column; an
/// owned chain that meets prose, the viewport edge or a cap is `Incomplete`, an unowned one is
/// `NotApplicable`.
pub(super) fn bare_hardwrap_chain<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    kind: HardwrapKind,
    pane: (u16, u16),
    opener_row: u64,
    opener: &HardwrapPaneRow,
    pointed: AbsoluteCell,
) -> HardwrapChain {
    let right = usize::from(pane.1);
    let column = opener.token.token_start;
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    spans.push(AbsoluteCellSpan {
        row: opener_row,
        start_col: column as u16,
        end_col: opener.token.token_end as u16,
    });
    let mut joined = opener.text.clone();
    let mut owned = false;
    for offset in 1..MAX_WRAPPED_PATH_ROWS as u64 {
        let row_number = opener_row + offset;
        if row_number >= frame.view_end() {
            // When: `row_number` reaches `view_end`, the chain's next row is out of view and unproven.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        let Some(row) = hardwrap_pane_row(frame, rows, row_number, pane, column as u16, false)
            .filter(|row| {
                !row.soft_wrapped
                    && row.token.first_text == column
                    && !row.text.is_empty()
                    && !hardwrap_starts_new(kind, &row.text)
            })
        else {
            // When: the next row holds no continuation at the token's column, the edge end is unproven.
            return hardwrap_unproven(&spans, owned, pointed);
        };
        owned = true;
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: column as u16,
            end_col: row.token.token_end as u16,
        });
        if joined.len() + row.text.len() > MAX_LOGICAL_PATH_BYTES {
            // When: the joined text would pass the byte cap, the chain cannot be proven.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        joined.push_str(&row.text);
        if row.token.trailing_text {
            // When: text follows the fragment on its row, the row is prose, not the token's end.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        if !row.token.reaches_edge(right) {
            // When: the fragment ends before the edge, it is the chain's last row.
            if !spans.iter().any(|span| span.contains(pointed))
                || spans_join_across_pane_border(frame, &spans)
            {
                // When: no span covers `pointed` or the spans cross a pane border, the chain does not own it.
                return HardwrapChain::NotApplicable;
            }
            return HardwrapChain::Proven { spans, joined };
        }
    }
    // Every row up to the cap reached the edge, so the chain's end is out of reach.
    hardwrap_unproven(&spans, owned, pointed)
}

/// Decide a URL a program broke across rows itself, outside any wrapper. The token starts its
/// row's content (after the pane-relative indent or one bullet), every non-last row ends exactly
/// at the pane's right edge, and continuation rows start at the token's start column. Runs after
/// the bracketed rule and before the wrap-aware scanner, on both screens.
pub(super) fn bare_hardwrap_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    match bare_hardwrap_in(frame, pointed, HardwrapKind::Uri) {
        HardwrapChain::NotApplicable => HardwrapUri::NotApplicable,
        HardwrapChain::Incomplete => HardwrapUri::Incomplete,
        HardwrapChain::Proven { spans, joined } => hardwrap_uri_target(spans, &joined, pointed),
    }
}

/// The chain of `kind` under `pointed`: the rows above it that a chain could span are walked
/// once, and openers among them are tried top first, so a continuation row that resembles an
/// opener never cuts off the rows above it.
pub(super) fn bare_hardwrap_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    kind: HardwrapKind,
) -> HardwrapChain {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane token lies under it.
        return HardwrapChain::NotApplicable;
    }
    let right = usize::from(pane.1);
    let top = pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    let mut rows = HardwrapRows { views: HashMap::new() };
    // Walk up while the row above fills to the edge and this row starts at its token column.
    let mut start = pointed.row;
    while start > top {
        let Some(lower) = hardwrap_pane_row(frame, &mut rows, start, pane, pointed.col, false)
        else {
            // When: this row holds no text in the pane, no chain continues through it.
            break;
        };
        let Some(upper) = hardwrap_pane_row(frame, &mut rows, start - 1, pane, pointed.col, true)
        else {
            // When: the row above holds no text in the pane, no chain reaches down from it.
            break;
        };
        if lower.soft_wrapped
            || !upper.token.reaches_edge(right)
            || lower.token.first_text != upper.token.token_start
        {
            // When: `lower` is soft-wrapped, `upper` misses the edge, or `first_text` misaligns, stop the walk.
            break;
        }
        start -= 1;
    }
    for opener_row in start..=pointed.row {
        let Some(opener) = hardwrap_pane_row(frame, &mut rows, opener_row, pane, pointed.col, true)
            .filter(|row| {
                !row.soft_wrapped
                    && row.token.reaches_edge(right)
                    && hardwrap_opens(kind, &row.text)
            })
        else {
            // When: this row's token does not open a chain of `kind` at the edge, try the next row.
            continue;
        };
        match bare_hardwrap_chain(frame, &mut rows, kind, pane, opener_row, &opener, pointed) {
            HardwrapChain::NotApplicable => {
                // When: this opener's chain does not decide `pointed`, a lower opener may.
            }
            decided => {
                // When: `decided` is Proven or Incomplete, the topmost owning opener settles `pointed`.
                return decided;
            }
        }
    }
    HardwrapChain::NotApplicable
}

/// Decide a path a program broke across rows itself, with the URL rule's row geometry. The joined
/// text goes through the typed path scanner for `style`, so source references, suffixes, limits
/// and locality behave as for a single-row path.
pub(super) fn bare_hardwrap_path_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    style: PathStyle,
) -> HardwrapPathScan {
    let spans = match bare_hardwrap_in(frame, pointed, HardwrapKind::Path) {
        HardwrapChain::NotApplicable => {
            // When: `bare_hardwrap_in` is NotApplicable, the path rule does not own `pointed`.
            return HardwrapPathScan::NotApplicable;
        }
        HardwrapChain::Incomplete => {
            // When: `bare_hardwrap_in` is Incomplete, the owned chain stays inert.
            return HardwrapPathScan::Incomplete;
        }
        HardwrapChain::Proven { spans, .. } => spans,
    };
    let mut cells = Vec::new();
    let mut positions = Vec::new();
    let mut rows = SmallVec::<[PathRowIdentity; 2]>::new();
    for span in &spans {
        let Some(row) = frame.row(span.row) else {
            // When: a chain row is no longer displayed, the proven chain cannot be read again.
            return HardwrapPathScan::Incomplete;
        };
        rows.push(PathRowIdentity { row: span.row, fingerprint: row_fingerprint(row) });
        let row_cells = row.iter().collect::<Vec<_>>();
        for column in span.start_col..span.end_col {
            let Some(cell) = row_cells.get(usize::from(column)) else {
                // When: `row_cells` has no cell at `column`, the row changed since the chain was read.
                return HardwrapPathScan::Incomplete;
            };
            cells.push(*cell);
            positions.push(AbsoluteCell { row: span.row, col: column });
        }
    }
    let Some(pointed_index) = positions.iter().position(|position| *position == pointed) else {
        // When: `pointed` is outside the chain's cells, the chain does not own it.
        return HardwrapPathScan::NotApplicable;
    };
    let Some(mapped) = PathCellText::from_cells(&cells, &positions) else {
        // When: the joined cells do not map to text, the scanner has nothing to read.
        return HardwrapPathScan::NotApplicable;
    };
    let candidates = mapped
        .candidates(&cells, pointed_index, style, false)
        .into_iter()
        .filter(|(matched, _)| !matches!(matched.target, DetectedTarget::Uri(_)))
        .filter_map(|(matched, range)| {
            let spans = spans_for_positions(&positions[range])?;
            (spans.len() >= 2 && spans.iter().any(|span| span.contains(pointed)))
                .then_some(LogicalTargetCandidate { target: matched.target, spans, joined: true })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        // When: `candidates` is empty, no path crosses a break and the single-row result stands.
        return HardwrapPathScan::NotApplicable;
    }
    HardwrapPathScan::Joined(LogicalPathScan { candidates, rows })
}

/// Group `positions`, in reading order, into one span per row; `None` when a row's cells are not
/// contiguous.
pub(super) fn spans_for_positions(
    positions: &[AbsoluteCell],
) -> Option<SmallVec<[AbsoluteCellSpan; 2]>> {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    for position in positions {
        match spans.last_mut() {
            Some(span) if span.row == position.row && span.end_col == position.col => {
                span.end_col = position.col.checked_add(1)?;
            }
            Some(span) if span.row == position.row => {
                // When: `span` and `position` share a row without adjacency, the span map is discontinuous.
                return None;
            }
            _ => spans.push(AbsoluteCellSpan {
                row: position.row,
                start_col: position.col,
                end_col: position.col.checked_add(1)?,
            }),
        }
    }
    Some(spans)
}

/// The joined chain's candidates, then the pointed row's own fragments that cover `pointed`, so
/// every candidate contains the hovered cell; rows cover every chain row and fragment row.
pub(super) fn merge_hardwrap_fragments(
    mut joined: LogicalPathScan,
    fragments: Option<LogicalPathScan>,
    pointed: AbsoluteCell,
) -> LogicalPathScan {
    let Some(fragments) = fragments else {
        // When: `fragments` is None, the joined candidates stand alone.
        return joined;
    };
    for candidate in fragments.candidates {
        let covers = candidate.spans.iter().any(|span| span.contains(pointed));
        let known = joined.candidates.iter().any(|existing| {
            existing.target == candidate.target && existing.spans == candidate.spans
        });
        if covers && !known {
            joined.candidates.push(candidate);
        }
    }
    for row in fragments.rows {
        if !joined.rows.contains(&row) {
            joined.rows.push(row);
        }
    }
    joined.rows.sort_by_key(|row| row.row);
    joined
}

#[cfg(test)]
#[path = "path_target_hardwrap_paths_tests.rs"]
mod path_target_hardwrap_paths_tests;
