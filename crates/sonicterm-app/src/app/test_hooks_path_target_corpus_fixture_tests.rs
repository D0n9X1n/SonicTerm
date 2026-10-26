//! Reference hard-wrap parser and deterministic equivalence corpus. regression contracts.

use super::*;

/// The shared-view pre-pass decides every cell exactly as the per-wrapper pre-pass did: the same
/// refusal (`Incomplete`), the same ownership and the same spans, over primary and alternate
/// screens, live and scrolled-back frames, split borders, hyperlinks and wide cells. The corpus
/// must reach both `Complete` and `Incomplete`, or the agreement would prove little.
#[test]
fn hardwrap_pre_pass_matches_the_per_wrapper_reference() {
    let mut rng = CorpusRng(0x5eed_1649_cafe_f00d);
    let (mut complete_count, mut incomplete_count) = (0usize, 0usize);
    for grid_index in 0..160 {
        let cols = [20u16, 24, 30][rng.below(3)];
        let rows = 8u16;
        let mut lines = Vec::new();
        while lines.len() < 12 {
            // When: the coin picks a chain, emit a hard-wrapped URL; otherwise one prose row.
            if rng.below(2) == 0 {
                lines.extend(corpus_chain_lines(&mut rng, cols));
            } else {
                lines.push(corpus_prose_line(&mut rng, cols));
            }
        }
        let alt_screen = grid_index % 3 == 0;
        let output = if alt_screen {
            let mut placed = String::from("\x1b[?1049h");
            for (row, line) in lines.iter().take(usize::from(rows)).enumerate() {
                placed.push_str(&format!("\x1b[{};1H{line}", row + 1));
            }
            if grid_index % 2 == 0 {
                placed.push_str(&pane_border(cols / 2, rows));
            }
            placed
        } else {
            lines.join("\r\n")
        };
        let mut parser = sonicterm_vt::vt::Parser::new(Grid::new(cols, rows));
        parser.advance(output.as_bytes());
        let grid = parser.grid();
        let live_top = grid.scrollback_evicted() + grid.scrollback_len() as u64;
        let scrolled_top = live_top.saturating_sub(1 + rng.below(3) as u64);
        for view_top in [live_top, scrolled_top] {
            for row in view_top..view_top + u64::from(rows) {
                for col in 0..cols {
                    let pointed = AbsoluteCell { row, col };
                    let found = hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed);
                    let expected =
                        reference_hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed);
                    assert_eq!(
                        found, expected,
                        "grid {grid_index}, view_top {view_top}, cell {row}:{col}, output {output:?}"
                    );
                    match expected {
                        HardwrapUri::Complete(_) => complete_count += 1,
                        HardwrapUri::Incomplete => incomplete_count += 1,
                        HardwrapUri::NotApplicable => {}
                    }
                }
            }
        }
    }
    assert!(complete_count > 50, "the corpus reached Complete only {complete_count} times");
    assert!(incomplete_count > 50, "the corpus reached Incomplete only {incomplete_count} times");
}

/// A row with two `(https://` openers, the earlier one still followed by a chain of URL text, is
/// decided as the reference decides it: only the opener at the row's last break can start a chain,
/// and the earlier opener's body holds that later wrapper. The chain must resolve, or the case
/// would not exercise a completed claim beside the shadowed opener.
#[test]
fn hardwrap_pre_pass_matches_the_reference_with_two_openers_on_one_row() {
    let cols = 40u16;
    let width = usize::from(cols);
    let opener_text = "x (https://a.co/y (https://ex.co/";
    let first = format!("{opener_text}{}", "a".repeat(width - opener_text.len()));
    let middle = format!("  {}", "b".repeat(width - 2));
    let lines = vec![first.clone(), middle.clone(), "  cccc) done".to_string()];
    let parser = hardwrap_grid(cols, &lines);
    let (complete_count, _) = assert_hardwrap_cells_match_reference(parser.grid(), "two openers");
    assert!(complete_count > 0, "the later opener's chain resolves");
    let pointed = AbsoluteCell { row: 1, col: 4 };
    let HardwrapUri::Complete(candidate) = hardwrap_uri_at_cell(parser.grid(), 0, pointed) else {
        panic!("the continuation belongs to the later opener's chain");
    };
    let expected_url = format!("https://ex.co/{}{}cccc", &first[opener_text.len()..], &middle[2..]);
    assert_eq!(candidate.target, DetectedTarget::Uri(expected_url));
    // An unterminated variant: the last row holds no closer, so the chain cannot be proven.
    let lines = vec![first, middle, "  cccc done".to_string()];
    let parser = hardwrap_grid(cols, &lines);
    let (_, incomplete_count) =
        assert_hardwrap_cells_match_reference(parser.grid(), "two openers, unclosed");
    assert!(incomplete_count > 0, "an unterminated chain refuses its truncated prefix");
}

