use super::*;
use sonicterm_grid::grid::Color;

fn state_with_matches() -> SearchState {
    SearchState {
        matches: vec![
            MatchRange { row: 10, col_start: 2, col_end: 5 },
            MatchRange { row: 20, col_start: 8, col_end: 9 },
            MatchRange { row: 30, col_start: 1, col_end: 3 },
        ],
        ..SearchState::new()
    }
}

#[test]
fn first_enter_selects_nearest_match_to_cursor() {
    let mut s = state_with_matches();
    s.select_nearest(19, 0);
    assert_eq!(s.current, Some(1));
    assert_eq!(s.requested_scroll_row, Some(20));
}

#[test]
fn arrow_direction_selects_relative_to_cursor_when_unselected() {
    let mut down = state_with_matches();
    down.next_from(20, 8);
    assert_eq!(down.current, Some(2));

    let mut up = state_with_matches();
    up.prev_from(20, 8);
    assert_eq!(up.current, Some(0));
}

/// Viewport anchoring retains global indices and never queues a scroll while typing.
#[test]
fn viewport_anchor_uses_document_order_without_scrolling() {
    let mut search = state_with_matches();
    for (top, expected) in [(0, 0), (10, 0), (11, 1), (30, 2), (31, 2)] {
        search.anchor_to_viewport(top);
        assert_eq!(search.current, Some(expected));
        assert_eq!(search.requested_scroll_row, None);
    }
    search.matches.clear();
    search.anchor_to_viewport(0);
    assert_eq!(search.current, None);
}

/// Rebinding two equal-revision grids must discard the prior pane's match identity.
#[test]
fn pane_identity_invalidates_equal_revision_matches() {
    let grid = Grid::new(20, 2);
    let mut search = state_with_matches();
    search.bind_pane(7);
    search.refresh(&grid);
    search.matches = state_with_matches().matches;
    search.current = Some(1);
    search.bind_pane(8);
    assert_eq!(search.current, None);
    assert!(search.maybe_refresh_for_revision(&grid));
    assert!(search.matches.is_empty());
}

#[test]
fn search_ignores_newline_input() {
    let grid = Grid::new(10, 2);
    let mut s = SearchState::new();
    s.input_char('a', &grid);
    s.input_char('\n', &grid);
    s.input_char('\r', &grid);
    s.input_char('b', &grid);
    assert_eq!(s.query, "ab");
}

#[test]
fn search_accepts_ime_commit_text_as_single_line() {
    let grid = Grid::new(10, 2);
    let mut s = SearchState::new();
    s.input_str("你\r\n好\n世界", &grid);
    assert_eq!(s.query, "你好世界");
}

#[test]
fn search_inserts_committed_text_at_the_unicode_caret() {
    let grid = Grid::new(20, 2);
    let mut s = SearchState::new();
    s.set_query("你🙂好", &grid);
    s.apply_text_edit(crate::text_edit::TextEdit::MoveStart, &grid);
    s.apply_text_edit(crate::text_edit::TextEdit::MoveForward, &grid);
    s.input_str("A\r\nB", &grid);

    assert_eq!(s.query, "你AB🙂好");
    assert_eq!(s.cursor(), "你AB".len());
}

#[test]
fn search_core_deletions_refresh_matches() {
    let mut grid = Grid::new(20, 1);
    for ch in "alpha beta".chars() {
        grid.put_char(
            ch,
            sonicterm_grid::grid::Color::Default,
            sonicterm_grid::grid::Color::Default,
            CellFlags::empty(),
        );
    }
    let mut s = SearchState::new();
    s.set_query("alpha beta", &grid);
    assert_eq!(s.matches.len(), 1);

    s.apply_text_edit(crate::text_edit::TextEdit::DeletePreviousWord, &grid);

    assert_eq!(s.query, "alpha ");
    assert_eq!(s.cursor(), s.query.len());
    assert_eq!(s.matches.len(), 1, "mutating the query must recompute search matches");
}

