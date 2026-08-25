use super::*;
use sonicterm_grid::grid::{Color, Grid};

/// Build a 1-row grid wide enough to hold `text`, writing one cell per
/// char from column 0. Mirrors the put_char usage in grid.rs tests.
fn grid_with(text: &str) -> Grid {
    let cols = text.chars().count().max(1) as u16;
    let mut grid = Grid::new(cols, 1);
    grid.goto(0, 0);
    for ch in text.chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    grid
}

/// Build a multi-row grid from `lines`, writing each line left-aligned
/// from column 0 of its row. Grid width is the widest line (min 1).
/// Used by the `word_drag` / `line_drag` cross-row tests.
fn grid_rows(lines: &[&str]) -> Grid {
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as u16;
    let rows = lines.len().max(1) as u16;
    let mut grid = Grid::new(cols, rows);
    for (r, line) in lines.iter().enumerate() {
        grid.goto(r as u16, 0);
        for ch in line.chars() {
            grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    grid
}

// ---- word_bounds (pure helper) ----

#[test]
fn word_bounds_inside_connector_word() {
    // "foo bar.baz qux"
    //  0123456789012345
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // col 8 is the 'b' of "baz" inside "bar.baz"; the '.' connector
    // keeps "bar.baz" a single word spanning cols 4..=10.
    assert_eq!(word_bounds(&chars, 8), (4, 10));
    // The connector '.' itself (col 7) is a word char → same span.
    assert_eq!(word_bounds(&chars, 7), (4, 10));
}

#[test]
fn word_bounds_on_space_is_single_cell() {
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // col 3 is the space between "foo" and "bar.baz".
    assert_eq!(word_bounds(&chars, 3), (3, 3));
    // col 11 is the space before "qux".
    assert_eq!(word_bounds(&chars, 11), (11, 11));
}

#[test]
fn word_bounds_stops_at_word_edges() {
    let chars: Vec<char> = "foo bar.baz qux".chars().collect();
    // Start of "foo".
    assert_eq!(word_bounds(&chars, 0), (0, 2));
    // End of "foo".
    assert_eq!(word_bounds(&chars, 2), (0, 2));
    // Start of "qux".
    assert_eq!(word_bounds(&chars, 12), (12, 14));
    // End of "qux".
    assert_eq!(word_bounds(&chars, 14), (12, 14));
}

#[test]
fn word_bounds_empty_slice() {
    assert_eq!(word_bounds(&[], 0), (0, 0));
    assert_eq!(word_bounds(&[], 5), (0, 0));
}

// ---- word_at / line_at (Grid constructors) ----

#[test]
fn word_at_selects_connector_word() {
    let grid = grid_with("foo bar.baz qux");
    // Click inside "baz" (col 8) → whole "bar.baz" (cols 4..=10).
    let sel = Selection::word_at(&grid, 0, 8);
    assert_eq!(sel.start, (0, 4));
    assert_eq!(sel.end, (0, 10));
    assert_eq!(sel.as_text(&grid), "bar.baz");
}

#[test]
fn word_at_on_space_selects_single_cell() {
    let grid = grid_with("foo bar.baz qux");
    let sel = Selection::word_at(&grid, 0, 3);
    assert_eq!(sel.start, (0, 3));
    assert_eq!(sel.end, (0, 3));
}

#[test]
fn word_at_clamps_and_stops_at_boundaries() {
    let grid = grid_with("foo bar.baz qux");
    // Start of "foo".
    let sel = Selection::word_at(&grid, 0, 0);
    assert_eq!((sel.start, sel.end), ((0, 0), (0, 2)));
    // End of last word, with an out-of-range col that clamps to last.
    let sel = Selection::word_at(&grid, 0, 999);
    assert_eq!((sel.start, sel.end), ((0, 12), (0, 14)));
}

#[test]
fn line_at_spans_full_row() {
    let grid = grid_with("foo bar.baz qux");
    let sel = Selection::line_at(&grid, 0);
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (0, 14)); // last col = len - 1
    assert_eq!(sel.as_text(&grid), "foo bar.baz qux");
}

// ---- word_drag / line_drag (WezTerm SelectionMode drag) ----

#[test]
fn word_drag_same_row_forward_includes_both_words() {
    // "foo bar.baz qux" — anchor in "foo" (col 1), drag onto "qux"
    // (col 13). The union spans the whole "foo" word through the whole
    // "qux" word: cols 0..=14.
    let grid = grid_with("foo bar.baz qux");
    let sel = Selection::word_drag(&grid, (0, 1), (0, 13));
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (0, 14));
    assert!(sel.anchored);
    assert_eq!(sel.as_text(&grid), "foo bar.baz qux");
}

