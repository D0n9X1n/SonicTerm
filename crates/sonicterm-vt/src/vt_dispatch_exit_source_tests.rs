//! Pins the D marker's `sonicterm_src=` key: read by key wherever it sits, the last one winning,
//! an unknown value naming no source, a D with no code keeping none, and a later D replacing both
//! code and source. The source is recorded beside the D's trust, never in place of it.

use super::*;
use sonicterm_grid::grid::{ExitSource, PinnedRun};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A tokened parser with a run pinned at its first prompt and that run's C already seen.
fn started() -> Parser {
    let mut parser = Parser::new(Grid::new(30, 6));
    parser.set_shell_token(Some(TOKEN.to_owned()));
    let good = format!("sonicterm_token={TOKEN}");
    parser.advance(format!("\x1b]133;A;{good}\x07\x1b]133;B;{good}\x07").as_bytes());
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(format!("\x1b]133;C;{good}\x07").as_bytes());
    parser
}

/// Send a D with `params` (joined by `;`) and return the pinned run.
fn end(parser: &mut Parser, params: &str) -> PinnedRun {
    parser.advance(format!("\x1b]133;D;{params}\x07").as_bytes());
    parser.grid().pinned_run().unwrap()
}

/// The key is read wherever it sits, before or after the token, and its value maps to a source.
#[test]
fn the_source_is_read_by_key_in_any_position() {
    let token = format!("sonicterm_token={TOKEN}");
    let run = end(&mut started(), &format!("4;sonicterm_src=native;{token}"));
    assert_eq!((run.exit, run.exit_source), (Some(Some(4)), Some(ExitSource::Native)));
    let run = end(&mut started(), &format!("1;{token};sonicterm_src=powershell"));
    assert_eq!((run.exit, run.exit_source), (Some(Some(1)), Some(ExitSource::PowerShell)));
    assert!(run.end_authenticated, "the source key does not disturb the token");
}

/// An unknown value names no source (reported later as `reported`), and a repeated key takes the
/// last value.
#[test]
fn an_unknown_value_names_none_and_the_last_key_wins() {
    let token = format!("sonicterm_token={TOKEN}");
    let run = end(&mut started(), &format!("0;sonicterm_src=telepathy;{token}"));
    assert_eq!(run.exit_source, None);
    let run = end(&mut started(), &format!("0;sonicterm_src=native;sonicterm_src=status;{token}"));
    assert_eq!(run.exit_source, Some(ExitSource::Status));
}

/// A D whose first parameter is the token or the key carries no code, so the key is never read
/// as one.
#[test]
fn a_d_without_a_code_has_no_code() {
    let run = end(&mut started(), &format!("sonicterm_token={TOKEN}"));
    assert_eq!(run.exit, Some(None));
    let run = end(&mut started(), &format!("sonicterm_src=native;sonicterm_token={TOKEN}"));
    assert_eq!(run.exit, Some(None), "the key in the code's place is not a code");
}

/// A later D replaces the code and the source together: a source never outlives its code.
#[test]
fn a_later_d_replaces_code_and_source_together() {
    let mut parser = started();
    let token = format!("sonicterm_token={TOKEN}");
    end(&mut parser, &format!("3;sonicterm_src=native;{token}"));
    let run = end(&mut parser, &format!("0;{token}"));
    assert_eq!((run.exit, run.exit_source), (Some(Some(0)), None));
}

/// A tokenless D on an untokened pane keeps its source but stays unauthenticated, so the source
/// alone never makes the code trusted.
#[test]
fn a_tokenless_source_is_never_trusted() {
    let mut parser = Parser::new(Grid::new(30, 6));
    parser.advance(b"\x1b]133;A\x07\x1b]133;B\x07");
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(b"\x1b]133;C\x07");
    let run = end(&mut parser, "2;sonicterm_src=native");
    assert_eq!(run.exit_source, Some(ExitSource::Native));
    assert!(!run.end_authenticated && !run.output_authenticated);
}