#[test]
fn search_caret_movement_does_not_reset_the_current_match() {
    let grid = Grid::new(20, 1);
    let mut s = SearchState::new();
    s.set_query("needle", &grid);
    s.matches = state_with_matches().matches;
    s.current = Some(1);

    s.apply_text_edit(crate::text_edit::TextEdit::MoveStart, &grid);
    s.apply_text_edit(crate::text_edit::TextEdit::MoveForward, &grid);

    assert_eq!(s.cursor(), 1);
    assert_eq!(s.current, Some(1));
}

#[test]
fn visible_match_range_bounds_to_viewport() {
    // Rows 10, 20, 30 (matches the shared fixture's ordering).
    let s = state_with_matches();
    // Viewport [15, 25) -> only the row-20 match (index 1).
    assert_eq!(s.visible_match_range(15, 10), (1, 2));
    // Viewport [0, 10) -> nothing (row 10 is excluded by the half-open top).
    assert_eq!(s.visible_match_range(0, 10), (0, 0));
    // Viewport covering everything.
    assert_eq!(s.visible_match_range(0, 100), (0, 3));
    // Viewport above all matches.
    assert_eq!(s.visible_match_range(40, 10), (3, 3));
}

#[test]
fn visible_match_range_includes_all_matches_on_a_boundary_row() {
    // Multiple matches on the same row must all fall inside the window —
    // equal-row runs are contiguous because matches are row-sorted.
    let s = SearchState {
        matches: vec![
            MatchRange { row: 5, col_start: 0, col_end: 1 },
            MatchRange { row: 5, col_start: 4, col_end: 6 },
            MatchRange { row: 5, col_start: 9, col_end: 11 },
            MatchRange { row: 99, col_start: 0, col_end: 2 },
        ],
        ..SearchState::new()
    };
    // Viewport [5, 6) captures all three row-5 matches, not the row-99 one.
    assert_eq!(s.visible_match_range(5, 1), (0, 3));
}

/// A search must not carry its matches onto a different grid.
///
/// The refresh gate compares `grid.revision()`, a per-grid counter, so two
/// unrelated grids can sit at the same number — and they routinely do, since a
/// fresh grid starts at zero and counts writes. When a searched pane closes
/// and focus lands on a survivor, that collision skips the rescan and the dead
/// pane's highlights are drawn over text they were never computed against.
///
/// The collision is constructed rather than raced: both grids take the same
/// number of writes, so their revisions are equal by construction.
#[test]
fn a_search_pointed_at_a_new_grid_rescans_despite_an_equal_revision() {
    fn grid_with(text: &str) -> Grid {
        let mut grid = Grid::new(20, 1);
        for ch in text.chars() {
            grid.put_char(
                ch,
                sonicterm_grid::grid::Color::Default,
                sonicterm_grid::grid::Color::Default,
                CellFlags::empty(),
            );
        }
        grid
    }

    // The pane being searched, and the survivor that focus lands on. Same
    // width, same write count, so the counters match.
    let searched = grid_with("alpha");
    let survivor = grid_with("bravo");
    assert_eq!(
        searched.revision(),
        survivor.revision(),
        "test setup: the two grids must collide on revision, or this proves nothing"
    );

    let mut s = SearchState::new();
    s.set_query("alpha", &searched);
    assert_eq!(s.matches.len(), 1, "precondition: the query matches in the searched pane");

    // Without being told the grid changed, the equal revision reads as
    // "nothing has changed" and the rescan is skipped.
    assert!(
        !s.maybe_refresh_for_revision(&survivor),
        "test setup: the revision collision must actually suppress the rescan"
    );
    assert_eq!(
        s.matches.len(),
        1,
        "and the stale match survives — this is the defect, shown before the fix acts"
    );

    // What the pane-close path now does.
    s.invalidate_for_new_grid();

    assert!(
        s.maybe_refresh_for_revision(&survivor),
        "invalidation must force the rescan the revision check cannot ask for"
    );
    assert!(
        s.matches.is_empty(),
        "the survivor does not contain the query, so no match may remain highlighted"
    );
}

/// Write `text` at the cursor through the normal `put_char` path.
fn write_text(grid: &mut Grid, text: &str) {
    for ch in text.chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// Build a grid whose rows hold `lines`, separated by CR LF like shell output.
fn grid_with_lines(cols: u16, rows: u16, lines: &[&str]) -> Grid {
    let mut grid = Grid::new(cols, rows);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        write_text(&mut grid, line);
    }
    grid
}