#[test]
fn word_drag_same_row_backward_includes_both_words() {
    // Anchor in "qux" (col 13), drag BACK onto "foo" (col 1). The union
    // is identical to the forward case — order-independent.
    let grid = grid_with("foo bar.baz qux");
    let sel = Selection::word_drag(&grid, (0, 13), (0, 1));
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (0, 14));
    assert!(sel.anchored);
}

#[test]
fn word_drag_cursor_inside_anchor_word_equals_word_at_anchor() {
    // Anchor on "bar.baz" (col 4), cursor still inside that same word
    // (col 9). The union must collapse to exactly word_at(anchor) — the
    // selection never shrinks below, but also never grows past, the
    // anchor word when the cursor never leaves it.
    let grid = grid_with("foo bar.baz qux");
    let anchor = Selection::word_at(&grid, 0, 4);
    let sel = Selection::word_drag(&grid, (0, 4), (0, 9));
    assert_eq!(sel.start, anchor.start);
    assert_eq!(sel.end, anchor.end);
    assert_eq!((sel.start, sel.end), ((0, 4), (0, 10)));
}

#[test]
fn word_drag_cross_row_unions_by_corner() {
    // Row 0 "alpha beta", row 1 "gamma delta". Anchor in "beta"
    // (row 0, col 7), drag down into "gamma" (row 1, col 2). The union
    // start is the top-left corner (beta's start), the end is the
    // bottom-right corner (gamma's end).
    let grid = grid_rows(&["alpha beta", "gamma delta"]);
    let sel = Selection::word_drag(&grid, (0, 7), (1, 2));
    assert_eq!(sel.start, (0, 6)); // "beta" starts at col 6
    assert_eq!(sel.end, (1, 4)); // "gamma" ends at col 4
    assert!(sel.anchored);
    // Backward drag (anchor below, cursor above) yields the same union.
    let rev = Selection::word_drag(&grid, (1, 2), (0, 7));
    assert_eq!((rev.start, rev.end), (sel.start, sel.end));
}

#[test]
fn line_drag_forward_spans_full_rows() {
    let grid = grid_rows(&["first line", "second line", "third line"]);
    // Anchor row 0, drag down to row 2. Spans row 0 col 0 through the
    // last col of row 2.
    let sel = Selection::line_drag(&grid, 0, 2);
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (2, grid.row(2).len() as u16 - 1));
    assert!(sel.anchored);
    assert_eq!(sel.as_text(&grid), "first line\nsecond line\nthird line");
}

#[test]
fn line_drag_backward_spans_full_rows() {
    let grid = grid_rows(&["first line", "second line", "third line"]);
    // Anchor row 2, drag UP to row 0 — same inclusive row span as the
    // forward case; end is still the last col of the bottom row (2).
    let sel = Selection::line_drag(&grid, 2, 0);
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (2, grid.row(2).len() as u16 - 1));
    assert!(sel.anchored);
}

#[test]
fn line_drag_single_row_is_full_line() {
    // Anchor == cursor row: collapses to a single full row, identical
    // to line_at — the selection never drops below the anchor line.
    let grid = grid_rows(&["only row here"]);
    let sel = Selection::line_drag(&grid, 0, 0);
    let line = Selection::line_at(&grid, 0);
    assert_eq!((sel.start, sel.end), (line.start, line.end));
}

// ---- anchored vs point-anchor emptiness (single-cell edge case) ----

#[test]
fn point_select_new_is_empty() {
    // A bare single-click point anchor (start == end, not anchored) is
    // empty, so release-clear/copy still treat it as "no selection".
    let sel = Selection::new(3, 7);
    assert_eq!(sel.start, sel.end);
    assert!(!sel.anchored);
    assert!(sel.is_empty());
}

