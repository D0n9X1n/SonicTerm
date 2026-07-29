use super::*;
use sonicterm_grid::grid::Color;

// --- fixtures -------------------------------------------------------------

/// Write `s` onto the current cursor row of `grid`, one `put_char` per
/// codepoint so wide chars and combining marks land in real cells/extras.
fn write(grid: &mut Grid, s: &str) {
    for ch in s.chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A single-visible-row grid holding `s`, wide enough to avoid autowrap.
fn grid_with_line(s: &str, cols: u16) -> Grid {
    let mut grid = Grid::new(cols, 1);
    write(&mut grid, s);
    grid
}

fn blank_grid(cols: u16, rows: u16) -> Grid {
    Grid::new(cols, rows)
}

// --- QuickSelectState::from_grid + text_for_hint --------------------------

#[test]
fn from_grid_finds_ascii_url_with_grid_columns() {
    // "see http://a.co" — url starts at col 4, last url char 'o' at col 14.
    let grid = grid_with_line("see http://a.co", 20);
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.hints.len(), 1);
    let h = &qs.hints[0];
    assert_eq!(h.hint, 'a');
    assert_eq!(h.row, 0);
    assert_eq!(h.col_start, 4);
    assert_eq!(h.col_end, 14);
    assert_eq!(h.text, "http://a.co");
}

#[test]
fn from_grid_assigns_sequential_hints_in_visible_order() {
    let mut grid = Grid::new(40, 2);
    write(&mut grid, "http://a.com x https://b.org");
    grid.goto(1, 0);
    write(&mut grid, "mailto:c@d.io");
    let qs = QuickSelectState::from_grid(&grid);
    let seen: Vec<(char, usize, &str)> =
        qs.hints.iter().map(|h| (h.hint, h.row, h.text.as_str())).collect();
    assert_eq!(
        seen,
        vec![('a', 0, "http://a.com"), ('b', 0, "https://b.org"), ('c', 1, "mailto:c@d.io"),]
    );
}

#[test]
fn from_grid_recognizes_all_supported_schemes() {
    for url in ["http://a.co", "https://a.co", "mailto:a@b.co", "file://x/y"] {
        let grid = grid_with_line(url, 40);
        let qs = QuickSelectState::from_grid(&grid);
        assert_eq!(qs.hints.len(), 1, "expected a hint for {url}");
        assert_eq!(qs.hints[0].text, url);
    }
}

#[test]
fn from_grid_ignores_unsupported_schemes() {
    for text in ["ftp://a.co", "ws://a.co", "just words", "a.co no scheme"] {
        let grid = grid_with_line(text, 40);
        let qs = QuickSelectState::from_grid(&grid);
        assert!(qs.hints.is_empty(), "did not expect a hint for {text:?}");
    }
}

#[test]
fn from_grid_scans_absolute_rows_with_scrollback_offset() {
    let mut grid = Grid::new(20, 2);
    // Push two rows into scrollback so the visible window is offset.
    write(&mut grid, "top");
    grid.linefeed();
    write(&mut grid, "second");
    grid.scroll_up(2);
    assert_eq!(grid.scrollback_len(), 2);
    // Now write a URL into the (blank) first visible row.
    grid.goto(0, 0);
    write(&mut grid, "http://x.io");
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.hints.len(), 1);
    // Row is scrollback-absolute: first visible row == scrollback_len().
    assert_eq!(qs.hints[0].row, grid.scrollback_len());
}

#[test]
fn from_grid_caps_at_26_hints() {
    let mut grid = Grid::new(12, 30);
    for r in 0..30u16 {
        grid.goto(r, 0);
        write(&mut grid, "http://a.com");
    }
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.hints.len(), 26);
    assert_eq!(qs.hints.first().unwrap().hint, 'a');
    assert_eq!(qs.hints.last().unwrap().hint, 'z');
}