/// A frame identity built from the query, caret, match count, and current index alone.
fn four_field_hash(search: &SearchState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    search.query.hash(&mut hash);
    search.cursor().hash(&mut hash);
    search.matches.len().hash(&mut hash);
    search.current.hash(&mut hash);
    hash.finish()
}

/// A regex toggle that moves the only match must change the digest while count and focus stay equal.
#[test]
fn regex_toggle_moving_the_only_match_changes_the_presentation_hash() {
    let grid = grid_with_lines(8, 1, &["aa+"]);
    let mut search = SearchState::new();
    search.set_query("a+", &grid);
    search.anchor_to_viewport(0);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    let (legacy, digest) = (four_field_hash(&search), search.presentation_hash(0, grid.rows));

    search.toggle_regex(&grid);
    search.anchor_to_viewport(0);

    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 0, col_end: 2 }]);
    assert_eq!(four_field_hash(&search), legacy, "a four-field identity cannot see the move");
    assert_ne!(search.presentation_hash(0, grid.rows), digest);
}

/// A case toggle that moves the only match must change the digest while count and focus stay equal.
#[test]
fn case_toggle_moving_the_only_match_changes_the_presentation_hash() {
    let grid = grid_with_lines(8, 1, &["Aaa"]);
    let mut search = SearchState::new();
    search.set_query("aa", &grid);
    search.anchor_to_viewport(0);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 0, col_end: 2 }]);
    let (legacy, digest) = (four_field_hash(&search), search.presentation_hash(0, grid.rows));

    search.toggle_case_sensitive(&grid);
    search.anchor_to_viewport(0);

    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    assert_eq!(four_field_hash(&search), legacy, "a four-field identity cannot see the move");
    assert_ne!(search.presentation_hash(0, grid.rows), digest);
}

/// Equal counts at different positions are different drawings.
#[test]
fn equal_match_counts_at_different_positions_hash_differently() {
    let a = state_with_matches();
    let mut b = state_with_matches();
    b.matches[1] = MatchRange { row: 20, col_start: 9, col_end: 10 };
    assert_eq!(four_field_hash(&a), four_field_hash(&b));
    assert_ne!(a.presentation_hash(0, 100), b.presentation_hash(0, 100));
}

/// Nothing is cached at refresh: a same-length in-place edit of `matches` changes the digest.
#[test]
fn in_place_same_length_match_edit_after_refresh_changes_the_hash() {
    let grid = grid_with_lines(10, 1, &["ab ab"]);
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    assert_eq!(search.matches.len(), 2);
    let before = search.presentation_hash(0, grid.rows);
    search.matches[1].col_start += 1;
    search.matches[1].col_end += 1;
    assert_ne!(search.presentation_hash(0, grid.rows), before);
}

/// A replacement state hashes by drawn content, not by instance identity or a counter.
#[test]
fn replacement_search_state_hashes_by_content() {
    let grid = grid_with_lines(10, 1, &["ab ab"]);
    let mut first = SearchState::new();
    first.set_query("ab", &grid);
    let mut replacement = SearchState::new();
    replacement.set_query("ab", &grid);
    assert_eq!(first.presentation_hash(0, 1), replacement.presentation_hash(0, 1));
    replacement.set_query("b", &grid);
    assert_ne!(first.presentation_hash(0, 1), replacement.presentation_hash(0, 1));
}