#[test]
fn word_at_single_char_word_is_not_empty() {
    // Double-clicking a one-character word ("x") yields start == end but
    // is a deliberate, anchored selection — it must NOT read as empty,
    // or it would be invisible / uncopyable / cleared on release.
    let grid = grid_with("x");
    let sel = Selection::word_at(&grid, 0, 0);
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end, (0, 0));
    assert!(sel.anchored);
    assert!(!sel.is_empty());
    assert!(sel.content_fingerprint.is_some());
    assert_eq!(sel.as_text(&grid), "x");
}

/// Build a multi-row grid then scroll `scroll` rows into scrollback, so
/// the live region sits at absolute rows `scroll..`. Returns the grid;
/// the first `scroll` lines are addressable only via `row_at_abs`.
fn grid_scrolled(lines: &[&str], scroll: u16) -> Grid {
    let mut grid = grid_rows(lines);
    grid.scroll_up(scroll);
    grid
}

#[test]
fn word_at_reads_scrollback_absolute_row() {
    // 2 visible rows; scroll 2 → both originals land in scrollback at
    // abs 0 ("alpha beta") and abs 1 ("gamma delta"); live rows are
    // blank at abs 2..=3. word_at must read the scrollback line.
    let grid = grid_scrolled(&["alpha beta", "gamma delta"], 2);
    assert_eq!(grid.scrollback_len(), 2);
    // abs row 1 = "gamma delta"; click col 2 → whole "gamma" (0..=4).
    let sel = Selection::word_at(&grid, 1, 2);
    assert_eq!(sel.start, (1, 0));
    assert_eq!(sel.end, (1, 4));
    assert_eq!(sel.as_text(&grid), "gamma");
}

#[test]
fn line_at_and_as_text_read_scrollback_absolute_row() {
    let grid = grid_scrolled(&["alpha beta", "gamma delta"], 2);
    // abs row 0 = "alpha beta" (now in scrollback).
    let sel = Selection::line_at(&grid, 0);
    assert_eq!(sel.start, (0, 0));
    assert_eq!(sel.end.0, 0);
    assert_eq!(sel.as_text(&grid), "alpha beta");
}

