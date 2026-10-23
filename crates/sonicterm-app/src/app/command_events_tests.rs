use super::*;

/// Prompt completion remains observational; only execution transitions the tab to Running.
#[test]
fn prompt_end_preserves_idle_and_completed_tab_status() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("shell");
    let at = Instant::now();
    app.__test_push_pane_command_event(pane, CommandEvent::PromptEnd, at, None);
    app.poll_command_events_for_all_tabs();
    assert!(matches!(app.main().unwrap().tab_states[0].command, CommandStatus::Idle));
    app.__test_push_pane_command_event(pane, CommandEvent::CmdStart, at, None);
    app.poll_command_events_for_all_tabs();
    assert!(
        matches!(app.main().unwrap().tab_states[0].command, CommandStatus::Running(started) if started == at)
    );
    app.__test_push_pane_command_event(pane, CommandEvent::CmdEnd(Some(0)), at, None);
    app.__test_push_pane_command_event(pane, CommandEvent::PromptEnd, at, None);
    app.poll_command_events_for_all_tabs();
    assert!(matches!(
        app.main().unwrap().tab_states[0].command,
        CommandStatus::Done { exit: Some(0), .. }
    ));
}

#[test]
fn command_event_queue_retains_only_newest_entries() {
    let mut queue = CommandQueue::default();
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

    let mut queue = CommandQueue::default();
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

/// The main window's command state for `pane`.
fn state_of(app: &App, pane: u64) -> PaneCommandState {
    app.main().unwrap().panes[&pane].command_state
}

/// Every transition row: `B` and `A` confirm a prompt, `C` runs from its instant, `D` records the
/// worker's duration, an `A` while running ends the command as abandoned, and `D` without `C`
/// records no duration. No exit code is kept.
#[test]
fn pane_command_state_follows_each_marker() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("shell");
    let start = Instant::now();
    assert_eq!(state_of(&app, pane), PaneCommandState::default(), "no marker: Unknown");
    app.__test_push_pane_command_event(pane, CommandEvent::PromptEnd, start, None);
    app.poll_command_events_for_all_tabs();
    assert_eq!(state_of(&app, pane).phase, CommandPhase::Prompt);
    let run = start + Duration::from_secs(1);
    app.__test_push_pane_command_event(pane, CommandEvent::CmdStart, run, None);
    app.poll_command_events_for_all_tabs();
    assert_eq!(state_of(&app, pane).phase, CommandPhase::Running { started_at: run });
    let end = run + Duration::from_secs(5);
    app.__test_push_pane_command_event(
        pane,
        CommandEvent::CmdEnd(Some(2)),
        end,
        Some(Duration::from_secs(5)),
    );
    app.poll_command_events_for_all_tabs();
    let done =
        LastCommand { ended_at: end, duration: Some(Duration::from_secs(5)), abandoned: false };
    assert_eq!(
        state_of(&app, pane),
        PaneCommandState { phase: CommandPhase::Prompt, last: Some(done) }
    );
    let rerun = end + Duration::from_secs(1);
    let prompt = rerun + Duration::from_secs(3);
    app.__test_push_pane_command_event(pane, CommandEvent::CmdStart, rerun, None);
    app.__test_push_pane_command_event(pane, CommandEvent::PromptStart, prompt, None);
    app.poll_command_events_for_all_tabs();
    let abandoned =
        LastCommand { ended_at: prompt, duration: Some(Duration::from_secs(3)), abandoned: true };
    assert_eq!(
        state_of(&app, pane),
        PaneCommandState { phase: CommandPhase::Prompt, last: Some(abandoned) }
    );
    app.__test_push_pane_command_event(pane, CommandEvent::PromptStart, prompt, None);
    app.__test_push_pane_command_event(pane, CommandEvent::CmdEnd(None), prompt, None);
    app.poll_command_events_for_all_tabs();
    let bare = LastCommand { ended_at: prompt, duration: None, abandoned: false };
    assert_eq!(state_of(&app, pane).last, Some(bare), "D without C has no duration");
}

/// An overflowed queue resets the state before its retained tail, so a tail that starts after a
/// lost `C` never leaves a stale `Running`; the flag clears with the drain.
#[test]
fn overflow_resets_command_state_before_the_tail() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("shell");
    let at = Instant::now();
    app.__test_push_pane_command_event(pane, CommandEvent::CmdStart, at, None);
    app.poll_command_events_for_all_tabs();
    assert!(matches!(state_of(&app, pane).phase, CommandPhase::Running { .. }));
    let burst = (0..=MAX_PANE_COMMAND_EVENTS).map(|_| PaneCommandEvent {
        event: CommandEvent::CmdEnd(None),
        at,
        duration: None,
    });
    {
        let main = app.main().unwrap();
        let mut queue = main.panes[&pane].command_events.lock();
        append_bounded_command_events(&mut queue, burst);
        assert!(queue.overflowed);
    }
    app.poll_command_events_for_all_tabs();
    let state = state_of(&app, pane);
    assert_eq!(state.phase, CommandPhase::Prompt, "the tail's D, applied after the reset");
    assert_eq!(state.last.map(|last| last.duration), Some(None));
    assert!(!app.main().unwrap().panes[&pane].command_events.lock().overflowed);
    {
        let main = app.main().unwrap();
        let mut queue = main.panes[&pane].command_events.lock();
        let tail = (0..=MAX_PANE_COMMAND_EVENTS).map(|_| PaneCommandEvent {
            event: CommandEvent::PromptEnd,
            at,
            duration: None,
        });
        append_bounded_command_events(&mut queue, tail);
    }
    app.poll_command_events_for_all_tabs();
    assert_eq!(state_of(&app, pane), PaneCommandState { phase: CommandPhase::Prompt, last: None });
}

/// A background tab's pane keeps its own state in a torn-out window, and each pane's state is its own.
#[test]
fn background_and_torn_out_panes_keep_their_command_state() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["front", "back"]);
    let panes = app.__test_child_pane_ids(child).unwrap();
    let (front, back) = (panes[0], panes[1]);
    let at = Instant::now();
    for (pane, event) in [(front, CommandEvent::PromptEnd), (back, CommandEvent::CmdStart)] {
        app.windows[&child].panes[&pane].command_events.lock().push(PaneCommandEvent {
            event,
            at,
            duration: None,
        });
    }
    let config = app.config.clone();
    poll_command_events_for_child_window(app.windows.get_mut(&child).unwrap(), &config);
    let window = &app.windows[&child];
    assert_eq!(window.panes[&front].command_state.phase, CommandPhase::Prompt);
    assert_eq!(window.panes[&back].command_state.phase, CommandPhase::Running { started_at: at });
}

/// A pane whose process exited has no confirmed prompt; its last command is kept.
#[test]
fn pane_exit_makes_the_phase_unknown() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("shell");
    let at = Instant::now();
    app.__test_push_pane_command_event(pane, CommandEvent::CmdEnd(None), at, None);
    app.poll_command_events_for_all_tabs();
    assert_eq!(state_of(&app, pane).phase, CommandPhase::Prompt);
    app.handle_pane_process_exited(pane, sonicterm_types::PaneExitStatus::Code(1));
    let state = state_of(&app, pane);
    assert_eq!(state.phase, CommandPhase::Unknown);
    assert!(state.last.is_some());
}