#[test]
fn from_grid_maps_columns_across_leading_wide_cell() {
    // '中' is a wide cell: occupies grid columns 0 and 1 (WIDE + WIDE_CONT)
    // but only one `char`. The URL 'h' therefore sits at grid column 2.
    // A char-count mapping would report col_start = 1 — the regression.
    let grid = grid_with_line("中http://a.co", 20);
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.hints.len(), 1);
    let h = &qs.hints[0];
    assert_eq!(h.text, "http://a.co");
    assert_eq!(h.col_start, 2, "wide lead cell must not shift the URL left");
    // 11 url chars starting at col 2 → last char at col 12.
    assert_eq!(h.col_end, 12);
}

#[test]
fn from_grid_maps_columns_across_leading_combining_mark() {
    // 'e' + combining acute (U+0301) is ONE grid cell (mark stored in
    // extras) but TWO chars / three bytes. The URL 'h' sits at grid
    // column 1. A byte/char mapping would over-count and report col 2.
    let grid = grid_with_line("e\u{0301}http://a.co", 20);
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.hints.len(), 1);
    let h = &qs.hints[0];
    assert_eq!(h.text, "http://a.co");
    assert_eq!(h.col_start, 1, "combining mark must not shift the URL right");
    assert_eq!(h.col_end, 11);
}

#[test]
fn text_for_hint_is_case_insensitive_and_misses_are_none() {
    let grid = grid_with_line("http://a.co", 20);
    let qs = QuickSelectState::from_grid(&grid);
    assert_eq!(qs.text_for_hint('a'), Some("http://a.co"));
    assert_eq!(qs.text_for_hint('A'), Some("http://a.co"));
    assert_eq!(qs.text_for_hint('b'), None);
}

// --- cursor movement / clamping ------------------------------------------

#[test]
fn move_left_saturates_at_zero() {
    let grid = blank_grid(10, 3);
    let mut s = CopyModeState::new_at((0, 1));
    s.move_left(&grid);
    assert_eq!(s.cursor, (0, 1));
}

#[test]
fn move_right_clamps_to_last_column() {
    let grid = blank_grid(5, 3); // max col = 4
    let mut s = CopyModeState::new_at((4, 0));
    s.move_right(&grid);
    assert_eq!(s.cursor, (4, 0));
}

#[test]
fn move_up_saturates_and_down_clamps_to_last_row() {
    let grid = blank_grid(5, 3); // max row = 2 (no scrollback)
    let mut top = CopyModeState::new_at((1, 0));
    top.move_up(&grid);
    assert_eq!(top.cursor, (1, 0));

    let mut bottom = CopyModeState::new_at((1, 2));
    bottom.move_down(&grid);
    assert_eq!(bottom.cursor, (1, 2));
}

#[test]
fn move_line_end_lands_on_last_non_blank_then_home_resets() {
    let grid = grid_with_line("hi", 10); // 'h' col0, 'i' col1, rest blank
    let mut s = CopyModeState::new_at((5, 0));
    s.move_line_end(&grid);
    assert_eq!(s.cursor.0, 1);
    s.move_line_start(&grid);
    assert_eq!(s.cursor.0, 0);
}

#[test]
fn move_line_end_on_blank_row_uses_last_column() {
    let grid = blank_grid(8, 1); // max col 7, fully blank
    let mut s = CopyModeState::new_at((0, 0));
    s.move_line_end(&grid);
    assert_eq!(s.cursor.0, 7);
}

#[test]
fn move_top_and_bottom_clamp_column_and_row() {
    let grid = blank_grid(6, 4); // max col 5, max row 3
    let mut s = CopyModeState::new_at((5, 2));
    s.move_top(&grid);
    assert_eq!(s.cursor, (5, 0));
    s.move_bottom(&grid);
    assert_eq!(s.cursor, (5, 3));
}