/// Grid edits, screen switches, and history eviction reach the digest through the matches they move.
#[test]
fn grid_screen_and_eviction_changes_move_the_hash() {
    // Grid edit: overwriting the row shifts the only match one column right.
    let mut grid = grid_with_lines(8, 1, &["ab"]);
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    search.anchor_to_viewport(0);
    let before = search.presentation_hash(0, grid.rows);
    grid.goto(0, 0);
    write_text(&mut grid, " ab");
    assert!(search.maybe_refresh_for_revision(&grid));
    search.anchor_to_viewport(0);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    assert_ne!(search.presentation_hash(0, grid.rows), before);

    // Screen switch: the alternate screen shows the query at another column.
    let mut grid = grid_with_lines(8, 1, &["ab"]);
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    search.anchor_to_viewport(0);
    let before = search.presentation_hash(0, grid.rows);
    grid.enter_alt_screen();
    write_text(&mut grid, "  ab");
    assert!(search.maybe_refresh_for_revision(&grid));
    search.anchor_to_viewport(0);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 2, col_end: 4 }]);
    assert_ne!(search.presentation_hash(0, grid.rows), before);

    // Eviction: one history row drops, the count stays two, and a four-field identity collides.
    let mut grid = grid_with_lines(8, 2, &["ab", "xab"]);
    grid.set_scrollback_limit(1);
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    search.anchor_to_viewport(0);
    let (legacy, before) = (four_field_hash(&search), search.presentation_hash(0, 2));
    for _ in 0..2 {
        grid.carriage_return();
        grid.linefeed();
    }
    write_text(&mut grid, "ab");
    assert!(grid.scrollback_evicted() > 0, "test setup: a history row must be evicted");
    assert!(search.maybe_refresh_for_revision(&grid));
    search.anchor_to_viewport(0);
    assert_eq!(search.matches.len(), 2);
    assert_eq!(four_field_hash(&search), legacy);
    assert_ne!(search.presentation_hash(0, 2), before);
}

/// Moving focus between visible matches, or editing the focused range, changes the digest.
#[test]
fn focused_range_changes_move_the_hash() {
    let mut search = state_with_matches();
    search.current = Some(0);
    let first = search.presentation_hash(0, 100);
    search.current = Some(1);
    assert_ne!(search.presentation_hash(0, 100), first);
    // The focused match is offscreen for this viewport, so only its identity can move the digest.
    let offscreen = search.presentation_hash(25, 10);
    search.matches[1].col_end += 1;
    assert_ne!(search.presentation_hash(25, 10), offscreen);
}

/// Unfocused matches outside the viewport stay out of the digest, keeping its cost bounded.
#[test]
fn presentation_hash_ignores_unfocused_matches_outside_the_viewport() {
    let mut search = state_with_matches();
    search.current = Some(1);
    let before = search.presentation_hash(15, 10);
    search.matches[0].col_end += 1;
    search.matches[2].col_start = 0;
    assert_eq!(search.presentation_hash(15, 10), before);
}

/// The digest is stable for an unchanged state and follows a caret-only move.
#[test]
fn presentation_hash_is_stable_and_tracks_the_caret() {
    let grid = grid_with_lines(8, 1, &["ab"]);
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    let before = search.presentation_hash(0, 1);
    assert_eq!(search.presentation_hash(0, 1), before);
    search.apply_text_edit(crate::text_edit::TextEdit::MoveStart, &grid);
    assert_ne!(search.presentation_hash(0, 1), before);
}

/// Literal search matches NFD and NFC spellings of the same full cluster.
#[test]
fn literal_search_matches_nfd_and_nfc_spellings_of_one_cluster() {
    let grid = grid_with_lines(8, 1, &["xe\u{301}y"]);
    for query in ["\u{e9}", "e\u{301}"] {
        assert_eq!(
            find_in_grid(&grid, query, false),
            vec![MatchRange { row: 0, col_start: 1, col_end: 2 }],
            "query {query:?}"
        );
    }
    assert_eq!(find_in_grid(&grid, "xe\u{301}y", true).len(), 1);
}

/// Regex mode matches the exact raw NFD sequence and does not normalize either side.
#[test]
fn regex_search_matches_the_exact_raw_nfd_sequence() {
    let grid = grid_with_lines(8, 1, &["xe\u{301}y"]);
    assert_eq!(
        find_regex_in_grid(&grid, "e\u{301}", true),
        Ok(vec![MatchRange { row: 0, col_start: 1, col_end: 2 }])
    );
    assert_eq!(find_regex_in_grid(&grid, "\u{e9}", true), Ok(Vec::new()));
}

