//! Pins the finished-command records `command_history` lists: every D stamps its time and trust
//! and a later D replaces them, only a newer prompt settles a region, a forged D alone never lists
//! a running command, the list is newest first and bounded by FIFO eviction, and a region whose B
//! row left history keeps no B coordinates; C records its column.

use std::time::Duration;

use super::*;

/// Run one command at the cursor's row: A, B at column 2, C on the next row, then D with `exit`
/// on the row after it, leaving the cursor there.
fn command(grid: &mut Grid, exit: i32) {
    let row = grid.cursor.row;
    grid.record_prompt_start();
    grid.goto(row, 2);
    grid.record_input_start();
    grid.goto(row + 1, 0);
    grid.record_output_start();
    grid.goto(row + 2, 0);
    grid.record_prompt_end(Some(exit));
}

/// Every D stamps the region's end time and whether it was authenticated; a later D replaces the
/// code, the trust and the time together.
#[test]
fn every_d_stamps_the_end_and_a_later_d_replaces_it() {
    let mut grid = Grid::new(20, 10);
    command(&mut grid, 0);
    let first = *grid.newest_prompt().expect("region");
    assert!(first.end_at.is_some() && !first.end_authenticated);
    std::thread::sleep(Duration::from_millis(2));
    grid.record_prompt_end_marked(Some(1), true);
    let second = *grid.newest_prompt().expect("region");
    assert!(second.end_at > first.end_at, "the later D's time replaces the earlier one");
    assert!(second.end_authenticated);
    assert_eq!(second.exit_code, Some(1));
}

/// A finished command is listed only once a newer prompt opens after its D: a D printed by a
/// still-running program cannot list it, and the shell's real D before the next prompt wins. A
/// region without C is never listed.
#[test]
fn only_a_newer_prompt_settles_a_region() {
    let mut grid = Grid::new(20, 10);
    command(&mut grid, 0);
    assert!(grid.command_regions(10).is_empty(), "D alone does not settle the region");
    assert!(!grid.newest_prompt().expect("region").settled);
    // The program's forged D;0, then the shell's real D;3, then the next prompt.
    grid.record_prompt_end_marked(Some(3), true);
    grid.goto(3, 0);
    grid.record_prompt_start();
    let listed = grid.command_regions(10);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].seq, listed[0].exit_code, listed[0].end_authenticated),
        (0, Some(3), true)
    );
    assert_eq!(listed[0].input_start, Some((0, 2)));
    assert_eq!((listed[0].output_start_row, listed[0].end_row), (Some(1), Some(2)));
    assert!(listed[0].output_start_at.is_some() && listed[0].end_at.is_some());

    // A, D (no C), A: the middle region never ran a command, so it is not listed.
    grid.record_prompt_end(Some(0));
    grid.goto(4, 0);
    grid.record_prompt_start();
    assert_eq!(grid.command_regions(10).len(), 1);
}

/// Regions come newest first, `limit` bounds the list, a seq names any retained finished command,
/// and the primary screen's list is still read while the alternate screen is active.
#[test]
fn regions_list_newest_first_within_the_limit() {
    let mut grid = Grid::new(20, 12);
    for exit in 0..3 {
        command(&mut grid, exit);
    }
    grid.record_prompt_start();
    let seqs: Vec<u64> = grid.command_regions(10).iter().map(|region| region.seq).collect();
    assert_eq!(seqs, vec![2, 1, 0]);
    assert_eq!(grid.command_regions(2).len(), 2);
    assert_eq!(grid.completed_command(1).map(|span| span.exit_code), Some(Some(1)));
    assert_eq!(grid.completed_command(3), None, "the open prompt has no finished command");
    grid.enter_alt_screen();
    grid.record_prompt_start();
    assert_eq!(grid.command_regions(10).len(), 3, "the primary list is read under the alternate");
    assert_eq!(grid.completed_command(0).map(|span| span.exit_code), Some(Some(0)));
}

/// The list follows FIFO eviction: past the region limit the oldest commands are forgotten, and
/// their seqs no longer name a command.
#[test]
fn evicted_regions_leave_the_list() {
    let mut grid = Grid::new(10, 3);
    let commands = PROMPT_REGION_LIMIT as u64 + 4;
    for _ in 0..commands {
        grid.goto(0, 0);
        command(&mut grid, 0);
    }
    grid.goto(0, 0);
    grid.record_prompt_start();
    let listed = grid.command_regions(usize::MAX);
    // The newest region is the open prompt, so one fewer than the limit is listed.
    assert_eq!(listed.len(), PROMPT_REGION_LIMIT - 1);
    assert_eq!(
        listed.last().map(|region| region.seq),
        Some(commands + 1 - PROMPT_REGION_LIMIT as u64)
    );
    assert_eq!(grid.completed_command(0), None);
}

/// A long command whose B and C rows left history is still listed after the next prompt, with no
/// B coordinates and no output start, so its text and output start read as lost.
#[test]
fn a_region_whose_b_left_history_has_no_b_coordinates() {
    let mut grid = Grid::new(10, 2);
    grid.set_scrollback_limit(1);
    grid.record_prompt_start();
    grid.goto(0, 2);
    grid.record_input_start();
    grid.record_output_start();
    for _ in 0..3 {
        grid.scroll_up(1);
    }
    grid.record_prompt_end(Some(7));
    grid.goto(1, 0);
    grid.record_prompt_start();
    let listed = grid.command_regions(10);
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].input_start, listed[0].output_start_row), (None, None));
    assert_eq!(listed[0].exit_code, Some(7));
}

/// C records its column beside its row, and the listed command region carries it, so a command
/// whose output starts on its own row can stop the typed text where the output begins.
#[test]
fn c_records_its_column() {
    let mut grid = Grid::new(30, 10);
    grid.record_prompt_start();
    grid.goto(0, 2);
    grid.record_input_start();
    grid.goto(0, 9);
    grid.record_output_start();
    grid.goto(0, 15);
    grid.record_prompt_end(Some(0));
    let region = *grid.newest_prompt().expect("region");
    assert_eq!((region.output_start_row, region.output_start_col), (Some(0), 9));
    grid.goto(1, 0);
    grid.record_prompt_start();
    let listed = grid.command_regions(10);
    assert_eq!((listed[0].output_start_row, listed[0].output_start_col), (Some(0), 9));
}

/// A C that arrives while a wrap is pending sits past the full row, so the character in the last
/// column still counts as typed text rather than being cut off at C.
#[test]
fn c_after_a_full_row_records_the_column_past_it() {
    let mut grid = Grid::new(4, 3);
    grid.record_prompt_start();
    grid.record_input_start();
    for character in "abcd".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert!(grid.pending_wrap());
    grid.record_output_start();
    assert_eq!(grid.newest_prompt().expect("region").output_start_col, 4);
}