#[test]
fn multiline_copy_omits_whitespace_separated_right_edge_frame_glyphs() {
    let mut grid = Grid::new(24, 3);
    for (row, (text, frame)) in
        [("[Environment]::Set(", '│'), ("    \"VALUE\",", '│'), (")", '╯')].into_iter().enumerate()
    {
        grid.goto(row as u16, 0);
        for ch in text.chars() {
            grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
        grid.goto(row as u16, grid.cols - 1);
        grid.put_char(frame, Color::Default, Color::Default, CellFlags::empty());
    }
    let selection = Selection {
        start: (0, 0),
        end: (2, grid.cols - 1),
        anchored: true,
        pane_id: None,
        content_seq: 0,
        on_alt_screen: false,
        scrollback_evicted: 0,
        content_fingerprint: None,
    };

    assert_eq!(selection.as_text(&grid), "[Environment]::Set(\n    \"VALUE\",\n)");
}

#[test]
fn partial_final_row_selection_omits_coherent_right_edge_frame() {
    let mut grid = Grid::new(11, 3);
    for (row, (text, frame)) in
        [("first", '│'), ("middle", '│'), ("last", '┘')].into_iter().enumerate()
    {
        grid.goto(row as u16, 0);
        for ch in text.chars() {
            grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
        grid.goto(row as u16, grid.cols - 1);
        grid.put_char(frame, Color::Default, Color::Default, CellFlags::empty());
    }
    let selection = Selection {
        start: (0, 0),
        end: (2, 3),
        anchored: true,
        pane_id: None,
        content_seq: 0,
        on_alt_screen: false,
        scrollback_evicted: 0,
        content_fingerprint: None,
    };

    assert_eq!(selection.as_text(&grid), "first\nmiddle\nlast");
}

#[test]
fn copy_preserves_ambiguous_detached_box_glyph_at_right_edge() {
    let grid = grid_with("foo     │");
    let selection = Selection::line_at(&grid, 0);

    assert_eq!(selection.as_text(&grid), "foo     │");
}

#[test]
fn copy_preserves_incomplete_multiline_right_edge_frame_pattern() {
    let grid = grid_rows(&["foo     │", "bar     │"]);
    let selection = Selection::line_drag(&grid, 0, 1);

    assert_eq!(selection.as_text(&grid), "foo     │\nbar     │");
}

#[test]
fn copy_preserves_box_drawing_that_is_not_a_detached_right_edge_frame() {
    let grid = grid_with("Write-Output '│'");
    let selection = Selection::line_at(&grid, 0);

    assert_eq!(selection.as_text(&grid), "Write-Output '│'");
}

#[test]
fn copy_preserves_right_edge_border_attached_to_wide_glyph() {
    let mut grid = Grid::new(3, 1);
    grid.put_char('デ', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(0, 2);
    grid.put_char('│', Color::Default, Color::Default, CellFlags::empty());
    let selection = Selection::line_at(&grid, 0);

    assert_eq!(selection.as_text(&grid), "デ│");
}

#[test]
fn as_text_spans_scrollback_into_live_region() {
    // Scroll only 1 row: abs 0 = "alpha beta" (scrollback), abs 1 =
    // "gamma delta" (still live, the bottom visible row). A cross-row
    // selection must read both the scrollback and the live row.
    let grid = grid_scrolled(&["alpha beta", "gamma delta"], 1);
    assert_eq!(grid.scrollback_len(), 1);
    let sel = Selection {
        start: (0, 0),
        end: (1, 10),
        anchored: true,
        pane_id: None,
        content_seq: 0,
        on_alt_screen: false,
        scrollback_evicted: 0,
        content_fingerprint: None,
    };
    assert_eq!(sel.as_text(&grid), "alpha beta\ngamma delta");
}

#[test]
fn as_text_stops_at_unavailable_absolute_row() {
    // end.row past the bottom of the buffer: the walk stops cleanly
    // (no panic) and yields only the rows that exist.
    let grid = grid_rows(&["only line"]);
    let sel = Selection {
        start: (0, 0),
        end: (50, 5),
        anchored: true,
        pane_id: None,
        content_seq: 0,
        on_alt_screen: false,
        scrollback_evicted: 0,
        content_fingerprint: None,
    };
    assert_eq!(sel.as_text(&grid), "only line");
}

// ---- selection invalidation after alternate-screen content changes ----

const PANE_ID: u64 = 7;

fn alt_grid() -> Grid {
    let mut grid = Grid::new(12, 12);
    grid.enter_alt_screen();
    grid
}

fn anchored_selection(grid: &Grid, start_row: u64, end_row: u64) -> Selection {
    Selection {
        start: (start_row, 2),
        end: (end_row, 7),
        anchored: true,
        pane_id: Some(PANE_ID),
        content_seq: grid.content_seq(),
        on_alt_screen: grid.is_alt(),
        scrollback_evicted: grid.scrollback_evicted(),
        content_fingerprint: None,
    }
    .with_content_fingerprint(grid)
}

fn write_row(grid: &mut Grid, row: u16, ch: char) {
    grid.goto(row, 0);
    grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
}

/// Repainting selected cells to the same complete value preserves the selection.
#[test]
fn same_value_repaint_preserves_selection() {
    let mut grid = alt_grid();
    write_row(&mut grid, 4, 'x');
    let mut selection = anchored_selection(&grid, 4, 4);
    let before = selection.content_seq;

    let same = grid.row(4)[0].clone();
    grid.row_mut(4)[0] = same;

    assert!(grid.content_seq() > before);
    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));
    assert_eq!(selection.content_seq, grid.content_seq());
}

/// A complete primary-to-alternate-to-primary round trip replaces buffer identity even when restored cells match.
#[test]
fn screen_buffer_round_trip_invalidates_without_intermediate_revalidation() {
    let mut grid = Grid::new(12, 12);
    write_row(&mut grid, 4, 'x');
    let mut selection = anchored_selection(&grid, 4, 4);

    grid.enter_alt_screen();
    grid.leave_alt_screen();

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

/// Every logical cell-identity class invalidates a selected range when changed.
#[test]
fn character_style_hyperlink_wide_and_combining_changes_invalidate_selection() {
    let mutations: [fn(&mut sonicterm_grid::grid::Cell); 5] = [
        |cell| cell.ch = 'y',
        |cell| cell.fg = Color::Indexed(3),
        |cell| cell.set_hyperlink(Some(sonicterm_types::HyperlinkId(7))),
        |cell| cell.flags.insert(CellFlags::WIDE),
        |cell| cell.set_extras(Some("\u{301}".into())),
    ];
    for mutate in mutations {
        let mut grid = alt_grid();
        write_row(&mut grid, 4, 'x');
        let mut selection = anchored_selection(&grid, 4, 4);
        mutate(&mut grid.row_mut(4)[2]);
        assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
    }
}

#[test]
fn content_change_on_a_selected_alt_row_invalidates_the_selection() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 4, 6);

    write_row(&mut grid, 6, 'x');

    assert!(
        revalidate_selection(&mut selection, PANE_ID, &grid),
        "selected row 6 changed after selection, so it no longer necessarily contains the chosen text"
    );
}

