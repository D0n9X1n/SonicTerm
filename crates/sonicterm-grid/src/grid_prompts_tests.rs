//! Pins the OSC 133 region records an agent's `run_command` relies on: B and C positions, the
//! growing `seq`, drain rebasing that keeps an unfinished region, the pinned run that survives
//! eviction and the alternate screen, and stable row tokens.

use super::*;

/// A grid with no scrollback limit pressure, `cols` x `rows`.
fn grid(cols: u16, rows: u16) -> Grid {
    Grid::new(cols, rows)
}

/// Open a region at the cursor, then move to `col` and record B there.
fn prompt_at(grid: &mut Grid, col: u16) {
    grid.record_prompt_start();
    grid.goto(grid.cursor.row, col);
    grid.record_input_start();
}

/// A, B, C, D;3 records the input start, the output row and exit code 3, and the region is the
/// last completed command; a region with D but no C is not one.
#[test]
fn a_full_marker_sequence_records_every_position() {
    let mut grid = grid(20, 5);
    prompt_at(&mut grid, 2);
    grid.goto(1, 0);
    grid.record_output_start();
    grid.goto(3, 0);
    grid.record_prompt_end(Some(3));
    let region = *grid.newest_prompt().expect("region");
    assert_eq!(region.input_start, Some((0, 2)));
    assert!(region.input_start_at.is_some() && region.output_start_at.is_some());
    assert_eq!((region.output_start_row, region.end_row), (Some(1), Some(3)));
    let span = grid.last_completed_command().expect("completed");
    assert_eq!((span.seq, span.exit_code, span.output_truncated), (region.seq, Some(3), false));

    let mut without_c = Grid::new(20, 5);
    without_c.record_prompt_start();
    without_c.record_prompt_end(Some(0));
    assert_eq!(without_c.last_completed_command(), None);
}

/// A repeated B or C in one region keeps the first position.
#[test]
fn duplicate_b_and_c_keep_the_first_position() {
    let mut grid = grid(20, 5);
    prompt_at(&mut grid, 2);
    grid.goto(0, 9);
    grid.record_input_start();
    grid.goto(1, 0);
    grid.record_output_start();
    grid.goto(2, 0);
    grid.record_output_start();
    let region = grid.newest_prompt().copied().expect("region");
    assert_eq!((region.input_start, region.output_start_row), (Some((0, 2)), Some(1)));
}

/// `seq` keeps growing past the 256-region FIFO limit, so the newest region's identity is
/// never reused.
#[test]
fn seq_grows_across_fifo_eviction() {
    let mut grid = grid(10, 3);
    for row in 0..(PROMPT_REGION_LIMIT as u16 + 10) {
        grid.goto(row % 3, 0);
        grid.record_prompt_start();
        grid.record_prompt_end(None);
    }
    assert_eq!(grid.prompts_len(), PROMPT_REGION_LIMIT);
    assert_eq!(grid.newest_prompt().map(|region| region.seq), Some(PROMPT_REGION_LIMIT as u64 + 9));
}

/// Draining history rebases every coordinate; a region whose command is running (C, no D) and
/// whose start left history is kept with `start_row = None` and skipped by prompt jumping and
/// the gutter caret; once finished, or without C, such a region is dropped.
#[test]
fn a_drained_unfinished_region_is_kept_and_skipped_by_prompt_jumping() {
    let mut grid = grid(10, 2);
    grid.set_scrollback_limit(1);
    prompt_at(&mut grid, 1);
    grid.record_output_start();
    for _ in 0..3 {
        grid.scroll_up(1);
    }
    let region = grid.newest_prompt().copied().expect("kept while unfinished");
    assert_eq!((region.start_row, region.input_start, region.output_start_row), (None, None, None));
    assert!(region.output_seen, "C stays seen after its row left history");
    assert_eq!(grid.prompt_before(u64::MAX), None);
    assert_eq!(grid.prompt_after(0), None);
    assert_eq!(grid.prompt_visible_row(&region), None);

    grid.record_prompt_end(Some(0));
    let span = grid.last_completed_command().expect("completed");
    assert!(span.output_truncated, "the output's first row is gone");
    grid.scroll_up(1);
    assert_eq!(grid.prompts_len(), 0, "a finished region without a start is dropped");
}

/// A pinned run survives draining its region's rows, reports C and D, and its tokens still
/// resolve while the rows are in history, so the C-to-D span stays exact.
#[test]
fn a_pinned_run_survives_drains_and_records_its_span() {
    let mut grid = grid(10, 3);
    grid.set_scrollback_limit(400);
    prompt_at(&mut grid, 2);
    let seq = grid.newest_prompt().expect("region").seq;
    assert!(grid.pin_run(seq));
    assert!(!grid.pin_run(seq), "only one run is pinned at a time");
    grid.goto(1, 0);
    grid.record_output_start();
    for _ in 0..300 {
        grid.scroll_up(1);
    }
    grid.record_prompt_end(Some(4));
    let run = grid.pinned_run().expect("pinned");
    assert!(run.output_seen && !run.evicted);
    assert_eq!(run.exit, Some(Some(4)));
    let start = grid.resolve_row(run.output_start.expect("C token")).expect("still in history");
    let end = grid.resolve_row(run.end.expect("D token")).expect("live");
    assert_eq!(end - start, 300, "the C-to-D span is exact after 300 scrolls");
    assert_eq!(grid.unpin_run(), Some(run));
    assert_eq!(grid.pinned_run(), None);
}

