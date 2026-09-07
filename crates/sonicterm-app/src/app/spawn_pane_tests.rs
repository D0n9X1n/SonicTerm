use super::*;

use sonicterm_grid::grid::Grid;
use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::MediaProtocol;

#[test]
fn repeated_reply_rejections_emit_one_event_and_preserve_totals() {
    // A stalled writer cannot amplify a fixed reply burst into an unbounded number of UI events.
    let (tx, rx) = crossbeam_channel::unbounded();
    for _ in 0..1000 {
        tx.send(vec![b'x'; 12]).unwrap();
    }
    drop(tx);
    let mut events = 0;
    let mut summaries = Vec::new();
    forward_pty_replies(
        rx,
        |bytes| Err(sonicterm_io::pty::PtyInputError::QueueFull(bytes)),
        |_| events += 1,
        |totals| summaries.push(totals),
    );
    assert_eq!(events, 1);
    assert_eq!(summaries.iter().map(|s| s.messages).sum::<u64>(), 999);
    assert_eq!(summaries.iter().map(|s| s.bytes).sum::<u64>(), 999 * 12);
    assert_eq!(summaries.iter().map(|s| s.queue_full).sum::<u64>(), 999);
    assert!(summaries.iter().all(|s| s.too_large == 0 && s.disconnected == 0));
}

#[test]
fn reply_forwarding_keeps_successes_and_rejection_reasons_distinct() {
    // Recovery preserves accepted reply order, while later refusal causes remain counted without another UI event.
    use sonicterm_io::pty::PtyInputError;
    let (tx, rx) = crossbeam_channel::unbounded();
    for marker in 0..6 {
        tx.send(vec![marker]).unwrap();
    }
    drop(tx);
    let mut accepted = Vec::new();
    let mut events = 0;
    let mut summaries = Vec::new();
    forward_pty_replies(
        rx,
        |bytes| match bytes[0] {
            0 => Err(PtyInputError::QueueFull(bytes)),
            2 => Err(PtyInputError::MessageTooLarge(bytes)),
            4 => Err(PtyInputError::WriterDisconnected(bytes)),
            _ => {
                accepted.push(bytes);
                Ok(())
            }
        },
        |_| events += 1,
        |totals| summaries.push(totals),
    );
    assert_eq!(accepted, [vec![1], vec![3]]);
    assert_eq!(events, 1);
    assert_eq!(summaries.iter().map(|s| s.messages).sum::<u64>(), 2);
    assert_eq!(summaries.iter().map(|s| s.bytes).sum::<u64>(), 2);
    assert_eq!(summaries.iter().map(|s| s.too_large).sum::<u64>(), 1);
    assert_eq!(summaries.iter().map(|s| s.disconnected).sum::<u64>(), 1);
}

#[test]
fn reply_rejection_summary_flushes_while_the_parser_is_idle() {
    // Pending counts must reach the log even when no later reply or channel close wakes the worker.
    let (tx, rx) = crossbeam_channel::bounded(2);
    let (event_tx, event_rx) = crossbeam_channel::bounded(1);
    let (summary_tx, summary_rx) = crossbeam_channel::bounded(1);
    let worker = std::thread::spawn(move || {
        forward_pty_replies(
            rx,
            |bytes| Err(sonicterm_io::pty::PtyInputError::QueueFull(bytes)),
            |_| event_tx.send(()).unwrap(),
            |totals| summary_tx.send(totals).unwrap(),
        );
    });
    tx.send(vec![b'x'; 12]).unwrap();
    tx.send(vec![b'x'; 13]).unwrap();
    event_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let summary = summary_rx.recv_timeout(Duration::from_secs(3));
    drop(tx);
    worker.join().unwrap();
    assert_eq!(
        summary.unwrap(),
        ReplyRejectionTotals { messages: 1, bytes: 13, queue_full: 1, ..Default::default() }
    );
    assert!(event_rx.try_recv().is_err());
}

