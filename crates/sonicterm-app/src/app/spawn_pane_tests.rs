use sonicterm_grid::grid::Grid;
use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::MediaProtocol;

use super::*;

#[test]
fn reply_bursts_preserve_every_byte_and_release_parser_before_delivery() {
    // More queries than either old queue could hold must arrive in order without holding pane locks.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let mut expected = Vec::new();
    let mut input = Vec::new();
    for row in 1..=24 {
        for col in 1..=80 {
            input.extend_from_slice(format!("\x1b[{row};{col}H\x1b[6n").as_bytes());
            expected.extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
        }
    }
    input.extend_from_slice(b"done");
    let mut delivered = Vec::new();
    let mut submissions = 0;
    process_pane_vt_batch_with(
        &handles,
        &input,
        &mut None,
        |_| None,
        |_| {},
        Instant::now,
        |bytes| {
            assert!(handles.parser.try_lock().is_some());
            assert!(handles.inline_images.try_lock().is_some());
            assert!(handles.command_events.try_lock().is_some());
            submissions += 1;
            assert!(bytes.len() <= 32 * 1024);
            delivered.extend(bytes);
        },
    );
    assert_eq!(delivered, expected);
    assert_eq!(submissions, 1, "small replies share one bounded submission per output batch");
}

#[test]
fn reply_spool_admission_leaves_parser_available_with_visible_output_applied() {
    // A storage operation must leave rendering and resizing access to the already-updated grid.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let parser = handles.parser.clone();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (resume_tx, resume_rx) = crossbeam_channel::bounded(1);
    let worker = std::thread::spawn(move || {
        process_pane_vt_batch_with(
            &handles,
            b"\x1b[6nX",
            &mut None,
            |_| None,
            |_| {},
            Instant::now,
            |bytes| {
                entered_tx.send(bytes).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            },
        );
    });
    let reply = entered_rx.recv_timeout(Duration::from_secs(3));
    let available = parser.try_lock().map(|guard| guard.grid().cursor.col);
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(reply.unwrap(), b"\x1b[1;1R");
    assert_eq!(available, Some(1));
    assert_eq!(parser.lock().grid().row(0)[0].ch, 'X');
}

#[test]
fn reply_delivery_failure_does_not_abandon_visible_output() {
    // Input failure must not end the sole output/exit observer or discard already-buffered visible output.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let mut failed_reply = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b[6nX",
        &mut None,
        |_| None,
        |_| {},
        Instant::now,
        |bytes| {
            failed_reply = Some(bytes);
        },
    );
    assert_eq!(failed_reply.unwrap(), b"\x1b[1;1R");
    assert_eq!(handles.parser.lock().grid().row(0)[0].ch, 'X');
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

/// Both main spawn routes must pass the active pane's OSC 7 directory to the PTY boundary.
#[test]
fn main_tab_and_split_inherit_exact_active_pane_cwd() {
    for split in [false, true] {
        let mut app = app_with_unavailable_shell();
        let pane = app.__test_seed_tab("source");
        app.__test_seed_tab("unrelated tab");
        app.main_mut().unwrap().tabs.activate(0);
        let child = app.__test_seed_child_window(&["unrelated window"]);
        app.frontmost_window = Some(child);
        let native = if cfg!(windows) { "/C:/work/source" } else { "/work/source" };
        app.__test_advance_pane_parser(
            pane,
            format!("\x1b]7;file://localhost{native}\x07").as_bytes(),
        );
        if split {
            app.split_active(Direction::Right);
        } else {
            app.new_tab("new");
        }
        let expected = if cfg!(windows) { r"C:\work\source" } else { "/work/source" };
        let launches = app.test_pane_launches.borrow();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].1.cwd, Some(std::path::PathBuf::from(expected)));
    }
}

/// Finishing a prompt must not start execution timing while the user edits the command.
#[test]
fn prompt_end_does_not_start_command_timer() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let mut started = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b]133;B\x07",
        &mut started,
        |_| None,
        |_| {},
        Instant::now,
        |_| {},
    );
    assert_eq!(started, None);
}

/// Execution duration starts at C, not B, and B/D without execution has no duration.
#[test]
fn command_duration_excludes_prompt_editing_time() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (_pane, handles) = pane_and_worker_handles();
    let base = Instant::now();
    let mut times =
        [0, 60, 63, 64, 70].into_iter().map(|seconds| base + Duration::from_secs(seconds));
    let mut started = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;B\x07\x1b]133;D;0\x07",
        &mut started,
        |_| None,
        |_| {},
        || times.next().unwrap(),
        |_| {},
    );
    let events = handles.command_events.lock();
    assert_eq!(events[0].event, CommandEvent::PromptEnd);
    assert_eq!(events[1].event, CommandEvent::CmdStart);
    assert_eq!(events[2].duration, Some(Duration::from_secs(3)));
    assert_eq!(events[4].duration, None);
    assert_eq!(started, None);
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
        "\x1b_Gf=100,a=T;image\x1b\\\x1b[?25l\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m\x1b[>1u\x1b]52;c;{payload}\x1b\\\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\"
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
        |_| {},
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