#[test]
fn unrelated_alt_rows_preserve_the_selection() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 4, 6);

    for (row, ch) in [(0, 'a'), (3, 'b'), (7, 'c'), (11, 'd')] {
        write_row(&mut grid, row, ch);
    }

    assert!(
        !revalidate_selection(&mut selection, PANE_ID, &grid),
        "an unrelated status line or spinner update must not make selection impossible in a live TUI"
    );
}

#[test]
fn content_intersection_normalizes_a_backwards_selection() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 9, 3);

    write_row(&mut grid, 5, 'x');
    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));

    let mut selection = anchored_selection(&grid, 9, 3);
    write_row(&mut grid, 2, 'y');
    write_row(&mut grid, 10, 'z');
    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn content_changed_before_selection_does_not_invalidate_it() {
    let mut grid = alt_grid();
    write_row(&mut grid, 4, 'x');
    let mut selection = anchored_selection(&grid, 4, 6);

    grid.mark_all_dirty();

    assert!(
        !revalidate_selection(&mut selection, PANE_ID, &grid),
        "dirty state older than the selection baseline must not clear a fresh selection"
    );
}

#[test]
fn extending_a_selection_advances_its_content_baseline() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 4, 6);
    write_row(&mut grid, 5, 'x');

    selection.extend_with_content_state(
        6,
        8,
        PANE_ID,
        grid.content_seq(),
        grid.is_alt(),
        grid.scrollback_evicted(),
    );
    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));

    write_row(&mut grid, 5, 'y');
    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn a_bare_point_anchor_is_not_invalidated_as_a_selection() {
    let mut grid = alt_grid();
    let mut selection = Selection::new(4, 2).with_content_state(
        PANE_ID,
        grid.content_seq(),
        grid.is_alt(),
        grid.scrollback_evicted(),
    );
    write_row(&mut grid, 4, 'x');

    assert!(
        !revalidate_selection(&mut selection, PANE_ID, &grid),
        "a click point that release handling treats as empty must not become a content selection"
    );
}

/// Scrollback eviction rebases a point anchor before its first drag motion.
#[test]
fn bare_primary_point_rebases_before_selection_extension() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(1);
    write_row(&mut grid, 0, 'A');
    write_row(&mut grid, 1, 'B');
    grid.scroll_up(1);
    let mut selection = Selection::new(1, 0).with_content_state(
        PANE_ID,
        grid.content_seq(),
        false,
        grid.scrollback_evicted(),
    );
    write_row(&mut grid, 1, 'C');
    grid.scroll_up(1);

    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));
    assert_eq!(selection.start, (0, 0));
}

#[test]
fn an_anchored_single_cell_is_invalidated_when_its_alt_row_changes() {
    let mut grid = alt_grid();
    let mut selection = Selection {
        start: (4, 2),
        end: (4, 2),
        anchored: true,
        pane_id: Some(PANE_ID),
        content_seq: grid.content_seq(),
        on_alt_screen: true,
        scrollback_evicted: 0,
        content_fingerprint: None,
    };
    write_row(&mut grid, 4, 'x');

    assert!(
        revalidate_selection(&mut selection, PANE_ID, &grid),
        "a double-clicked one-character word is real even when its endpoints are equal"
    );
}