fn app_with_unavailable_shell() -> App {
    // A child path below the test executable cannot launch a shell or read user shell profiles.
    let shell = std::env::current_exe().expect("test executable").join("unavailable-shell");
    let config = Config {
        terminal: sonicterm_cfg::config::TerminalConfig {
            shell: Some(shell.to_string_lossy().into_owned()),
            ..Default::default()
        },
        ..Config::default()
    };
    App::new(Theme::default(), config, Keymap::default())
}

#[test]
fn main_split_after_zoom_keeps_active_parser_visible_in_every_direction() {
    // Exercise the real main split path without launching a user shell; native runs cover live PTYs.
    for nested in [false, true] {
        for direction in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            let mut app = app_with_unavailable_shell();
            app.__test_seed_tab("main");
            let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
            assert!(app.__test_set_main_pane_viewport(outer, 10.0, 10.0));
            app.resize_visible_panes();
            if nested {
                app.split_active(Direction::Right);
            }
            let window = app.main().expect("main window");
            let tab = &window.tab_states[window.tabs.active_index()];
            let previous_active = tab.active_pane;
            let mut expected = tab.tree.clone();
            app.toggle_active_pane_zoom();
            assert_eq!(app.compute_active_pane_rects(), [(previous_active, outer)]);
            for pane in app.main().expect("main window").panes.values() {
                pane.parser.lock().grid_mut().clear_dirty();
            }

            app.split_active(direction);

            let window = app.main().expect("main window");
            let tab = &window.tab_states[window.tabs.active_index()];
            let active = tab.active_pane;
            assert_ne!(active, previous_active);
            assert!(expected.split(previous_active, direction, active));
            let rects = app.compute_active_pane_rects();
            assert_eq!(rects, expected.layout(outer), "nested={nested}, {direction:?}");
            assert_eq!(tab.tree.zoomed_pane_id(), None);
            assert_eq!(rects.iter().filter(|(id, _)| *id == active).count(), 1);
            let guards: Vec<_> = rects
                .iter()
                .map(|(id, rect)| {
                    let pane = window.panes.get(id).expect("visible pane is live");
                    (*id, pane.parser.try_lock().expect("coherent parser guard"), *rect)
                })
                .collect();
            assert!(guards.iter().any(|(id, _, _)| *id == active));
            for (id, parser, rect) in &guards {
                let grid = parser.grid();
                assert_eq!(
                    (grid.cols, grid.rows),
                    ((rect.w / 10.0) as u16, (rect.h / 10.0) as u16)
                );
                if *id == active || *id == previous_active {
                    assert!(grid.dirty_rows().count() > 0, "split participants must redraw");
                }
            }
            assert!(
                window.panes[&active].pty.is_none(),
                "shell failure does not refuse a valid split"
            );
        }
    }
}

#[test]
fn main_split_refusal_preserves_zoom_focus_and_live_panes() {
    // Invalid focus and missing live state must leave the pre-existing zoomed topology unchanged.
    for missing_live_pane in [false, true] {
        let mut app = app_with_unavailable_shell();
        let pane = app.__test_seed_tab("main");
        let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
        assert!(app.__test_set_main_pane_viewport(outer, 10.0, 10.0));
        app.toggle_active_pane_zoom();
        let window = app.main_mut().expect("main window");
        if missing_live_pane {
            window.panes.remove(&pane);
        } else {
            window.tab_states[0].active_pane = u64::MAX;
        }
        let active = window.tab_states[0].active_pane;
        let mut panes_before: Vec<_> = window.panes.keys().copied().collect();
        panes_before.sort_unstable();
        let layout_before = window.tab_states[0].tree.layout(outer);

        app.split_active(Direction::Right);

        let window = app.main().expect("main window");
        let tab = &window.tab_states[0];
        let mut panes_after: Vec<_> = window.panes.keys().copied().collect();
        panes_after.sort_unstable();
        assert_eq!(panes_after, panes_before);
        assert_eq!(tab.active_pane, active);
        assert_eq!(tab.tree.leaves(), [pane]);
        assert_eq!(tab.tree.zoomed_pane_id(), Some(pane));
        assert_eq!(tab.tree.layout(outer), layout_before);
    }
}

