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

/// The queue must hold no more memory than its class records.
///
/// The bound above asserts `len`, which is the figure the cap controls.
/// Memory follows `capacity`, and the two part company here: the append
/// extends first and drains second, and `Vec::drain` lowers the length while
/// keeping the allocation. A batch larger than the cap therefore leaves the
/// queue trimmed to 1024 entries and still holding the peak allocation, for as
/// long as the pane lives.
///
/// One parse batch is one PTY chunk, and `OSC 133;A` is about eight bytes, so
/// a 64 KiB chunk of prompt markers is a realistic burst rather than an
/// adversarial one — the shell emits them, and a `for` loop over many short
/// commands produces exactly this.
#[test]
fn command_event_queue_holds_no_more_memory_than_its_class_records() {
    /// The per-pane figure the coverage table records for `CommandEvents`.
    const CLAIMED_PER_PANE_BYTES: usize = 40 * 1024;

    // A burst from one 64 KiB parse batch of prompt markers.
    const BURST: usize = 8 * 1024;

    let mut queue = Vec::new();
    append_bounded_command_events(
        &mut queue,
        (0..BURST).map(|index| PaneCommandEvent {
            event: CommandEvent::CmdEnd(Some((index % 256) as u8)),
            at: Instant::now(),
            duration: None,
        }),
    );

    // `PaneCommandEvent` holds no pointer to further heap, so the retained
    // bytes are exactly the allocation the capacity describes.
    let retained = queue.capacity() * std::mem::size_of::<PaneCommandEvent>();

    assert_eq!(queue.len(), MAX_PANE_COMMAND_EVENTS, "precondition: the cap still bounds length");
    assert!(
        retained <= CLAIMED_PER_PANE_BYTES,
        "the queue holds {retained} bytes ({} entries of capacity) after a {BURST}-event \
         batch, but the coverage table records {CLAIMED_PER_PANE_BYTES} per pane — the cap \
         bounds length and the memory follows capacity",
        queue.capacity()
    );
}