#[test]
fn stale_cursor_beyond_grid_is_clamped_on_next_move() {
    let grid = blank_grid(4, 2); // max col 3, max row 1
    let mut s = CopyModeState::new_at((99, 99));
    s.move_left(&grid); // clamps to (3,1) then steps left
    assert_eq!(s.cursor, (2, 1));
}

#[test]
fn move_word_fwd_crosses_row_boundary() {
    let mut grid = Grid::new(6, 2);
    write(&mut grid, "ab"); // row 0: a b _ _ _ _
    grid.goto(1, 0);
    write(&mut grid, "cd"); // row 1: c d
    let mut s = CopyModeState::new_at((0, 0)); // on 'a'
    s.move_word_fwd(&grid);
    assert_eq!(s.cursor, (0, 1)); // next word starts at 'c' on row 1
}

#[test]
fn move_word_back_crosses_row_boundary() {
    let mut grid = Grid::new(6, 2);
    write(&mut grid, "ab");
    grid.goto(1, 0);
    write(&mut grid, "cd");
    let mut s = CopyModeState::new_at((0, 1)); // on 'c'
    s.move_word_back(&grid);
    assert_eq!(s.cursor, (0, 0)); // back to start of 'ab' on row 0
}

// --- anchor / selection transitions --------------------------------------

#[test]
fn start_select_sets_anchor_once_and_enters_select_mode() {
    let grid = blank_grid(10, 3);
    let mut s = CopyModeState::new_at((2, 1));
    s.start_select();
    assert_eq!(s.mode, CopyMode::Select);
    assert_eq!(s.anchor, Some((2, 1)));
    // Moving then re-calling must not move the anchor.
    s.move_right(&grid);
    s.start_select();
    assert_eq!(s.anchor, Some((2, 1)));
}

#[test]
fn selected_range_normalizes_reversed_selection() {
    let mut s = CopyModeState::new_at((3, 4));
    s.start_select();
    // Move cursor "above/left" of the anchor so cursor < anchor.
    s.cursor = (1, 2);
    let (start, end) = s.selected_range().expect("anchored selection");
    assert_eq!(start, (1, 2));
    assert_eq!(end, (3, 4));
}

#[test]
fn selected_range_is_none_without_anchor() {
    let mut s = CopyModeState::new_at((0, 0));
    assert_eq!(s.selected_range(), None);
    s.mode = CopyMode::Select; // mode alone, still no anchor
    assert_eq!(s.selected_range(), None);
}

// --- read-only guards -----------------------------------------------------

#[test]
fn read_only_blocks_start_select() {
    let mut s = CopyModeState::read_only_at((1, 1));
    assert!(s.is_read_only());
    s.start_select();
    assert_eq!(s.anchor, None);
    assert_eq!(s.mode, CopyMode::Cursor);
    assert_eq!(s.selected_range(), None);
}

#[test]
fn read_only_suppresses_range_even_with_anchor() {
    // Build a selection first, then flip read_only on to prove the guard
    // is enforced at read time, not only at start_select time.
    let mut s = CopyModeState::new_at((2, 0));
    s.start_select();
    s.cursor = (5, 0);
    assert!(s.selected_range().is_some());
    s.read_only = true;
    assert_eq!(s.selected_range(), None);
}

// --- quick-select lifecycle ----------------------------------------------

#[test]
fn quick_select_lifecycle_set_query_clear() {
    let grid = grid_with_line("http://a.co", 20);
    let mut s = CopyModeState::new_at((0, 0));
    assert!(s.quick_select.is_none());

    s.quick_select = Some(QuickSelectState::from_grid(&grid));
    let qs = s.quick_select.as_ref().unwrap();
    assert_eq!(qs.hints.len(), 1);
    assert_eq!(qs.text_for_hint('a'), Some("http://a.co"));

    s.quick_select = None;
    assert!(s.quick_select.is_none());
}

// --- production shapes: read-only entry and quick-select snapshot ----------