/// A newer prompt on the pinned screen before D marks the run evicted, so a waiter reports it
/// lost instead of attributing a later command's completion to it.
#[test]
fn a_newer_prompt_before_d_evicts_the_pinned_run() {
    let mut grid = grid(10, 3);
    prompt_at(&mut grid, 2);
    let seq = grid.newest_prompt().expect("region").seq;
    assert!(grid.pin_run(seq));
    grid.goto(1, 0);
    grid.record_prompt_start();
    grid.record_output_start();
    grid.record_prompt_end(Some(0));
    let run = grid.pinned_run().expect("pinned");
    assert!(run.evicted && run.exit.is_none() && !run.output_seen);
}

/// Pin, C, enter and leave the alternate screen, D: the run completes and `seq` keeps growing
/// across the swap; marks printed inside the alternate screen do not touch it.
#[test]
fn a_pinned_run_completes_across_an_alternate_screen_round_trip() {
    let mut grid = grid(10, 4);
    prompt_at(&mut grid, 2);
    let seq = grid.newest_prompt().expect("region").seq;
    assert!(grid.pin_run(seq));
    grid.goto(1, 0);
    grid.record_output_start();
    grid.enter_alt_screen();
    grid.record_prompt_start();
    grid.record_output_start();
    grid.record_prompt_end(Some(9));
    assert!(grid.newest_prompt().expect("alt region").seq > seq, "seq grows across the swap");
    let inside = grid.pinned_run().expect("pinned");
    assert!(inside.exit.is_none() && !inside.evicted, "alternate-screen marks do not complete it");
    assert!(grid.last_completed_command().is_none(), "the primary region has no D yet");
    grid.leave_alt_screen();
    grid.goto(2, 0);
    grid.record_prompt_end(Some(0));
    assert_eq!(grid.pinned_run().expect("pinned").exit, Some(Some(0)));
}

/// The primary screen's last command is found while the alternate screen is active.
#[test]
fn the_last_command_is_read_from_the_primary_screen_while_alternate() {
    let mut grid = grid(10, 4);
    prompt_at(&mut grid, 2);
    grid.record_output_start();
    grid.record_prompt_end(Some(6));
    grid.enter_alt_screen();
    assert_eq!(grid.last_completed_command().map(|span| span.exit_code), Some(Some(6)));
}

/// Row tokens keep growing at the scrollback limit and with scrollback disabled, resolve to
/// `None` once the row left history (including after ED 3) and on the other screen, and a
/// primary token resolves again after an alternate-screen round trip.
#[test]
fn row_tokens_stay_stable_across_eviction_clears_and_screens() {
    for limit in [2, 0] {
        let mut grid = grid(10, 3);
        grid.set_scrollback_limit(limit);
        let first = grid.row_token(grid.scrollback_len() as u64 + 2);
        grid.scroll_up(1);
        let second = grid.row_token(grid.scrollback_len() as u64 + 2);
        assert!(second.line > first.line, "limit {limit}: new rows get new lines");
        for _ in 0..10 {
            grid.scroll_up(1);
        }
        assert_eq!(grid.resolve_row(first), None, "limit {limit}: an evicted row is gone");
        let live = grid.row_token(grid.scrollback_len() as u64);
        assert_eq!(grid.resolve_row(live), Some(grid.scrollback_len() as u64));
    }

    let mut grid = grid(10, 3);
    grid.set_scrollback_limit(10);
    grid.scroll_up(2);
    let in_history = grid.row_token(0);
    let live = grid.row_token(grid.scrollback_len() as u64 + 1);
    grid.clear_scrollback();
    assert_eq!(grid.resolve_row(in_history), None, "ED 3 drops the history row");
    assert_eq!(grid.resolve_row(live), Some(1), "the live row keeps its token");
    grid.enter_alt_screen();
    assert_eq!(grid.resolve_row(live), None, "a primary token does not resolve on alt");
    let alt = grid.row_token(0);
    grid.leave_alt_screen();
    assert_eq!(grid.resolve_row(alt), None, "an alternate token does not resolve on primary");
    assert_eq!(grid.resolve_row(live), Some(1), "the primary token resolves again");
}

/// A pinned region drained before its C is kept, so the C and D that follow still complete the
/// run instead of leaving it pending forever.
#[test]
fn a_pinned_region_drained_before_c_still_completes() {
    let mut grid = grid(10, 2);
    grid.set_scrollback_limit(1);
    prompt_at(&mut grid, 2);
    let seq = grid.newest_prompt().expect("region").seq;
    assert!(grid.pin_run(seq));
    for _ in 0..4 {
        grid.scroll_up(1);
    }
    assert_eq!(grid.prompts_len(), 1, "the pinned region survives the drain");
    grid.record_output_start();
    grid.record_prompt_end(Some(0));
    let run = grid.pinned_run().expect("pinned");
    assert_eq!((run.output_seen, run.exit, run.evicted), (true, Some(Some(0)), false));
}

/// Once C was seen, an A on the same row is a new prompt: it evicts the pinned run instead of
/// being coalesced, so a later command's D cannot complete the earlier run.
#[test]
fn an_a_on_the_same_row_after_c_is_a_new_prompt() {
    let mut grid = grid(20, 4);
    prompt_at(&mut grid, 2);
    let seq = grid.newest_prompt().expect("region").seq;
    assert!(grid.pin_run(seq));
    grid.record_output_start();
    prompt_at(&mut grid, 2);
    grid.record_output_start();
    grid.record_prompt_end(Some(9));
    let run = grid.pinned_run().expect("pinned");
    assert!(run.evicted, "the new prompt superseded the run");
    assert_eq!(run.exit, None, "the later D did not complete it");
    assert_eq!(grid.newest_prompt().map(|region| region.seq), Some(seq + 1));
}
