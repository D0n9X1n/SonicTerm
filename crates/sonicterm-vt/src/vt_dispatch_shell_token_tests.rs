//! Pins OSC 133 marker trust against a pane's shell token: a marker with the token authenticates
//! and latches the pane for the parser's life; a wrong token, or any tokenless marker after the
//! latch, changes neither the grid nor the events, so a program's output cannot forge or
//! downgrade them. Without a token the parser behaves as before.

use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A parser for a pane whose shell was given `TOKEN`.
fn tokened() -> Parser {
    let mut parser = Parser::new(Grid::new(30, 6));
    parser.set_shell_token(Some(TOKEN.to_owned()));
    parser
}

/// OSC 133 `kind`, with `extra` parameters, terminated by BEL.
fn mark(kind: &str, extra: &[&str]) -> Vec<u8> {
    let mut text = format!("\x1b]133;{kind}");
    for param in extra {
        text.push(';');
        text.push_str(param);
    }
    text.push('\x07');
    text.into_bytes()
}

/// The token parameter a SonicTerm shell integration appends.
fn token_param(token: &str) -> String {
    format!("sonicterm_token={token}")
}

/// Whether `events` hold any OSC 133 command event.
fn has_command_event(events: &[VtEvent]) -> bool {
    events.iter().any(|event| matches!(event, VtEvent::Command(_)))
}

#[test]
fn a_tokened_marker_authenticates_and_latches_the_pane() {
    // The pane's own token admits the marker and marks the run's C and D authenticated.
    let mut parser = tokened();
    let good = token_param(TOKEN);
    parser.advance(&mark("A", &[&good]));
    parser.advance(&mark("B", &[&good]));
    assert!(parser.shell_markers_authenticated());
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(&mark("C", &[&good]));
    parser.advance(&mark("D", &["4", &good]));
    let run = parser.grid().pinned_run().unwrap();
    assert!(run.output_authenticated && run.end_authenticated);
    assert_eq!(run.exit, Some(Some(4)), "the exit code still comes from the first parameter");
}

#[test]
fn a_wrong_token_changes_neither_the_grid_nor_the_events() {
    // A program that guesses a token gains nothing: the marker is dropped entirely.
    let mut parser = tokened();
    let bad = token_param("ffffffffffffffffffffffffffffffff");
    let events = parser.advance(&mark("A", &[&bad]));
    assert!(!has_command_event(&events));
    assert_eq!(parser.grid().prompts_len(), 0);
    assert!(!parser.shell_markers_authenticated());
}

#[test]
fn after_the_latch_tokenless_markers_are_dropped() {
    // Once the shell proved itself, a forged tokenless D or A from command output is ignored, so
    // it can neither complete the run nor evict it.
    let mut parser = tokened();
    let good = token_param(TOKEN);
    parser.advance(&mark("A", &[&good]));
    parser.advance(&mark("B", &[&good]));
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(&mark("C", &[&good]));
    let events = parser.advance(&[mark("D", &["0"]), mark("A", &[])].concat());
    assert!(!has_command_event(&events));
    let run = parser.grid().pinned_run().unwrap();
    assert_eq!(run.exit, None, "the forged D was dropped");
    assert!(!run.evicted, "the forged A was dropped");
    parser.advance(&mark("D", &["1", &good]));
    parser.advance(&mark("A", &[&good]));
    let run = parser.grid().pinned_run().unwrap();
    assert_eq!(run.exit, Some(Some(1)));
    assert!(run.prompt_after_end && run.end_authenticated);
}

#[test]
fn the_latch_survives_a_reset_and_screen_switches() {
    // RIS and the alternate screen are output-controlled, so neither may downgrade the pane.
    let mut parser = tokened();
    let good = token_param(TOKEN);
    parser.advance(&mark("A", &[&good]));
    parser.advance(b"\x1bc");
    parser.advance(b"\x1b[?1049h\x1b[?1049l");
    assert!(parser.shell_markers_authenticated());
    let events = parser.advance(&mark("A", &[]));
    assert!(!has_command_event(&events), "a tokenless marker after RIS is still dropped");
}

#[test]
fn without_a_token_markers_behave_as_before_and_stay_unverified() {
    // A pane with no shell token keeps today's behavior, and nothing it records is verified.
    let mut parser = Parser::new(Grid::new(30, 6));
    parser.advance(&mark("A", &[]));
    parser.advance(&mark("B", &[]));
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    let events = parser.advance(&[mark("C", &[]), mark("D", &["0"])].concat());
    assert!(has_command_event(&events));
    let run = parser.grid().pinned_run().unwrap();
    assert_eq!(run.exit, Some(Some(0)));
    assert!(!run.output_authenticated && !run.end_authenticated);
    assert!(!parser.shell_markers_authenticated());
}

#[test]
fn before_the_latch_tokenless_markers_are_admitted_unverified() {
    // A tokened pane whose shell has not proved itself yet (third-party integration) still works,
    // but nothing it records is verified, and an unknown extra parameter is tolerated.
    let mut parser = tokened();
    parser.advance(&mark("A", &["k=s"]));
    assert_eq!(parser.grid().prompts_len(), 1);
    assert!(!parser.shell_markers_authenticated());
}

/// A tokened parser with an input flag, and the flag, as the app wires a shell pane.
fn tokened_with_input() -> (Parser, Arc<AtomicBool>) {
    let mut parser = tokened();
    let input_seen = Arc::new(AtomicBool::new(false));
    parser.set_shell_input_seen(Some(input_seen.clone()));
    (parser, input_seen)
}

#[test]
fn a_token_first_seen_after_input_never_latches() {
    // A shell with no integration leaves the token in every command's environment, so a command
    // the user ran can print it. Markers carrying it after input are admitted but never verified,
    // never latch the pane, and tokenless markers keep working.
    let (mut parser, input_seen) = tokened_with_input();
    input_seen.store(true, Ordering::SeqCst);
    let good = token_param(TOKEN);
    parser.advance(&mark("A", &[&good]));
    parser.advance(&mark("B", &[&good]));
    assert!(!parser.shell_markers_authenticated(), "a token after input proves nothing");
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(&[mark("C", &[&good]), mark("D", &["0", &good])].concat());
    let run = parser.grid().pinned_run().unwrap();
    assert_eq!(run.exit, Some(Some(0)), "the markers still record the run");
    assert!(!run.output_authenticated && !run.end_authenticated);
    let events = parser.advance(&mark("A", &[]));
    assert!(has_command_event(&events), "an unlatched pane still admits tokenless markers");
}

#[test]
fn input_after_the_latch_keeps_the_pane_verified() {
    // A shell integration latches at its first prompt, before anyone types; the input that
    // follows changes nothing, so its commands stay verified and forgeries stay dropped.
    let (mut parser, input_seen) = tokened_with_input();
    let good = token_param(TOKEN);
    parser.advance(&mark("A", &[&good]));
    input_seen.store(true, Ordering::SeqCst);
    parser.advance(&mark("B", &[&good]));
    let seq = parser.grid().prompts().last().unwrap().seq;
    assert!(parser.grid_mut().pin_run(seq));
    parser.advance(&[mark("C", &[&good]), mark("D", &["0", &good])].concat());
    let run = parser.grid().pinned_run().unwrap();
    assert!(run.output_authenticated && run.end_authenticated);
    let events = parser.advance(&mark("A", &[]));
    assert!(!has_command_event(&events), "the latch still drops tokenless markers");
}
