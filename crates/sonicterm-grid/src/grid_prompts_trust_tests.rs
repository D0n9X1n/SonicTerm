//! Pins what a pinned run records about trust: whether its C and latest D came from authenticated
//! shell markers, and whether a later prompt arrived after D. A forged D from the command's own
//! output must never by itself mark the run finished or verified.

use super::*;

/// A grid with a prompt opened and pinned, ready for the command's C.
fn pinned(cols: u16, rows: u16) -> Grid {
    let mut grid = Grid::new(cols, rows);
    grid.record_prompt_start();
    grid.record_input_start();
    let seq = grid.prompts().last().expect("a region").seq;
    assert!(grid.pin_run(seq));
    grid
}

#[test]
fn a_d_alone_records_the_exit_but_no_later_prompt() {
    // The command's own output can print D, so D alone never says the shell got control back.
    let mut grid = pinned(20, 5);
    grid.record_output_start_marked(false);
    grid.record_prompt_end_marked(Some(0), false);
    let run = grid.pinned_run().unwrap();
    assert_eq!(run.exit, Some(Some(0)));
    assert!(!run.prompt_after_end);
    assert!(!run.output_authenticated && !run.end_authenticated);
}

#[test]
fn the_latest_d_wins_and_a_later_prompt_closes_the_run() {
    // A forged D;0 and then the shell's real D;1: the run keeps the latest exit, and only the
    // shell's next prompt marks it finished.
    let mut grid = pinned(20, 5);
    grid.record_output_start_marked(false);
    grid.record_prompt_end_marked(Some(0), false);
    grid.record_prompt_end_marked(Some(1), false);
    grid.record_prompt_start();
    let run = grid.pinned_run().unwrap();
    assert_eq!(run.exit, Some(Some(1)));
    assert!(run.prompt_after_end);
    assert!(!run.evicted, "a prompt after D completes the run rather than evicting it");
}

#[test]
fn verification_needs_the_run_s_own_authenticated_c_and_latest_d() {
    // An authenticated D after a tokenless C stays unverified; both must be authenticated.
    let mut grid = pinned(20, 5);
    grid.record_output_start_marked(false);
    grid.record_prompt_end_marked(Some(0), true);
    let run = grid.pinned_run().unwrap();
    assert!(!run.output_authenticated && run.end_authenticated);
    let mut grid = pinned(20, 5);
    grid.record_output_start_marked(true);
    grid.record_prompt_end_marked(Some(2), true);
    let run = grid.pinned_run().unwrap();
    assert!(run.output_authenticated && run.end_authenticated);
    assert_eq!(run.exit, Some(Some(2)));
}

#[test]
fn a_later_prompt_on_the_other_screen_does_not_close_the_run() {
    // Marks on the alternate screen never touch a run pinned on the primary one.
    let mut grid = pinned(20, 5);
    grid.record_output_start_marked(false);
    grid.record_prompt_end_marked(Some(0), false);
    grid.enter_alt_screen();
    grid.record_prompt_start();
    assert!(!grid.pinned_run().unwrap().prompt_after_end);
}

#[test]
fn a_finished_pin_keeps_its_result_through_eviction_and_later_commands() {
    // A run that ended (D, then the shell's next prompt) keeps its own exit code, trust and
    // completion while more than `PROMPT_REGION_LIMIT` later prompts evict its region and later
    // commands end with their own C and D: no newer region's marks reach the older pin.
    let mut grid = pinned(20, 5);
    let seq = grid.pinned_run().unwrap().seq;
    grid.record_output_start_marked(true);
    grid.record_prompt_end_marked(Some(3), true);
    grid.record_prompt_start();
    for _ in 0..PROMPT_REGION_LIMIT + 10 {
        grid.record_input_start();
        grid.record_output_start_marked(false);
        grid.record_prompt_end_marked(Some(0), false);
        grid.linefeed();
        grid.record_prompt_start();
    }
    assert!(grid.prompts().all(|region| region.seq != seq), "the pin's region was evicted");
    let run = grid.pinned_run().unwrap();
    assert_eq!(run.seq, seq);
    assert_eq!(run.exit, Some(Some(3)));
    assert!(run.output_authenticated && run.end_authenticated);
    assert!(run.prompt_after_end && !run.evicted);
}

#[test]
fn an_unfinished_pin_is_lost_to_a_newer_prompt_and_untouched_by_its_marks() {
    // A newer prompt before the pin's D means its D can no longer arrive: the pin is marked lost,
    // and the newer region's C and D, and every later one through eviction, leave it untouched.
    let mut grid = pinned(20, 5);
    let seq = grid.pinned_run().unwrap().seq;
    grid.record_output_start_marked(false);
    grid.record_prompt_start();
    for _ in 0..PROMPT_REGION_LIMIT + 10 {
        grid.record_input_start();
        grid.record_output_start_marked(true);
        grid.record_prompt_end_marked(Some(7), true);
        grid.linefeed();
        grid.record_prompt_start();
    }
    let run = grid.pinned_run().unwrap();
    assert_eq!(run.seq, seq);
    assert!(run.evicted);
    assert_eq!(run.exit, None, "a later region's D reached the pin");
    assert!(!run.end_authenticated && !run.prompt_after_end);
}
