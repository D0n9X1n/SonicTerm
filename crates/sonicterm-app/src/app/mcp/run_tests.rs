//! Pins `run_command`'s precondition and outcome rules: no shell integration, a running command, a
//! draft at the prompt and typing during the previous command each refuse; a run completes on C
//! then D, did not start on D alone, is interfered with by typing at or before C, and is lost when
//! its record is evicted or the shell exits.

use std::time::{Duration, Instant};

use sonicterm_grid::grid::{Grid, PinnedRun};
use sonicterm_vt::vt::{CaptureStagingPool, Parser};

use super::*;

/// A parser that received `bytes`.
fn parsed(bytes: &[u8]) -> Parser {
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(40, 8), None, CaptureStagingPool::new());
    parser.advance(bytes);
    parser
}

/// A prompt marked with A and B and nothing typed after it.
const READY: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

/// A prompt with no marks, one still running, or one with a draft is refused; a clean marked
/// prompt names its region.
#[test]
fn the_prompt_must_be_marked_idle_and_empty() {
    assert_eq!(check_prompt(parsed(b"$ ").grid(), None), Err(Refusal::NoShellIntegration));
    let running = parsed(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07building");
    assert_eq!(check_prompt(running.grid(), None), Err(Refusal::Busy));
    let draft = parsed(b"\x1b]133;A\x07$ \x1b]133;B\x07rm -rf");
    assert_eq!(check_prompt(draft.grid(), None), Err(Refusal::DraftPresent));
    let moved_home = parsed(b"\x1b]133;A\x07$ \x1b]133;B\x07typed\x1b[3D");
    assert_eq!(check_prompt(moved_home.grid(), None), Err(Refusal::DraftPresent));
    assert!(check_prompt(parsed(READY).grid(), None).is_ok());
}

/// Typing admitted while the previous command ran may still be in flight, so it refuses; typing
/// only before that command started does not; a pane with no earlier command must never have
/// been typed into.
#[test]
fn typing_during_the_previous_command_refuses() {
    let before = Instant::now();
    let parser = parsed(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07");
    let mut after = parser;
    after.advance(READY);
    let during = Instant::now();
    assert_eq!(check_prompt(after.grid(), Some(during)), Err(Refusal::DraftPresent));
    assert!(check_prompt(after.grid(), Some(before)).is_ok());
    assert_eq!(check_prompt(parsed(READY).grid(), Some(before)), Err(Refusal::DraftPresent));
}

/// A pinned record for region 0.
fn pinned() -> PinnedRun {
    PinnedRun {
        seq: 0,
        alt: false,
        output_seen: false,
        output_start: None,
        output_at: None,
        end: None,
        end_col: 0,
        exit: None,
        evicted: false,
        prompt_after_end: false,
        output_authenticated: false,
        end_authenticated: false,
        exit_source: None,
    }
}

/// C then D and the shell's next prompt complete a run; D without C did not start one; eviction
/// or an exited shell loses it; a revoked call ends it; nothing yet keeps waiting.
#[test]
fn run_states_follow_the_pinned_record() {
    let waiting = pinned();
    assert_eq!(run_state(Some(waiting), None, false, false), None);
    assert_eq!(run_state(Some(waiting), None, false, true), Some(RunState::Revoked));
    let output_at = Instant::now();
    let started = PinnedRun { output_seen: true, output_at: Some(output_at), ..waiting };
    let done = PinnedRun { exit: Some(Some(5)), prompt_after_end: true, ..started };
    assert_eq!(run_state(Some(done), None, false, false), Some(RunState::Completed));
    let rejected = PinnedRun { exit: Some(Some(130)), prompt_after_end: true, ..waiting };
    assert_eq!(
        run_state(Some(rejected), None, false, false),
        Some(RunState::NotStarted),
        "D without C ran nothing"
    );
    assert_eq!(
        run_state(Some(PinnedRun { evicted: true, ..started }), None, false, false),
        Some(RunState::Lost)
    );
    assert_eq!(run_state(Some(started), None, true, false), Some(RunState::Lost));
    assert_eq!(run_state(None, None, false, false), None, "a busy parser keeps waiting");
}

/// Typing is judged by when it arrived, not by when the worker looked: typing at or before C
/// interferes even when C and D both arrived before the next poll, and typing after C is the
/// running command's input.
#[test]
fn typing_is_ordered_against_c_not_against_the_poll() {
    let output_at = Instant::now();
    let before = output_at - Duration::from_millis(5);
    let after = output_at + Duration::from_millis(5);
    let done = PinnedRun {
        output_seen: true,
        output_at: Some(output_at),
        exit: Some(Some(0)),
        prompt_after_end: true,
        ..pinned()
    };
    assert_eq!(run_state(Some(done), Some(before), false, false), Some(RunState::Interfered));
    assert_eq!(run_state(Some(done), Some(output_at), false, false), Some(RunState::Interfered));
    assert_eq!(run_state(Some(done), Some(after), false, false), Some(RunState::Completed));
    assert_eq!(
        run_state(Some(pinned()), Some(before), false, false),
        Some(RunState::Interfered),
        "typing with no C yet came first"
    );
}

/// A typing stamp is never 0, which means no typing, and names the instant it was taken from.
#[test]
fn typing_stamps_round_trip_and_are_never_zero() {
    let first = Instant::now();
    assert!(typing_stamp(first) >= 1);
    let later = first + Duration::from_millis(3);
    let back = typing_instant(typing_stamp(later)).expect("a stamp names an instant");
    assert!(back >= later - Duration::from_micros(1) && back <= later + Duration::from_micros(1));
    assert_eq!(typing_instant(0), None);
}

/// A command with any control character is refused before anything else is checked.
#[test]
fn control_characters_are_refused() {
    for command in ["ls\nrm -rf /", "a\rb", "\x1b[31m", "x\u{85}y"] {
        assert!(has_control_character(command), "{command:?}");
    }
    assert!(!has_control_character("echo 'hi there' | wc -c"));
}

/// A D alone never ends an unauthenticated run: the command's own output can print it, so the
/// run waits for the shell's next prompt. That stops `printf '\e]133;D;0\a'; sleep 3; false`
/// from reporting success while `sleep` still runs.
#[test]
fn a_lone_unauthenticated_d_keeps_the_run_waiting() {
    let forged = PinnedRun {
        output_seen: true,
        output_at: Some(Instant::now()),
        exit: Some(Some(0)),
        ..pinned()
    };
    assert_eq!(run_state(Some(forged), None, false, false), None);
    let closed = PinnedRun { prompt_after_end: true, exit: Some(Some(1)), ..forged };
    assert_eq!(run_state(Some(closed), None, false, false), Some(RunState::Completed));
}

/// An authenticated C and D end the run at D: the pane's token proves the shell sent them, and
/// only then is the exit code verified.
#[test]
fn an_authenticated_run_completes_at_d_and_is_verified() {
    let verified = PinnedRun {
        output_seen: true,
        output_at: Some(Instant::now()),
        exit: Some(Some(0)),
        output_authenticated: true,
        end_authenticated: true,
        ..pinned()
    };
    assert_eq!(run_state(Some(verified), None, false, false), Some(RunState::Completed));
    assert!(exit_verified(&verified));
    let mixed = PinnedRun { output_authenticated: false, ..verified };
    assert!(!exit_verified(&mixed), "a tokenless C with an authenticated D stays unverified");
    assert_eq!(run_state(Some(mixed), None, false, false), None, "and waits for a prompt");
}

/// A verified D without a code (PowerShell's line that ran both a failing cmdlet and a failing
/// native command) still completes the run, with no exit code rather than a guessed one.
#[test]
fn an_authenticated_d_without_a_code_completes_with_no_exit_code() {
    let unknown = PinnedRun {
        output_seen: true,
        output_at: Some(Instant::now()),
        exit: Some(None),
        output_authenticated: true,
        end_authenticated: true,
        ..pinned()
    };
    assert_eq!(run_state(Some(unknown), None, false, false), Some(RunState::Completed));
    assert!(exit_verified(&unknown));
    assert_eq!(unknown.exit.flatten(), None, "the code is unknown, not zero");
}

/// A closed pane's finish waits only for what is left of the close drain: nothing once the drain
/// has passed, so the run returns within that one bound and never a `FINISH_WAIT` past it.
#[test]
fn the_finish_budget_is_what_the_close_drain_has_left() {
    let drain = super::super::tools_read::CLOSE_DRAIN;
    assert_eq!(close_budget(None), FINISH_WAIT, "an open pane waits the normal finish wait");
    let expired = Instant::now().checked_sub(drain + Duration::from_millis(1)).unwrap();
    assert_eq!(close_budget(Some(expired)), Duration::ZERO, "a drained pane waits nothing more");
    // Just closed: the drain's remainder exceeds the finish wait, which still caps it.
    assert_eq!(close_budget(Some(Instant::now())), FINISH_WAIT);
}

/// OSC 133 annotations (P, I, N, L, the options on A) and 9;12 change only the semantic state:
/// after a ready prompt they leave every region and `run_command`'s readiness exactly as they were.
#[test]
fn annotations_never_change_regions_or_mcp_readiness() {
    let mut parser = parsed(b"\x1b]133;A;redraw=last;click_events=2\x07$ \x1b]133;B\x07");
    let regions: Vec<_> = parser.grid().prompts().copied().collect();
    let ready = check_prompt(parser.grid(), None);
    assert!(ready.is_ok());
    for annotation in [
        &b"\x1b]133;P;k=i\x07"[..],
        b"\x1b]133;P;k=c\x07",
        b"\x1b]133;I\x07",
        b"\x1b]133;N\x07",
        b"\x1b]9;12\x07",
        b"\x1b]133;L\x07",
    ] {
        parser.advance(annotation);
        let after: Vec<_> = parser.grid().prompts().copied().collect();
        assert_eq!(after, regions, "{annotation:?} changed a region");
        assert_eq!(check_prompt(parser.grid(), None), ready, "{annotation:?} changed readiness");
    }
}

/// A pinned run keeps waiting through N, P and 9;12 after its C: only an A closes or evicts it.
#[test]
fn n_does_not_evict_pinned_run() {
    let mut parser = parsed(READY);
    let seq = check_prompt(parser.grid(), None).expect("ready");
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(b"make\r\n\x1b]133;C\x07");
    parser.advance(b"\x1b]133;N\x07\x1b]9;12\x07\x1b]133;P;k=i\x07");
    let run = parser.grid().pinned_run().expect("pinned");
    assert!(!run.evicted, "an annotation evicted the run");
    parser.advance(b"\x1b]133;D;0\x07\x1b]133;N\x07");
    let run = parser.grid().pinned_run().expect("pinned");
    assert_eq!(run.exit, Some(Some(0)));
    assert!(!run.prompt_after_end, "only an A says the shell took control back");
}

/// A parser of `cols` x `rows` with `scrollback` rows of history that received `bytes`.
fn parsed_sized(cols: u16, rows: u16, scrollback: usize, bytes: &[u8]) -> Parser {
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(cols, rows), None, CaptureStagingPool::new());
    parser.grid_mut().set_scrollback_limit(scrollback);
    parser.advance(bytes);
    parser
}

/// With history off, a rows shrink under a live prompt discards the rows above the cursor's row.
/// B moves up with its row, so a shell-prefilled draft on B's row (no typing stamp) still refuses
/// `run_command`; reading B's old position would see the blank row that took it and run.
#[test]
fn a_no_history_prompt_shrink_keeps_the_draft_refused() {
    let mut parser = parsed_sized(40, 4, 0, b"o0\r\no1\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n");
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
    assert_eq!(parser.resize_terminal(40, 3).scrolled_rows, 1);
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
}

/// Readiness fails closed when the marks can no longer locate the input: a plain clip (Windows'
/// resize) cut B's row or a narrower grid cut B's column, so no cell is left to read as blank.
/// A newest prompt whose A row was discarded refuses rather than handing readiness to an older
/// prompt below it (a cleared screen), whose rows would then be read in its place.
#[test]
fn readiness_fails_closed_when_the_marks_no_longer_locate_the_input() {
    let mut cut_row = parsed(b"\r\n\r\n\r\n\r\n\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls");
    cut_row.resize(40, 3);
    assert_eq!(check_prompt(cut_row.grid(), None), Err(Refusal::NoShellIntegration));
    let mut cut_col = parsed(b"\x1b]133;A\x07prompt> \x1b]133;B\x07ls");
    cut_col.resize(5, 8);
    assert_eq!(check_prompt(cut_col.grid(), None), Err(Refusal::NoShellIntegration));
    // An abandoned prompt on row 3, then Ctrl-L: the screen is erased and the next prompt opens on
    // row 0, above the old prompt's region, which keeps its (now blank) rows.
    let mut cleared = parsed_sized(
        40,
        4,
        0,
        b"\x1b[4;1H\x1b]133;A\x07$ \x1b]133;B\x07\x1b[H\x1b[2J\x1b]133;A\x07$ \x1b]133;B\x07",
    );
    assert!(check_prompt(cleared.grid(), None).is_ok());
    cleared.grid_mut().scroll_up(1);
    assert_eq!(check_prompt(cleared.grid(), None), Err(Refusal::NoShellIntegration));
}

/// Physical row eviction is the one way an annotation reaches the regions: a fresh line (`L`, `N`,
/// `9;12`) at a full bottom margin scrolls like any line feed, and the scroll moves or evicts
/// regions. Once the prompt's A row is discarded (history off) or leaves history, readiness
/// refuses instead of reading the rows that replaced the prompt.
#[test]
fn a_fresh_line_at_a_full_bottom_margin_fails_readiness_closed() {
    let annotations = [&b"\x1b]133;L\x07"[..], b"\x1b]133;N\x07", b"\x1b]9;12\x07"];
    let prompts = [
        (0, &b"\x1b]133;A\x07~/src\r\n$ \x1b]133;B\x07"[..]),
        (1, b"\x1b]133;A\x07~/src\r\nmain\r\n$ \x1b]133;B\x07"),
    ];
    for annotation in annotations {
        for (scrollback, prompt) in prompts {
            let mut parser = parsed_sized(40, 2, scrollback, prompt);
            assert!(check_prompt(parser.grid(), None).is_ok(), "{scrollback}");
            parser.advance(annotation);
            assert_eq!(
                check_prompt(parser.grid(), None),
                Err(Refusal::NoShellIntegration),
                "{annotation:?} with {scrollback} history row(s)"
            );
        }
    }
}

/// The shell can prefill later lines and put the caret back on B. Losing that tail makes the
/// remaining blanks untrustworthy even when the resize did not move the caret or either mark.
#[test]
fn readiness_clip_loses_trailing_draft_below_retained_b() {
    let mut parser =
        parsed_sized(20, 5, 0, b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b[5;1Hdraft\x1b[1;3H");
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
    assert_eq!(parser.resize_terminal(20, 3).scrolled_rows, 0);
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    parser.advance(b"\x1b]133;B\x07");
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    parser.advance(b"\x1b[H");
    parser.advance(READY);
    assert!(check_prompt(parser.grid(), None).is_ok());
}

/// Padding on growth is not fresh prompt evidence, even if B's former row or column exists again.
#[test]
fn readiness_clip_growth_cannot_resurrect_b() {
    for (cols, rows) in [(20, 2), (1, 5)] {
        let mut parser = parsed_sized(20, 5, 0, b"\x1b[4;1H\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(check_prompt(parser.grid(), None).is_ok());
        parser.resize(cols, rows);
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
        parser.resize(20, 5);
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    }
}

/// Moving rows in a proper margin must not leave an apparently empty input area at B's old
/// location. A margin entirely above the prompt, by contrast, does not affect its proof.
#[test]
fn readiness_margin_move_puts_draft_above_b() {
    let mut parser = parsed_sized(20, 6, 0, b"\x1b[4;1H\x1b]133;A\x07$ \x1b]133;B\x07");
    let ready = check_prompt(parser.grid(), None);
    assert!(ready.is_ok());
    parser.advance(b"\x1b[1;2r\x1b[S");
    assert_eq!(check_prompt(parser.grid(), None), ready);
    parser.advance(b"\x1b[2;5r\x1b[4;3Hdraft");
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
    parser.advance(b"\x1b[S");
    assert!(parser.grid().row(2).iter().any(|cell| cell.ch == 'd'));
    assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
}

/// Fresh-line annotations at a partial bottom margin physically scroll the draft out of its
/// marked input area. None provides a fresh A/B pair, and none changes a pinned run record.
#[test]
fn readiness_margin_annotations_invalidate_without_touching_a_pin() {
    for annotation in [&b"\x1b]133;L\x07"[..], b"\x1b]133;N\x07", b"\x1b]9;12\x07"] {
        let mut parser =
            parsed_sized(20, 6, 0, b"\x1b[2;5r\x1b[5;1H\x1b]133;A\x07$ \x1b]133;B\x07");
        let seq = check_prompt(parser.grid(), None).unwrap();
        assert!(parser.grid_mut().pin_run(seq));
        let pin = parser.grid().pinned_run();
        parser.advance(b"draft");
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
        parser.advance(annotation);
        assert_eq!(parser.grid().pinned_run(), pin);
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    }
}

/// A narrower grid can erase the only nonblank even when the input mark and the lead of a
/// clipped wide glyph are in range. Blank padding on a later resize cannot repair the proof.
#[test]
fn readiness_clip_loses_right_edge_draft() {
    for draft in ["\x1b[1;9Hx", "\x1b[1;8H界"] {
        let mut parser = parsed_sized(20, 4, 0, READY);
        parser.advance(draft.as_bytes());
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
        parser.resize(8, 4);
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    }
}

/// A downward move can eject a draft through the bottom without changing the row number in B.
/// Neither a partial region nor a full-screen reverse scroll may turn that into a blank proof.
#[test]
fn readiness_downward_move_cannot_discard_draft_evidence() {
    for full_screen in [false, true] {
        let mut parser = parsed_sized(20, 6, 0, READY);
        parser.advance(b"\x1b[6;1Htail");
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::DraftPresent));
        if full_screen {
            parser.grid_mut().scroll_down(1);
        } else {
            parser.advance(b"\x1b[3;6r\x1b[T");
        }
        assert_eq!(check_prompt(parser.grid(), None), Err(Refusal::NoShellIntegration));
    }
}
