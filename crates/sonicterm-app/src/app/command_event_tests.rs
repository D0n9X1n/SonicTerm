use super::*;

#[test]
fn command_event_queue_retains_only_newest_entries() {
    let mut queue = Vec::new();
    append_bounded_command_events(
        &mut queue,
        (0..MAX_PANE_COMMAND_EVENTS + 10).map(|index| PaneCommandEvent {
            event: CommandEvent::CmdEnd(Some((index % 256) as u8)),
            at: Instant::now(),
            duration: None,
        }),
    );

    assert_eq!(queue.len(), MAX_PANE_COMMAND_EVENTS);
    assert_eq!(queue.first().map(|entry| entry.event), Some(CommandEvent::CmdEnd(Some(10))));
}
