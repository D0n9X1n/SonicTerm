//! Which layout a numbered box's rows allow: the signed rows only a diff writes, and the
//! continuations no diff shows.

use super::{box_blank, BoxRow, BoxRows, HardwrapUri};

/// Whether a diff line with `sign`, headed at `head_row` (or above the frame) with gutter end
/// `gutter_end`, is contradicted by its own rows: an unnumbered row from the head down to past
/// `pointed_row` that writes another character where a diff line repeats its sign. A diff shows
/// no such row, so the line is markdown.
pub(super) fn diff_contradicted(
    rows: &mut BoxRows<'_, '_>,
    (head_row, gutter_end, sign): (Option<u64>, usize, Option<char>),
    pointed_row: u64,
) -> bool {
    let first = head_row.map_or(rows.frame.view_top(), |head| head + 1);
    for row_number in first..rows.frame.view_end() {
        let Some(row) = rows.get(row_number) else {
            // When: the row is not readable in this pane, the line shows nothing further.
            return false;
        };
        if row.soft_wrapped || row.gutter_end.is_some() || !row.blank_through(gutter_end) {
            // When: the row soft_wrapped, is numbered, or writes into the gutter, the line ended.
            return false;
        }
        let cell = row.cells.get(gutter_end + 1).filter(|cell| !box_blank(cell));
        let contradicts = cell.is_some_and(|cell| Some(cell.ch) != sign);
        if contradicts {
            // When: the row `contradicts` the sign a diff line repeats there, no diff fits.
            return true;
        }
        let ended = row_number > pointed_row && cell.is_none() && sign.is_some();
        if ended {
            // When: the line `ended`, a row past the pointer leaving a signed line's column blank.
            return false;
        }
    }
    false
}

/// Whether the sign `sign` right after a numbered row's gutter end `gutter_end` opens a line only a
/// diff shows. A `+` does unless the row repeats it to the end (`+++`): markdown rarely bullets
/// with `+`, while an added line indented two blanks is common. A `-` does unless the row is a
/// markdown list item (a sign then one or two blanks before text) or a rule (`---`), both common.
pub(super) fn diff_only_sign(row: &BoxRow<'_>, gutter_end: usize, sign: char) -> bool {
    let diff_sign = matches!(sign, '+' | '-');
    if !diff_sign {
        // When: the character is no `diff_sign`, the row proves nothing.
        return false;
    }
    let after = |offset: usize| row.cells.get(gutter_end + 1 + offset);
    let blanks =
        (1..=3).take_while(|offset| after(*offset).is_some_and(|cell| box_blank(cell))).count();
    let list_item = sign == '-'
        && (1..=2).contains(&blanks)
        && after(blanks + 1).is_some_and(|cell| !box_blank(cell));
    let last = row.text.map_or(gutter_end + 1, |(_, last)| last);
    let rule = (gutter_end + 1..=last)
        .all(|column| row.cells.get(column).is_some_and(|cell| cell.ch == sign));
    !list_item && !rule
}

/// Whether `decided` opens a URL a markdown `](` opens on the row where it starts.
pub(super) fn markdown_target(rows: &mut BoxRows<'_, '_>, decided: &HardwrapUri) -> bool {
    let HardwrapUri::Complete(candidate) = decided else {
        // When: the reading is not `Complete`, it opens no markdown target.
        return false;
    };
    let Some(first) = candidate.spans.first() else {
        // When: the candidate has no `first` span, it opens nothing.
        return false;
    };
    let start = usize::from(first.start_col);
    let Some(row) = rows.get(first.row).filter(|_| start >= 2) else {
        // When: the URL's row is unreadable or starts too near its edge, no `](` precedes it.
        return false;
    };
    row.cells[start - 2].ch == ']' && row.cells[start - 1].ch == '('
}

/// One decision from two readings of a box: the same URL with the same spans, or neither owning
/// the token, stands; any other pair means the box's layout decides the destination, which it
/// does not show, so nothing opens.
pub(super) fn agreed(earlier: HardwrapUri, later: HardwrapUri) -> HardwrapUri {
    match (earlier, later) {
        (HardwrapUri::Complete(first), HardwrapUri::Complete(second)) if first == second => {
            HardwrapUri::Complete(first)
        }
        (HardwrapUri::NotApplicable, HardwrapUri::NotApplicable) => HardwrapUri::NotApplicable,
        // A URL one reading keeps on one row and the other leaves to the single-row rules is a
        // one-row URL either way, so the single-row rules decide it.
        (HardwrapUri::Complete(one_row), HardwrapUri::NotApplicable)
        | (HardwrapUri::NotApplicable, HardwrapUri::Complete(one_row))
            if one_row.spans.len() == 1 =>
        {
            HardwrapUri::NotApplicable
        }
        _ => HardwrapUri::Incomplete,
    }
}