#[test]
fn changing_the_active_pane_invalidates_the_window_selection() {
    let grid = alt_grid();
    let mut selection = anchored_selection(&grid, 4, 6);

    assert!(
        revalidate_selection(&mut selection, PANE_ID + 1, &grid),
        "a window selection belongs to the pane it was made in; rendering or copying it against a different active pane would target unrelated content"
    );
}

#[test]
fn changing_panes_invalidates_a_primary_selection_too() {
    let grid = Grid::new(12, 3);
    let mut selection = anchored_selection(&grid, 1, 1);

    assert!(
        revalidate_selection(&mut selection, PANE_ID + 1, &grid),
        "a window-level selection must never be interpreted against another pane"
    );
}

#[test]
fn a_fully_out_of_view_selection_survives_visible_alt_changes() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 40, 50);
    write_row(&mut grid, 5, 'x');

    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn primary_scroll_preserves_unchanged_selected_text() {
    let mut grid = Grid::new(12, 3);
    write_row(&mut grid, 1, 'x');
    let mut selection = anchored_selection(&grid, 1, 1);

    grid.scroll_up(1);

    assert!(
        !revalidate_selection(&mut selection, PANE_ID, &grid),
        "ordinary scrolling moves the same selected text into history without replacing it"
    );
}

#[test]
fn rewriting_a_selected_primary_row_invalidates_it() {
    let mut grid = Grid::new(12, 3);
    let mut selection = anchored_selection(&grid, 1, 1);

    grid.goto(1, 2);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn scrollback_eviction_rebases_a_surviving_primary_selection() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(1);
    write_row(&mut grid, 0, 'A');
    write_row(&mut grid, 1, 'B');
    grid.scroll_up(1);
    let mut selection = Selection {
        start: (1, 0),
        end: (1, 0),
        anchored: true,
        pane_id: Some(PANE_ID),
        content_seq: grid.content_seq(),
        on_alt_screen: false,
        scrollback_evicted: grid.scrollback_evicted(),
        content_fingerprint: None,
    };
    write_row(&mut grid, 1, 'C');

    grid.scroll_up(1);

    assert!(!revalidate_selection(&mut selection, PANE_ID, &grid));
    assert_eq!((selection.start.0, selection.end.0), (0, 0));
    assert_eq!(selection.as_text(&grid), "B");
}

#[test]
fn primary_write_then_scroll_still_invalidates_the_selected_history_row() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(4);
    write_row(&mut grid, 0, 'A');
    let mut selection = Selection {
        start: (0, 0),
        end: (0, 0),
        anchored: true,
        pane_id: Some(PANE_ID),
        content_seq: grid.content_seq(),
        on_alt_screen: false,
        scrollback_evicted: grid.scrollback_evicted(),
        content_fingerprint: None,
    };

    write_row(&mut grid, 0, 'X');
    grid.scroll_up(1);

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn scrollback_eviction_clears_a_range_that_lost_selected_rows() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(1);
    write_row(&mut grid, 0, 'A');
    grid.scroll_up(1);
    let mut selection = Selection {
        start: (0, 0),
        end: (1, 0),
        anchored: true,
        pane_id: Some(PANE_ID),
        content_seq: grid.content_seq(),
        on_alt_screen: false,
        scrollback_evicted: grid.scrollback_evicted(),
        content_fingerprint: None,
    };

    grid.scroll_up(1);

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn entering_alt_screen_invalidates_a_primary_selection() {
    let mut grid = Grid::new(12, 3);
    let mut selection = anchored_selection(&grid, 1, 1);

    grid.enter_alt_screen();

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}

#[test]
fn leaving_alt_screen_invalidates_an_alt_selection() {
    let mut grid = alt_grid();
    let mut selection = anchored_selection(&grid, 1, 1);

    grid.leave_alt_screen();

    assert!(revalidate_selection(&mut selection, PANE_ID, &grid));
}