#[test]
fn main_split_without_window_or_tab_leaves_topology_empty() {
    // Missing destinations are no-ops rather than installing a pane outside a tab tree.
    let mut app = app_with_unavailable_shell();
    app.split_active(Direction::Down);
    assert!(app.main_window_id.is_none());
    assert!(app.windows.is_empty());

    app.__test_synthetic_main();
    app.split_active(Direction::Down);
    let window = app.main().expect("synthetic main");
    assert!(window.tab_states.is_empty());
    assert!(window.panes.is_empty());
}

fn pane_and_worker_handles() -> (PaneState, PaneVtHandles) {
    let pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);
    let worker = PaneVtHandles::from_pane_state(&pane);
    (pane, worker)
}

/// The app-side VT dispatcher must process every host-owned event after releasing the parser.
#[test]
fn pane_vt_batch_routes_clipboard_commands_media_and_modes_after_unlock() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let parser = handles.parser.clone();
    let base = Instant::now();
    let mut ticks = [base, base + Duration::from_secs(7)].into_iter();
    let mut command_started = None;
    let mut emitted = Vec::new();
    let mut decoder_unlocked = None;
    let payload = base64::engine::general_purpose::STANDARD.encode("copied");
    let bytes = format!(
        "\x1b_Gf=100,a=T;image\x1b\\\x1b[?25l\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m\x1b[>1u\x1b]52;c;{payload}\x1b\\\x1b]133;B\x1b\\\x1b]133;D;0\x1b\\"
    );

    process_pane_vt_batch_with(
        &handles,
        bytes.as_bytes(),
        &mut command_started,
        |media| {
            decoder_unlocked = Some(parser.try_lock().is_some());
            assert_eq!(media.protocol, MediaProtocol::Kitty);
            Some(InlineImage {
                id: 1,
                row: media.row,
                col: media.col,
                width: 1,
                height: 1,
                bgra: Arc::from([1, 2, 3, 255]),
            })
        },
        |event| emitted.push(event),
        || ticks.next().expect("one timestamp per command marker"),
    );

    assert_eq!(
        decoder_unlocked,
        Some(true),
        "a media event must reach decoding only after the parser guard is released"
    );
    assert!(!handles.cursor_visible.load(Ordering::Relaxed));
    assert_eq!(handles.kitty_flags.load(Ordering::Relaxed), 1);
    let keyboard_modes =
        sonicterm_vt::vt::KeyboardModes::from_bits(handles.keyboard_modes.load(Ordering::Relaxed));
    assert!(keyboard_modes.application_cursor_keys());
    assert!(keyboard_modes.application_keypad());
    assert!(keyboard_modes.backarrow_key());
    assert!(keyboard_modes.newline());
    assert_eq!(keyboard_modes.modify_other_keys(), 2);
    assert_eq!(emitted, [UserEvent::ClipboardWrite { text: "copied".into() }]);
    let commands = handles.command_events.lock();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].event, CommandEvent::CmdStart);
    assert_eq!(commands[1].event, CommandEvent::CmdEnd(Some(0)));
    assert_eq!(commands[1].duration, Some(Duration::from_secs(7)));
    drop(commands);
    let images = handles.inline_images.lock();
    assert_eq!(images.len(), 1);
    assert_eq!(&*images[0].bgra, &[1, 2, 3, 255]);
}

/// Worker handles derived from a completed pane must share every mutable store with that pane.
#[test]
fn pane_derived_worker_handles_share_every_store_with_the_pane() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (pane, worker) = pane_and_worker_handles();

    assert!(Arc::ptr_eq(&worker.parser, &pane.parser));
    assert!(Arc::ptr_eq(&worker.redraw_target, &pane.redraw_target));
    assert!(Arc::ptr_eq(&worker.command_events, &pane.command_events));
    assert!(Arc::ptr_eq(&worker.inline_images, &pane.inline_images));
    assert!(Arc::ptr_eq(&worker.cursor_visible, &pane.cursor_visible));
    assert!(Arc::ptr_eq(&worker.kitty_flags, &pane.kitty_flags));
    assert!(Arc::ptr_eq(&worker.keyboard_modes, &pane.keyboard_modes));
    assert!(Arc::ptr_eq(&worker.inline_media_charge, &pane.inline_media_charge));
}
