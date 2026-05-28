use std::collections::HashMap;
use std::time::{Duration, Instant};

use sonic_app::app::{poll_command_events_for_tab_state, PaneCommandEvent, PaneState, TabState};
use sonic_core::{
    config::Config,
    grid::Grid,
    vt::{CommandEvent, Parser},
};
use sonic_ui::{
    pane::PaneTree,
    tabs::{CommandStatus, Tab, TabBar},
};

#[test]
fn command_events_are_polled_for_child_window_tabs() {
    let pane_id = sonic_app::app::next_pane_id();
    let parser = std::sync::Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(80, 24))));
    let pane = PaneState::new(parser, None);
    let started = Instant::now() - Duration::from_secs(6);
    pane.command_events.lock().push(PaneCommandEvent {
        event: CommandEvent::CmdStart,
        at: started,
        duration: None,
    });

    let mut panes = HashMap::new();
    panes.insert(pane_id, pane);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("child"));
    let mut tab_states = vec![TabState::new(PaneTree::leaf(pane_id), pane_id)];

    poll_command_events_for_tab_state(&panes, &mut tab_states, &mut tabs, &Config::default(), 0);

    assert_eq!(tab_states[0].command, CommandStatus::Running(started));
    assert_eq!(tabs.tabs()[0].command.clone().badge(Instant::now(), false), Some("…"));
}
