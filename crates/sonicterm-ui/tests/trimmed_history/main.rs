//! Selection, search and URL detection read trimmed history rows exactly as they read flat ones.
//!
//! The grid stores short rows that scroll into history trimmed: their cells up to the last one
//! that differs from the fill, then the fill once. These tests build a 200-column history from
//! 1,000 known short rows and check every consumer against the text the generator wrote.

#![warn(clippy::min_ident_chars)]

use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid};
use sonicterm_ui::search::{find_in_grid, find_regex_in_grid};
use sonicterm_ui::selection::Selection;

/// Lines written; the history keeps the last 1,000 that scrolled out.
const LINES_WRITTEN: usize = 1_100;
/// History rows the default limit keeps at 200x24.
const HISTORY_ROWS: usize = 1_000;
/// Columns of the grid.
const COLS: u16 = 200;
/// Visible rows of the grid.
const ROWS: u16 = 24;

/// The text of generated line `line`, cycling words, CJK wide pairs, a URL, a path and a prompt
/// whose erased tail is coloured.
fn line_text(line: usize) -> String {
    match line % 5 {
        0 => format!("alpha beta {line} gamma"),
        1 => format!("中文{line}字"),
        2 => format!("see https://example.com/page{line} now"),
        3 => format!("edit ./src/file{line}.rs now"),
        _ => format!("prompt{line}$"),
    }
}

/// Build the grid: every line written from column 0, the prompt lines erased to the end with a
/// coloured fill, each ended by CR LF.
fn grid_with_known_history() -> Grid {
    let coloured = Cell::plain(' ', Color::Default, Color::Indexed(4), CellFlags::empty());
    let mut grid = Grid::new(COLS, ROWS);
    for line in 0..LINES_WRITTEN {
        for character in line_text(line).chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        if line % 5 == 4 {
            grid.erase_line_to_end_with(coloured.clone());
        }
        grid.carriage_return();
        grid.linefeed();
    }
    grid
}

/// The generated line held by history row `row`: lines scroll out once the cursor reaches the
/// last visible row, so history starts `LINES_WRITTEN - (ROWS - 1) - HISTORY_ROWS` lines in.
fn line_at_history_row(row: usize) -> usize {
    LINES_WRITTEN - (usize::from(ROWS) - 1) - HISTORY_ROWS + row
}

/// The text of a history row as a reader sees it: each lead cell's character, skipping the
/// continuation half of a wide pair.
fn history_row_text(grid: &Grid, row: usize) -> String {
    let Some(line) = grid.row_at_abs(row as u64) else { panic!("history row {row}") };
    line.iter()
        .filter(|cell| !cell.flags.contains(CellFlags::WIDE_CONT))
        .map(|cell| cell.ch)
        .collect()
}

/// The history is stored trimmed (at least 900 rows) and retains at most half its flat figure,
/// so every check below reads trimmed rows.
#[test]
fn the_history_is_stored_trimmed() {
    let grid = grid_with_known_history();
    assert_eq!(grid.scrollback_len(), HISTORY_ROWS);
    assert!(
        grid.scrollback_trimmed_rows() >= 900,
        "{} rows trimmed",
        grid.scrollback_trimmed_rows()
    );
    let flat_bytes = HISTORY_ROWS * usize::from(COLS) * std::mem::size_of::<Cell>();
    let history_bytes = grid.retained_amount_by_region().history.bytes;
    assert!(history_bytes * 2 <= flat_bytes, "history {history_bytes} against flat {flat_bytes}");
}

/// Copying a range of history rows, a whole history row and a word in one returns the generated
/// text; the coloured erased tail and the padding trim away.
#[test]
fn selection_copies_history_rows_as_written() {
    let grid = grid_with_known_history();
    for first in [0usize, 3, 500, 995] {
        let last = first + 4;
        let selection = Selection {
            start: (first as u64, 0),
            end: (last as u64, COLS - 1),
            anchored: true,
            ..Selection::new(first as u64, 0)
        };
        let expected: Vec<String> =
            (first..=last).map(|row| line_text(line_at_history_row(row))).collect();
        assert_eq!(selection.as_text(&grid), expected.join("\n"), "rows {first}..={last}");
    }
    for row in 0..HISTORY_ROWS {
        let line = line_at_history_row(row);
        assert_eq!(Selection::line_at(&grid, row as u64).as_text(&grid), line_text(line));
        if line % 5 == 0 {
            assert_eq!(Selection::word_at(&grid, row as u64, 7).as_text(&grid), "beta");
        }
    }
}

/// Literal and regex search find each history hit at the generated row and columns.
#[test]
fn search_finds_history_hits_at_their_columns() {
    let grid = grid_with_known_history();
    for row in (3..HISTORY_ROWS).step_by(97) {
        let line = line_at_history_row(row);
        if line % 5 != 3 {
            continue;
        }
        let needle = format!("file{line}.rs");
        let hits = find_in_grid(&grid, &needle, true);
        assert_eq!(hits.len(), 1, "{needle}");
        let start = line_text(line).find(&needle).expect("needle in text") as u16;
        assert_eq!(
            (hits[0].row as usize, hits[0].col_start, hits[0].col_end),
            (row, start, start + needle.len() as u16)
        );
    }
    let hits = find_regex_in_grid(&grid, r"https://example\.com/page\d+", true).expect("regex");
    let history_hits: Vec<_> =
        hits.iter().filter(|hit| (hit.row as usize) < HISTORY_ROWS).collect();
    let url_rows = (0..HISTORY_ROWS).filter(|row| line_at_history_row(*row) % 5 == 2).count();
    assert_eq!(history_hits.len(), url_rows);
    for hit in history_hits {
        let line = line_at_history_row(hit.row as usize);
        assert_eq!(line % 5, 2, "a URL hit on a URL row");
        assert_eq!(hit.col_start, 4);
        assert_eq!(
            usize::from(hit.col_end - hit.col_start),
            format!("https://example.com/page{line}").len()
        );
    }
}

/// URL detection over each history row's text finds exactly the generated URL.
#[test]
fn url_detection_reads_history_rows() {
    let grid = grid_with_known_history();
    for row in 0..HISTORY_ROWS {
        let line = line_at_history_row(row);
        let urls: Vec<String> = sonicterm_cfg::url_scan::find_urls(&history_row_text(&grid, row))
            .into_iter()
            .map(|found| found.url)
            .collect();
        let expected = if line % 5 == 2 {
            vec![format!("https://example.com/page{line}")]
        } else {
            Vec::new()
        };
        assert_eq!(urls, expected, "history row {row}");
    }
}