/// Wrapper text a row holds past `cols`, as a resize without reflow leaves it, is decided as the
/// reference decides it: the edge bounds the body and its breaks, while closers past the edge stay
/// visible to the closer search, whether the opener sits inside the grid or past its edge.
#[test]
fn hardwrap_pre_pass_matches_the_reference_on_rows_longer_than_the_grid() {
    let cols = 24u16;
    let continuation =
        [format!("  {}", "b".repeat(usize::from(cols) - 2)), "  cccc) done".to_string()];
    // The opener inside the grid, its body running past the edge.
    let mut lines = vec!["x (https://ex.co/aaaaaaa".to_string()];
    lines.extend(continuation.iter().cloned());
    let mut parser = hardwrap_grid(cols, &lines);
    // The appended cells hold a blank, a break no body may contain, but past the edge it is no body.
    extend_row_past_grid(parser.grid_mut(), 0, "zz tail");
    let (complete_count, _) =
        assert_hardwrap_cells_match_reference(parser.grid(), "body past the edge");
    assert!(complete_count > 0, "the chain still resolves from the cells inside the grid");
    let pointed = AbsoluteCell { row: 1, col: 4 };
    let HardwrapUri::Complete(candidate) = hardwrap_uri_at_cell(parser.grid(), 0, pointed) else {
        panic!("the continuation belongs to the opener's chain");
    };
    let expected_url = format!("https://ex.co/aaaaaaa{}cccc", &continuation[0][2..]);
    assert_eq!(
        candidate.target,
        DetectedTarget::Uri(expected_url),
        "cells past the edge join nothing"
    );
    // The opener inside the grid with its own closer past the edge.
    let mut parser = hardwrap_grid(cols, &lines);
    extend_row_past_grid(parser.grid_mut(), 0, "aa) tail");
    let counts = assert_hardwrap_cells_match_reference(parser.grid(), "closer past the edge");
    assert_eq!(
        counts,
        (0, 0),
        "a same-row closer, even past the edge, leaves single-row detection in charge"
    );
    // The whole two-opener wrapper text sitting past the edge, after prose that fills the grid.
    let mut lines = vec!["see the docs at the link".to_string()];
    lines.extend(continuation.iter().cloned());
    let mut parser = hardwrap_grid(cols, &lines);
    extend_row_past_grid(parser.grid_mut(), 0, " x (https://a.co/y (https://ex.co/aaaa");
    assert!(parser.grid().row(0).len() > usize::from(cols), "the row is longer than the grid");
    let (complete_count, _) =
        assert_hardwrap_cells_match_reference(parser.grid(), "wrapper past the edge");
    assert_eq!(complete_count, 0, "no chain opens beyond the grid's edge");
}

/// An opener in the grid's last column is decided as the reference decides it: its scheme cannot
/// fit inside the row, and on a row longer than the grid its scheme lies past the edge, so neither
/// opens a chain; a `[` opener in that column behaves the same.
#[test]
fn hardwrap_pre_pass_matches_the_reference_with_an_opener_in_the_last_column() {
    let cols = 20u16;
    let mut first = "a".repeat(usize::from(cols) - 1);
    first.push('(');
    let lines = vec![first, "  https://ex.co/b".to_string(), "  cccc) done".to_string()];
    let parser = hardwrap_grid(cols, &lines);
    let counts = assert_hardwrap_cells_match_reference(parser.grid(), "opener at cols - 1");
    assert_eq!(counts, (0, 0), "an opener with no room for a scheme opens nothing");
    let mut parser = hardwrap_grid(cols, &lines);
    extend_row_past_grid(parser.grid_mut(), 0, "https://ex.co/aaa");
    let counts = assert_hardwrap_cells_match_reference(
        parser.grid(),
        "opener at cols - 1, scheme past the edge",
    );
    assert_eq!(counts, (0, 0), "a scheme past the edge leaves the opener no body inside the grid");
    let mut bracket = "a".repeat(usize::from(cols) - 1);
    bracket.push('[');
    let middle = format!("  {}", "b".repeat(usize::from(cols) - 2));
    let lines = vec![bracket, middle, "  cccc] done".to_string()];
    let parser = hardwrap_grid(cols, &lines);
    let counts = assert_hardwrap_cells_match_reference(parser.grid(), "bracket at cols - 1");
    assert_eq!(counts, (0, 0), "a bracket with no room for a scheme opens nothing");
}

/// A row of repeated complete `(https://a)` wrappers costs the pre-pass work linear in its width:
/// each wrapper's same-row closer is not found by re-collecting the row per wrapper.
#[test]
fn repeated_complete_wrappers_cost_linear_work() {
    assert_hardwrap_work_is_linear("(https://a)");
}

/// A row of repeated unmatched `(https://` openers costs work linear in its width: no opener
/// searches the growing suffix after it for a closer that never comes.
#[test]
fn repeated_unmatched_openers_cost_linear_work() {
    assert_hardwrap_work_is_linear("(https://a/b ");
}