/// A raw combining-mark-only regex match highlights the lead cell that owns the mark.
#[test]
fn regex_mark_only_match_maps_to_its_lead_cell() {
    let grid = grid_with_lines(8, 1, &["xe\u{301}y"]);
    let expected = Ok(vec![MatchRange { row: 0, col_start: 1, col_end: 2 }]);
    assert_eq!(find_regex_in_grid(&grid, "\u{301}", true), expected);
    assert_eq!(find_regex_in_grid(&grid, r"\p{Mn}", true), expected);
}

/// Literal mark-only search follows the NFC boundary: a composed mark and its base are not separately searchable.
#[test]
fn literal_mark_only_search_follows_the_nfc_boundary() {
    let grid = grid_with_lines(8, 1, &["e\u{301} q\u{301}"]);
    let q_cell = vec![MatchRange { row: 0, col_start: 2, col_end: 3 }];
    assert_eq!(find_in_grid(&grid, "\u{301}", true), q_cell, "only q + U+0301 keeps its mark");
    assert!(find_in_grid(&grid, "e", true).is_empty(), "e composed with its mark into U+00E9");
    assert_eq!(find_in_grid(&grid, "q", true), q_cell);
}

/// A wide lead with extras maps literal and regex matches to its full width, before and after ASCII.
#[test]
fn wide_lead_cell_with_extras_maps_matches_to_its_full_width() {
    let grid = grid_with_lines(10, 1, &["x\u{754c}\u{301}y"]);
    let literal = |query: &str| find_in_grid(&grid, query, true);
    assert_eq!(literal("x\u{754c}"), vec![MatchRange { row: 0, col_start: 0, col_end: 3 }]);
    assert_eq!(literal("\u{301}y"), vec![MatchRange { row: 0, col_start: 1, col_end: 4 }]);
    assert_eq!(literal("\u{754c}\u{301}"), vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    // The continuation cell carries no text, so the wide glyph matches exactly once.
    assert_eq!(literal("\u{754c}"), vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    assert_eq!(
        find_regex_in_grid(&grid, "\u{754c}\u{301}", true),
        Ok(vec![MatchRange { row: 0, col_start: 1, col_end: 3 }])
    );
    assert_eq!(
        find_regex_in_grid(&grid, r"\S", true),
        Ok(vec![
            MatchRange { row: 0, col_start: 0, col_end: 1 },
            MatchRange { row: 0, col_start: 1, col_end: 3 },
            MatchRange { row: 0, col_start: 3, col_end: 4 },
        ]),
        "the wide lead and its mark start in one cell and share one range"
    );
}

/// Unicode case expansion keeps every folded scalar owned by its source cell.
#[test]
fn unicode_case_expansion_stays_mapped_to_its_cell() {
    let grid = grid_with_lines(10, 1, &["\u{130}x \u{1e9e}"]);
    assert_eq!(
        find_in_grid(&grid, "\u{130}x", false),
        vec![MatchRange { row: 0, col_start: 0, col_end: 2 }]
    );
    assert_eq!(
        find_in_grid(&grid, "i", false),
        vec![MatchRange { row: 0, col_start: 0, col_end: 1 }]
    );
    assert_eq!(
        find_in_grid(&grid, "\u{df}", false),
        vec![MatchRange { row: 0, col_start: 3, col_end: 4 }]
    );
}

/// Regex classes and escapes keep their scalar meanings: neither pattern nor haystack is normalized.
#[test]
fn regex_classes_and_escapes_keep_their_scalar_meanings() {
    let grid = grid_with_lines(8, 1, &["e\u{301}x"]);
    let lead = Ok(vec![MatchRange { row: 0, col_start: 0, col_end: 1 }]);
    assert_eq!(find_regex_in_grid(&grid, "[\u{e9}]", true), Ok(Vec::new()));
    assert_eq!(find_regex_in_grid(&grid, "[e][\u{301}]", true), lead);
    assert_eq!(find_regex_in_grid(&grid, r"e\x{301}", true), lead);
    assert_eq!(find_regex_in_grid(&grid, "E\u{301}", false), lead);
    assert_eq!(find_regex_in_grid(&grid, "E\u{301}", true), Ok(Vec::new()));
}

/// Search, highlight, and copy agree on a decomposed cluster in a real grid.
#[test]
fn search_highlight_and_copy_agree_on_a_decomposed_cluster() {
    let grid = grid_with_lines(10, 2, &["plain", "cafe\u{301}!"]);
    let mut search = SearchState::new();
    search.set_query("caf\u{e9}", &grid);
    search.anchor_to_viewport(0);
    let found = search.current_match().expect("the NFC query finds the NFD text");
    assert_eq!(found, MatchRange { row: 1, col_start: 0, col_end: 4 });
    assert_eq!(search.visible_match_range(0, grid.rows), (0, 1));
    assert_eq!(search.match_visible_row(&found), Some(1));
    let mut selection = crate::selection::Selection::new(u64::from(found.row), found.col_start);
    selection.extend(u64::from(found.row), found.col_end - 1);
    assert_eq!(selection.as_text(&grid), "cafe\u{301}");
}

/// Literal and regex results over fixed rows keep the prior case, regex, and non-overlap semantics.
#[test]
fn search_results_keep_case_regex_and_non_overlap_semantics() {
    let cases: [(&str, &str, SearchMode, bool, &[(u16, u16)]); 11] = [
        ("aaaa", "aa", SearchMode::Substring, true, &[(0, 2), (2, 4)]),
        ("aabaabaaab", "aab", SearchMode::Substring, true, &[(0, 3), (3, 6), (7, 10)]),
        ("abababx", "abab", SearchMode::Substring, true, &[(0, 4)]),
        ("Hello hello", "hello", SearchMode::Substring, false, &[(0, 5), (6, 11)]),
        ("Hello hello", "hello", SearchMode::Substring, true, &[(6, 11)]),
        ("x\u{754c}y", "\u{754c}y", SearchMode::Substring, true, &[(1, 4)]),
        ("ab12cd345", r"\d+", SearchMode::Regex, true, &[(2, 4), (6, 9)]),
        ("Foo foo FOO", "foo", SearchMode::Regex, false, &[(0, 3), (4, 7), (8, 11)]),
        ("Foo foo FOO", "FOO", SearchMode::Regex, true, &[(8, 11)]),
        ("aaa", "a*", SearchMode::Regex, true, &[(0, 3)]),
        ("a.c abc", "a.c", SearchMode::Substring, true, &[(0, 3)]),
    ];
    for (row, query, mode, case_sensitive, expected) in cases {
        let grid = grid_with_lines(16, 1, &[row]);
        let mut search = SearchState { mode, case_sensitive, ..SearchState::new() };
        search.set_query(query, &grid);
        let spans: Vec<(u16, u16)> =
            search.matches.iter().map(|m| (m.col_start, m.col_end)).collect();
        assert_eq!(spans, expected, "{row:?} / {query:?} / {mode:?} / {case_sensitive}");
        let direct = match mode {
            SearchMode::Substring => find_in_grid(&grid, query, case_sensitive),
            SearchMode::Regex => find_regex_in_grid(&grid, query, case_sensitive).unwrap(),
        };
        assert_eq!(direct, search.matches);
    }
}

/// Owner-map steps of a literal scan that, after each match on a fully matching row, finds the
/// next cell by walking the owner map from index zero: quadratic in row width.
fn owner_restart_steps(width: u64) -> u64 {
    (0..width.saturating_sub(1)).map(|cell| cell + 2).sum()
}

/// Literal comparisons grow linearly as a fully matching row doubles in width.
#[test]
fn literal_comparisons_grow_linearly_with_row_width() {
    let comparisons = |width: u16| {
        let line = "a".repeat(usize::from(width) - 1);
        let grid = grid_with_lines(width, 1, &[line.as_str()]);
        let mut search = SearchState::new();
        search.set_query("a", &grid);
        assert_eq!(search.matches.len(), usize::from(width) - 1);
        search.work().comparisons
    };
    let (narrow, wide) = (comparisons(1024), comparisons(2048));
    assert_eq!(narrow, 1024, "one comparison per scalar, including the trailing blank");
    assert_eq!(wide, 2 * narrow);
    // An owner-map restart from index zero fails the same doubling bound by a wide margin.
    let (restart_narrow, restart_wide) = (owner_restart_steps(1024), owner_restart_steps(2048));
    assert!(restart_wide > 3 * restart_narrow, "an owner-map restart from zero is quadratic");
}

/// Row buffers are reused across rows, so growth follows row width rather than cell count.
#[test]
fn row_buffers_are_reused_across_rows() {
    let line = "needle ".repeat(9);
    let lines = vec![line.as_str(); 40];
    let grid = grid_with_lines(80, 40, &lines);
    let mut search = SearchState::new();
    search.set_query("needle", &grid);
    let work = search.work();
    assert_eq!(work.rows_scanned, 40);
    assert_eq!(search.matches.len(), 40 * 9);
    assert_eq!(work.scratch_growths, 1, "equal-width rows reuse the first row's buffers");
}

/// A multi-character key event edits the query and rescans history once, not once per character.
#[test]
fn multi_character_key_text_rescans_once() {
    let grid = grid_with_lines(20, 2, &["abc", "xabcx"]);
    let mut search = SearchState::new();
    search.input_key_text("a\nbc", &grid);
    assert_eq!(search.query, "abc");
    assert_eq!(search.cursor(), 3);
    assert_eq!(search.matches.len(), 2);
    assert_eq!(search.work().full_scans, 1);
    search.input_key_text("\r\n", &grid);
    assert_eq!(search.work().full_scans, 1, "line breaks alone change nothing");

    // Per-character input rescans once per character.
    let mut per_char = SearchState::new();
    for ch in "abc".chars() {
        per_char.input_char(ch, &grid);
    }
    assert_eq!(per_char.matches, search.matches);
    assert_eq!(per_char.work().full_scans, 3);
}

/// Streaming refresh stays equal to a full rescan through edits, scrolls, eviction, and clears.
#[test]
fn incremental_refresh_matches_a_full_rescan_while_streaming() {
    let steps: [(&str, fn(&mut Grid)); 10] = [
        ("new line", |g| {
            g.carriage_return();
            g.linefeed();
            write_text(g, "zab");
        }),
        ("cursor only", |g| g.goto(0, 3)),
        ("overwrite", |g| {
            g.goto(1, 0);
            write_text(g, "ab ab");
        }),
        ("erase line", |g| {
            g.goto(2, 0);
            g.erase_line();
        }),
        ("scroll and evict", |g| {
            for _ in 0..9 {
                g.goto(3, 0);
                g.carriage_return();
                g.linefeed();
                write_text(g, "ab");
            }
        }),
        ("scroll down", |g| g.scroll_down(1)),
        ("region scroll", |g| g.scroll_region_up(1, 2, 1)),
        ("insert cells", |g| g.insert_cells(3, 0, 2)),
        ("delete cells", |g| g.delete_cells(3, 0, 1)),
        ("clear history", |g| g.clear_scrollback()),
    ];
    for mode in [SearchMode::Substring, SearchMode::Regex] {
        let mut grid = grid_with_lines(12, 4, &["ab", "xxab"]);
        grid.set_scrollback_limit(6);
        let mut search = SearchState { mode, ..SearchState::new() };
        search.set_query("ab", &grid);
        let full_scans = search.work().full_scans;
        for (name, step) in steps {
            step(&mut grid);
            search.maybe_refresh_for_revision(&grid);
            let expected = match mode {
                SearchMode::Substring => find_in_grid(&grid, "ab", false),
                SearchMode::Regex => find_regex_in_grid(&grid, "ab", false).unwrap(),
            };
            assert_eq!(search.matches, expected, "{mode:?} after {name}");
        }
        assert!(grid.scrollback_evicted() > 0, "test setup: history must be evicted");
        assert_eq!(search.work().full_scans, full_scans, "streaming never rescans all history");
    }
}

/// Cursor-only revisions rescan no row and keep focus; pane and screen changes rescan everything.
#[test]
fn cursor_only_revisions_rescan_nothing_and_identity_changes_rescan_all() {
    let mut grid = grid_with_lines(10, 4, &["ab", "xab"]);
    let mut search = SearchState::new();
    search.bind_pane(1);
    search.set_query("ab", &grid);
    search.anchor_to_viewport(0);
    search.next();
    let focused = search.current_match();
    let (rows, full_scans) = (search.work().rows_scanned, search.work().full_scans);
    let revision = grid.revision();
    grid.goto(3, 5);
    assert_ne!(grid.revision(), revision, "test setup: cursor motion must bump the revision");
    assert!(search.maybe_refresh_for_revision(&grid));
    assert_eq!(search.work().rows_scanned, rows, "no row content changed");
    assert_eq!(search.current_match(), focused);

    search.bind_pane(2);
    assert!(search.maybe_refresh_for_revision(&grid));
    assert_eq!(search.work().full_scans, full_scans + 1);
    assert_eq!(search.current, None);

    grid.enter_alt_screen();
    write_text(&mut grid, " ab");
    assert!(search.maybe_refresh_for_revision(&grid));
    assert_eq!(search.work().full_scans, full_scans + 2);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
}

/// Alternate-screen evictions trim only the saved primary's history and never rebase matches.
#[test]
fn alternate_screen_eviction_of_primary_history_keeps_its_matches() {
    let mut grid = grid_with_lines(10, 3, &["1", "2", "3", "4", "5"]);
    assert_eq!(grid.scrollback_len(), 2, "test setup: the primary screen keeps history");
    grid.enter_alt_screen();
    write_text(&mut grid, " ab");
    let mut search = SearchState::new();
    search.set_query("ab", &grid);
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
    let evicted = grid.scrollback_evicted();
    grid.set_scrollback_limit(0);
    assert!(grid.scrollback_evicted() > evicted, "test setup: the saved primary loses history");
    assert!(search.maybe_refresh_for_revision(&grid));
    assert_eq!(search.matches, vec![MatchRange { row: 0, col_start: 1, col_end: 3 }]);
}

/// The matcher compiles once per query, mode, or case change and is reused by rescans.
#[test]
fn matcher_compiles_once_per_query_mode_or_case() {
    let mut grid = grid_with_lines(12, 2, &["ab"]);
    let mut search = SearchState { mode: SearchMode::Regex, ..SearchState::new() };
    search.set_query("a.", &grid);
    assert_eq!(search.work().matcher_builds, 1);
    for _ in 0..3 {
        write_text(&mut grid, " ab");
        assert!(search.maybe_refresh_for_revision(&grid));
    }
    search.invalidate_for_new_grid();
    assert!(search.maybe_refresh_for_revision(&grid));
    assert_eq!(search.work().matcher_builds, 1, "rescans with unchanged settings reuse it");
    search.toggle_case_sensitive(&grid);
    search.toggle_regex(&grid);
    assert_eq!(search.work().matcher_builds, 3);
    search.set_query("a.", &grid);
    assert_eq!(search.work().matcher_builds, 3, "an unchanged query reuses the matcher");
}

/// Measurement harness, not a gate: prints per-refresh cost while output streams with search
/// open. It uses only the query and revision-refresh APIs, so the same body can measure
/// another build on the same host for comparison.
#[test]
#[ignore = "measurement: run with --ignored --nocapture in release mode"]
fn measure_streaming_refresh_cost() {
    const COLS: u16 = 200;
    const FRAMES: u32 = 50;
    for requested in [1_000_usize, usize::MAX] {
        let mut grid = Grid::new(COLS, 50);
        grid.set_scrollback_limit(requested);
        let line = "a".repeat(usize::from(COLS) - 1);
        for _ in 0..6_000 {
            grid.carriage_return();
            grid.linefeed();
            write_text(&mut grid, &line);
        }
        let mut search = SearchState::new();
        search.set_query("a", &grid);
        let mut total = std::time::Duration::ZERO;
        for _ in 0..FRAMES {
            grid.carriage_return();
            grid.linefeed();
            write_text(&mut grid, &line);
            let started = std::time::Instant::now();
            search.maybe_refresh_for_revision(&grid);
            total += started.elapsed();
        }
        println!(
            "requested={requested} retained_rows={} matches={} mean_refresh_us={:.1}",
            grid.scrollback_len() + usize::from(grid.rows),
            search.matches.len(),
            total.as_secs_f64() * 1e6 / f64::from(FRAMES),
        );
    }
}