/// A read-only state must not expose a selection, however it was driven.
fn assert_no_selection(s: &CopyModeState, label: &str) {
    assert_eq!(s.anchor, None, "anchor set {label}");
    assert_eq!(s.mode, CopyMode::Cursor, "mode left Cursor {label}");
    assert_eq!(s.selected_range(), None, "range produced {label}");
}

#[test]
fn read_only_entry_yields_no_selection_through_full_key_sequence() {
    let mut grid = Grid::new(20, 2);
    write(&mut grid, "alpha beta");
    grid.goto(1, 0);
    write(&mut grid, "gamma delta");

    // The shape entering copy mode builds: read-only, unanchored, no hints.
    let mut s = CopyModeState::read_only_at((0, 0));
    assert!(s.is_read_only());
    assert!(s.quick_select.is_none());

    // Every motion the key handler can reach, with `start_select` interleaved
    // the way pressing `v` between motions would drive it.
    s.start_select();
    assert_no_selection(&s, "after start_select at origin");

    s.move_right(&grid);
    s.move_down(&grid);
    assert_eq!(s.cursor, (1, 1), "read-only suppresses selection, not motion");
    s.start_select();
    assert_no_selection(&s, "after start_select mid-grid");

    s.move_word_fwd(&grid);
    s.move_line_end(&grid);
    s.move_bottom(&grid);
    s.start_select();
    assert_no_selection(&s, "after start_select at bottom");

    s.move_word_back(&grid);
    s.move_line_start(&grid);
    s.move_top(&grid);
    s.move_left(&grid);
    s.move_up(&grid);
    s.start_select();
    assert_no_selection(&s, "after full traversal");
}

#[test]
fn quick_select_shape_has_no_anchor_or_range() {
    let grid = grid_with_line("http://a.co", 20);
    // The shape entering quick select builds: not read-only, hints attached.
    let mut s = CopyModeState::new_at((0, grid.scrollback_len()));
    s.quick_select = Some(QuickSelectState::from_grid(&grid));

    assert!(!s.is_read_only());
    assert_eq!(s.quick_select.as_ref().unwrap().hints.len(), 1);
    // Not read-only, yet still no range: the hint path never anchors.
    assert_eq!(s.anchor, None);
    assert_eq!(s.mode, CopyMode::Cursor);
    assert_eq!(s.selected_range(), None);

    // A hint keypress copies hint text, and copying leaves the range absent.
    assert_eq!(s.quick_select.as_ref().unwrap().text_for_hint('a'), Some("http://a.co"));
    assert_eq!(s.selected_range(), None);
}

#[test]
fn quick_select_text_is_a_snapshot_immune_to_later_grid_writes() {
    let mut grid = grid_with_line("http://a.co", 20);
    let captured = QuickSelectState::from_grid(&grid);
    assert_eq!(captured.text_for_hint('a'), Some("http://a.co"));

    // The pane keeps producing output: overwrite the scanned row in place.
    grid.goto(0, 0);
    write(&mut grid, "http://b.co");

    // A fresh scan sees the new text, so the grid really did change...
    assert_eq!(QuickSelectState::from_grid(&grid).text_for_hint('a'), Some("http://b.co"));
    // ...while the captured snapshot still yields what it scanned.
    assert_eq!(captured.text_for_hint('a'), Some("http://a.co"));
    assert_eq!(captured.hints[0].text, "http://a.co");
}

#[test]
fn quick_select_text_survives_scrollback_shift() {
    let mut grid = Grid::new(20, 2);
    write(&mut grid, "http://a.co");
    let captured = QuickSelectState::from_grid(&grid);
    assert_eq!(captured.hints[0].row, 0);

    // Output scrolls the scanned row out of the visible window.
    grid.scroll_up(1);
    assert_eq!(grid.scrollback_len(), 1);

    // The hint's row is stale now, but the text it copies is intact.
    assert!(QuickSelectState::from_grid(&grid).hints.is_empty());
    assert_eq!(captured.text_for_hint('a'), Some("http://a.co"));
}
