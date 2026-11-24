//! Pins the per-screen semantic prompt state the OSC 133 annotations drive: its anchor follows the
//! row through history and is dropped, with a new generation, when that row is evicted; each screen
//! keeps its own state; RIS resets it; and the command text it holds is bounded and never printed.

use super::*;

/// Mark an initial prompt with default options at the cursor.
fn prompt(grid: &mut Grid) {
    grid.mark_semantic_prompt(PromptKind::Initial, Some(PromptOptions::default()));
}

/// Scroll `count` full-screen lines from the bottom row.
fn scroll(grid: &mut Grid, count: u16) {
    for _ in 0..count {
        grid.scroll_up(1);
    }
}

#[test]
fn anchor_eviction_bumps_generation() {
    // An anchor follows its row into history; once that row leaves history the anchor is gone and
    // the generation moves, so a reader holding the old one knows the prompt it named is gone.
    let mut grid = Grid::new(10, 3);
    grid.set_scrollback_limit(2);
    prompt(&mut grid);
    let generation = grid.semantic().generation;
    assert_eq!(grid.semantic().anchor, Some(0));
    scroll(&mut grid, 2);
    assert_eq!(grid.semantic().anchor, Some(0), "the row is still in history");
    assert_eq!(grid.semantic().generation, generation);
    scroll(&mut grid, 1);
    assert_eq!(grid.semantic().anchor, None, "the row left history");
    assert_eq!(grid.semantic().generation, generation + 1);
}

#[test]
fn anchor_is_evicted_by_a_scroll_without_history() {
    // With scrollback disabled a full-screen scroll recycles the top row: the anchor moves up with
    // its row and is evicted when that row is recycled.
    let mut grid = Grid::new(10, 3);
    grid.set_scrollback_limit(0);
    grid.goto(1, 0);
    prompt(&mut grid);
    let generation = grid.semantic().generation;
    scroll(&mut grid, 1);
    assert_eq!(grid.semantic().anchor, Some(0), "the anchored row moved to the top");
    scroll(&mut grid, 1);
    assert_eq!(grid.semantic().anchor, None);
    assert_eq!(grid.semantic().generation, generation + 1);
}

#[test]
fn clearing_history_evicts_the_anchor() {
    // ED 3 drops the history the anchor sits in, which is an eviction like any other.
    let mut grid = Grid::new(10, 3);
    prompt(&mut grid);
    scroll(&mut grid, 3);
    let generation = grid.semantic().generation;
    grid.clear_scrollback();
    assert_eq!(grid.semantic().anchor, None);
    assert_eq!(grid.semantic().generation, generation + 1);
}

#[test]
fn alt_screen_state_independent() {
    // Each screen has its own semantic state: a prompt on the alternate screen leaves the
    // primary's untouched, and leaving restores the primary's exactly.
    let mut grid = Grid::new(10, 4);
    grid.goto(1, 0);
    prompt(&mut grid);
    grid.mark_semantic_input(false);
    let primary = grid.semantic().clone();
    grid.enter_alt_screen();
    assert_eq!(grid.semantic().anchor, None, "the alternate screen starts with no prompt");
    assert_eq!(grid.semantic().mode, SemanticMode::Output);
    assert_eq!(grid.primary_screen().semantic(), &primary);
    grid.goto(2, 0);
    prompt(&mut grid);
    let alt_generation = grid.semantic().generation;
    grid.leave_alt_screen();
    assert_eq!(grid.semantic(), &primary);
    grid.enter_alt_screen();
    assert!(
        grid.semantic().generation > alt_generation,
        "a later alternate screen never reuses an earlier one's generation"
    );
}

#[test]
fn reset_clears_state_and_bumps_generation() {
    // RIS returns the screen to output with no anchor, options or command text, in a new
    // generation.
    let mut grid = Grid::new(10, 3);
    prompt(&mut grid);
    grid.set_semantic_cmdline(CmdlineText::new("make".to_owned()));
    let generation = grid.semantic().generation;
    grid.reset_semantic();
    let state = grid.semantic();
    assert_eq!(
        (state.mode, state.anchor, state.cmdline.is_none()),
        (SemanticMode::Output, None, true)
    );
    assert_eq!(state.generation, generation + 1);
}

#[test]
fn a_new_prompt_clears_the_previous_command_text() {
    // The command text belongs to the newest region only, so the next prompt clears it.
    let mut grid = Grid::new(10, 3);
    prompt(&mut grid);
    grid.set_semantic_cmdline(CmdlineText::new("ls".to_owned()));
    assert_eq!(grid.semantic().cmdline.as_ref().map(CmdlineText::as_str), Some("ls"));
    prompt(&mut grid);
    assert!(grid.semantic().cmdline.is_none());
}

#[test]
fn cmdline_debug_redacted() {
    // Command text never reaches logs or diagnostics through `Debug`: only its length does, on
    // the text and on the state holding it.
    let text = CmdlineText::new("ssh secret-host".to_owned()).expect("short text is kept");
    let printed = format!("{text:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("15"), "{printed}");
    let mut grid = Grid::new(10, 3);
    grid.set_semantic_cmdline(Some(text));
    assert!(!format!("{:?}", grid.semantic()).contains("secret"));
}

#[test]
fn cmdline_text_is_bounded_and_counted() {
    // Decoded text past 1 KiB is refused, and kept text is charged to the grid's retained bytes.
    assert!(CmdlineText::new("x".repeat(MAX_CMDLINE_BYTES + 1)).is_none());
    let mut grid = Grid::new(10, 3);
    let before = grid.retained_amount().bytes;
    grid.set_semantic_cmdline(CmdlineText::new("x".repeat(MAX_CMDLINE_BYTES)));
    assert!(grid.retained_amount().bytes >= before + MAX_CMDLINE_BYTES);
    assert_eq!(grid.retained_amount_by_region().total(), grid.retained_amount());
}
