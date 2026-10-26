//! Literal and regex matching preserve Unicode clusters and linear scan work.

use super::super::search_tests::*;
use super::*;

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
            search.matches.iter().map(|hit| (hit.col_start, hit.col_end)).collect();
        assert_eq!(spans, expected, "{row:?} / {query:?} / {mode:?} / {case_sensitive}");
        let direct = match mode {
            SearchMode::Substring => find_in_grid(&grid, query, case_sensitive),
            SearchMode::Regex => find_regex_in_grid(&grid, query, case_sensitive).unwrap(),
        };
        assert_eq!(direct, search.matches);
    }
}

/// Literal scan work, comparisons plus cell lookups, grows linearly as a fully matching row
/// doubles in width, so no per-match lookup walks the row.
#[test]
fn literal_scan_work_grows_linearly_with_row_width() {
    let scan_work = |width: u16| {
        let line = "a".repeat(usize::from(width) - 1);
        let grid = grid_with_lines(width, 1, &[line.as_str()]);
        let mut search = SearchState::new();
        search.set_query("a", &grid);
        assert_eq!(search.matches.len(), usize::from(width) - 1);
        let counted = search.work();
        (counted.comparisons, counted.lookup_probes)
    };
    let (narrow, wide) = (scan_work(1024), scan_work(2048));
    assert_eq!(narrow, (1024, 3 * 1023), "one comparison per scalar, three lookups per match");
    let (narrow_total, wide_total) = (narrow.0 + narrow.1, wide.0 + wide.1);
    assert!(
        wide_total <= 2 * narrow_total + 8,
        "scan work must stay linear: {narrow_total} -> {wide_total}"
    );
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
