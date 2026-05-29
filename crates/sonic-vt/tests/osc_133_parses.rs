//! Integration tests for OSC 133 (shell-integration command lifecycle) parsing.

use sonic_grid::grid::Grid;
use sonic_vt::vt::{CommandEvent, Parser, VtEvent};

#[test]
fn osc_133_sequences_emit_command_lifecycle_events() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events =
        parser.advance(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]133;C\x1b\\\x1b]133;D;7\x1b\\");
    let command_events: Vec<CommandEvent> = events
        .into_iter()
        .filter_map(|ev| match ev {
            VtEvent::Command(cmd) => Some(cmd),
            _ => None,
        })
        .collect();
    assert_eq!(
        command_events,
        vec![
            CommandEvent::PromptStart,
            CommandEvent::CmdStart,
            CommandEvent::CmdStart,
            CommandEvent::CmdEnd(Some(7)),
        ]
    );
}

#[test]
fn osc_133_d_without_exit_emits_none() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = parser.advance(b"\x1b]133;D\x1b\\");
    assert!(events
        .into_iter()
        .any(|ev| matches!(ev, VtEvent::Command(CommandEvent::CmdEnd(None)))));
}
