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
