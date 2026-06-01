use std::collections::HashMap;
use std::time::{Duration, Instant};

use sonicterm_app::app::{
    __test_capture_command_notifications, __test_drain_command_notifications,
    poll_command_events_for_tab_state, PaneCommandEvent, PaneState, TabState,
};
use sonicterm_core::{
    config::{Config, NotificationsConfig},
    grid::Grid,
    vt::{CommandEvent, Parser},
};
use sonicterm_ui::{
    pane::PaneTree,
    tabs::{CommandStatus, Tab, TabBar},
};

#[test]
fn cmd_end_followed_by_prompt_start_keeps_done_status_and_badge() {
    let pane_id = sonicterm_app::app::next_pane_id();
    let parser = std::sync::Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(80, 24))));
    let pane = PaneState::new(parser, None);
    let started = Instant::now() - Duration::from_secs(12);
    let ended = started + Duration::from_secs(11);
    pane.command_events.lock().push(PaneCommandEvent {
        event: CommandEvent::CmdStart,
        at: started,
        duration: None,
    });
    pane.command_events.lock().push(PaneCommandEvent {
        event: CommandEvent::CmdEnd(Some(0)),
        at: ended,
        duration: Some(Duration::from_secs(11)),
    });
    pane.command_events.lock().push(PaneCommandEvent {
        event: CommandEvent::PromptStart,
        at: ended,
        duration: None,
    });

    let mut panes = HashMap::new();
    panes.insert(pane_id, pane);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("cmd"));
    let mut tab_states = vec![TabState::new(PaneTree::leaf(pane_id), pane_id)];
    let config = Config {
        notifications: NotificationsConfig { long_command: true, threshold_secs: 10 },
        ..Config::default()
    };
    __test_capture_command_notifications();

    poll_command_events_for_tab_state(&panes, &mut tab_states, &mut tabs, &config, 0);

    let expected = CommandStatus::Done { exit: Some(0), until: ended + Duration::from_secs(3) };
    assert_eq!(tab_states[0].command, expected);
    assert_eq!(tabs.tabs()[0].command, expected);
    assert_eq!(tabs.tabs()[0].command.clone().badge(ended, false), Some("✓"));
    assert_eq!(
        __test_drain_command_notifications(),
        vec!["Command completed successfully after 11s"]
    );
}
