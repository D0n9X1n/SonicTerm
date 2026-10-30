//! Pane launch refusal, ownership and command contracts.

use super::super::spawn_pane_tests::*;
use super::*;

/// READONLY affects new user gestures, not cursor-position replies sent through the production parser batch and reply spool.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_readonly_parser_reply_uses_production_spool() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::{isolated, phase},
    };
    use sonicterm_ui::copy_mode::CopyModeState;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let observed = PtySubmissions::start();
    for (window, pane_id) in windows {
        app.windows.get_mut(&window).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
        assert!(!app.admits_new_user_input(pane_id));
        let pane = &app.windows[&window].panes[&pane_id];
        let pty = pane.pty.as_ref().unwrap();
        let replies = pty.reply_sender();
        let handles = PaneVtHandles::from_pane_state(pane);
        let before = pty.input_diagnostics().completed_messages;
        let mut submitted = Vec::new();
        phase(pane_id, "parser-query");
        process_pane_vt_batch_and_publish(
            &handles,
            b"\x1b[6n",
            None,
            &mut None,
            &mut SyncLatch::default(),
            None,
            |bytes| {
                assert!(
                    handles.parser.try_lock().is_some(),
                    "reply writes must release the parser first"
                );
                let actual = bytes.clone();
                replies.send(bytes).expect("production reply spool must accept cursor reply");
                submitted.push(actual);
            },
        );
        assert_eq!(submitted, vec![b"\x1b[1;1R".to_vec()]);
        phase(pane_id, "reply-writer-completion");
        let deadline = Instant::now() + Duration::from_secs(3);
        while pty.input_diagnostics().completed_messages == before && Instant::now() < deadline {
            while pty.out_rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            pty.input_diagnostics().completed_messages,
            before + 1,
            "pane {pane_id} reply writer did not complete"
        );
        assert!(observed.take().is_empty(), "parser replies never become user-input admissions");
    }
}

#[test]
fn reply_bursts_preserve_every_byte_and_release_parser_before_delivery() {
    // More queries than either old queue could hold must arrive in order without holding pane locks.
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
        None,
        &mut None,
        &mut SyncLatch::default(),
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
    let (_pane, handles) = pane_and_worker_handles();
    let parser = handles.parser.clone();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (resume_tx, resume_rx) = crossbeam_channel::bounded(1);
    let worker = std::thread::spawn(move || {
        process_pane_vt_batch_with(
            &handles,
            b"\x1b[6nX",
            None,
            &mut None,
            &mut SyncLatch::default(),
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
    let (_pane, handles) = pane_and_worker_handles();
    let mut failed_reply = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b[6nX",
        None,
        &mut None,
        &mut SyncLatch::default(),
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
    let (_pane, handles) = pane_and_worker_handles();
    let mut started = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b]133;B\x07",
        None,
        &mut started,
        &mut SyncLatch::default(),
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
    let (_pane, handles) = pane_and_worker_handles();
    let base = Instant::now();
    let mut times =
        [0, 60, 63, 64, 70].into_iter().map(|seconds| base + Duration::from_secs(seconds));
    let mut started = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;B\x07\x1b]133;D;0\x07",
        None,
        &mut started,
        &mut SyncLatch::default(),
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

/// The app-side VT dispatcher must process every host-owned event after releasing the parser.
#[test]
fn pane_vt_batch_routes_clipboard_commands_media_and_modes_after_unlock() {
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
        None,
        &mut command_started,
        &mut SyncLatch::default(),
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
                kitty: None,
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
    // One atomic publication keeps the keyboard modes, Kitty flags, and routing epoch coherent.
    let snapshot = handles.keyboard_input.load(Ordering::Relaxed);
    assert_eq!(snapshot, parser.lock().keyboard_input_snapshot());
    assert_eq!((snapshot >> 8) as u8, 1);
    let keyboard_modes = sonicterm_vt::vt::KeyboardModes::from_bits(snapshot as u8);
    assert!(keyboard_modes.application_cursor_keys());
    assert!(keyboard_modes.application_keypad());
    assert!(keyboard_modes.backarrow_key());
    assert!(keyboard_modes.newline());
    assert_eq!(keyboard_modes.modify_other_keys(), 2);
    let attempts =
        crate::app::spawn_pane::ClipAttempts { total: 1, accepted: 1, refused: 0, latest: None };
    assert_eq!(
        emitted,
        [
            UserEvent::ClipboardWrite { pane_id: 0, text: "copied".into() },
            UserEvent::ClipboardAttempts { pane_id: 0, attempts },
        ]
    );
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

#[test]
fn pane_keyboard_snapshot_initializes_from_existing_parser_state() {
    // A pane attached to an already-negotiated parser must not briefly expose default keyboard flags.
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b[?9001h\x1b[?1h");
    let expected = parser.keyboard_input_snapshot();
    let pane = PaneState::new(Arc::new(Mutex::new(parser)), None);
    assert_eq!(pane.keyboard_input.load(Ordering::Relaxed), expected);
}

#[test]
fn parser_test_input_publishes_negotiated_keyboard_snapshot() {
    // Native input integration feeds genuine child output through the same coherent negotiation snapshot.
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    let pane_id = app.__test_seed_tab("native-negotiation");
    assert!(app.__test_advance_pane_parser(pane_id, b"\x1b[?9001h"));
    let pane = app.pane_by_id(pane_id).unwrap();
    assert_eq!(
        pane.keyboard_input.load(Ordering::Relaxed),
        pane.parser.lock().keyboard_input_snapshot()
    );
}

/// A VT batch publishes the pointer-routing byte after it parses, so a pointer handler
/// reads tracking, SGR, alternate-screen and DECCKM state the batch set without locking.
#[test]
fn pane_vt_batch_publishes_pointer_modes_after_each_parse() {
    let (_pane, handles) = pane_and_worker_handles();
    let mut command_started = None;
    process_pane_vt_batch_with(
        &handles,
        b"\x1b[?1002h\x1b[?1006h\x1b[?1049h\x1b[?1h",
        None,
        &mut command_started,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        Instant::now,
        |_| {},
    );
    let published = handles.pointer_input.load(Ordering::Relaxed);
    assert_eq!(published, handles.parser.lock().pointer_input_snapshot());
    let modes = sonicterm_vt::vt::PointerModes::from_bits(published);
    assert_eq!(modes.tracking(), sonicterm_vt::vt::MouseTracking::ButtonMotion);
    assert!(modes.sgr() && modes.is_alt() && modes.application_cursor());

    // A later batch that resets the modes republishes them; the byte never keeps a stale mode.
    process_pane_vt_batch_with(
        &handles,
        b"\x1b[?1002l\x1b[?1049l",
        None,
        &mut command_started,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        Instant::now,
        |_| {},
    );
    let modes =
        sonicterm_vt::vt::PointerModes::from_bits(handles.pointer_input.load(Ordering::Relaxed));
    assert_eq!(modes.tracking(), sonicterm_vt::vt::MouseTracking::Off);
    assert!(modes.sgr() && !modes.is_alt() && modes.application_cursor());
}

/// A pane attached to a parser that already negotiated mouse tracking starts with that
/// state published, so the first pointer event never routes with default modes.
#[test]
fn pane_pointer_snapshot_initializes_from_existing_parser_state() {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b[?1003h\x1b[?1006h");
    let expected = parser.pointer_input_snapshot();
    let pane = PaneState::new(Arc::new(Mutex::new(parser)), None);
    assert_eq!(pane.pointer_input.load(Ordering::Relaxed), expected);
    assert_eq!(pane.pointer_modes().tracking(), sonicterm_vt::vt::MouseTracking::AnyMotion);
}

/// The main and child parser test hooks publish the pointer byte as the VT worker does,
/// so integration tests that set modes through them drive the real pointer routes.
#[test]
fn parser_test_hooks_publish_pointer_snapshot_in_main_and_child() {
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    let pane_id = app.__test_seed_tab("pointer-main");
    assert!(app.__test_advance_pane_parser(pane_id, b"\x1b[?1000h"));
    let pane = app.pane_by_id(pane_id).unwrap();
    assert_eq!(pane.pointer_modes().tracking(), sonicterm_vt::vt::MouseTracking::Button);

    let child = app.__test_seed_child_window(&["pointer-child"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    assert!(app.__test_advance_child_pane_parser(child, child_pane, b"\x1b[?1003h\x1b[?1049h"));
    let modes = app.windows[&child].panes[&child_pane].pointer_modes();
    assert_eq!(modes.tracking(), sonicterm_vt::vt::MouseTracking::AnyMotion);
    assert!(modes.is_alt());
}

/// Worker handles derived from a completed pane must share every mutable store with that pane.
#[test]
fn pane_derived_worker_handles_share_every_store_with_the_pane() {
    let (pane, worker) = pane_and_worker_handles();

    assert!(Arc::ptr_eq(&worker.parser, &pane.parser));
    assert!(Arc::ptr_eq(&worker.redraw_target, &pane.redraw_target));
    assert!(Arc::ptr_eq(&worker.command_events, &pane.command_events));
    assert!(Arc::ptr_eq(&worker.inline_images, &pane.inline_images));
    assert!(Arc::ptr_eq(&worker.cursor_visible, &pane.cursor_visible));
    assert!(Arc::ptr_eq(&worker.keyboard_input, &pane.keyboard_input));
    assert!(Arc::ptr_eq(&worker.pointer_input, &pane.pointer_input));
    assert!(Arc::ptr_eq(&worker.inline_media_charge, &pane.inline_media_charge));
    assert!(Arc::ptr_eq(&worker.sync_word, &pane.sync_word));
    assert!(Arc::ptr_eq(&worker.sync_deadline_word, &pane.sync_deadline_word));
    assert!(Arc::ptr_eq(&worker.sync_resets, &pane.sync_resets));
}

/// The media pool outlives the app while a VT worker still holds a pane's
/// charge, and the charge returns its slot once the worker lets go.
#[test]
fn a_worker_keeps_the_media_pool_alive_after_the_app_closes() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = crate::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_inline_media_pool(pool.clone());
    let pane_id = app.__test_seed_tab("worker");
    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    let worker = PaneVtHandles::from_pane_state(&app.windows[&main].panes[&pane_id]);

    drop(app);
    assert_eq!(pool.live_charges(), 1, "the worker still holds the pane's charge");
    assert!(Arc::ptr_eq(worker.inline_media_charge.lock().pool(), &pool));

    drop(worker);
    assert_eq!(pool.live_charges(), 0, "the last holder returns the charge");
}

/// The production worker wrapper publishes only after complete batch side effects return and both locks are free.
#[test]
fn worker_output_generation_is_published_after_complete_nonempty_batches() {
    let (pane, handles) = pane_and_worker_handles();
    assert!(Arc::ptr_eq(&pane.output_generation, &handles.output_generation));
    let generation = Arc::clone(&handles.output_generation);
    let mut replies = 0;
    process_pane_vt_batch_and_publish(
        &handles,
        b"x\x1b[6n",
        None,
        &mut None,
        &mut SyncLatch::default(),
        None,
        |_| {
            replies += 1;
            assert_eq!(
                generation.load(Ordering::Acquire),
                0,
                "publication must follow reply dispatch"
            );
            assert!(handles.parser.try_lock().is_some());
            assert!(handles.inline_images.try_lock().is_some());
        },
    );
    assert_eq!(replies, 1);
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert!(handles.parser.try_lock().is_some());
    assert!(handles.inline_images.try_lock().is_some());
    process_pane_vt_batch_and_publish(
        &handles,
        b"",
        None,
        &mut None,
        &mut SyncLatch::default(),
        None,
        |_| {},
    );
    assert_eq!(generation.load(Ordering::Acquire), 1, "empty input is not output publication");
    process_pane_vt_batch_and_publish(
        &handles,
        b"y",
        None,
        &mut None,
        &mut SyncLatch::default(),
        None,
        |_| {},
    );
    assert_eq!(generation.load(Ordering::Acquire), 2);
}

/// Main and child spawn the same pane-owned worker, and publication precedes its coalesced redraw dispatch.
#[test]
fn worker_spawn_roles_publish_their_own_pane_not_an_app_global() {
    let source = concat!(
        include_str!("spawn_pane.rs"),
        "\n",
        include_str!("spawn_pane_commands.rs"),
        "\n",
        include_str!("spawn_pane_output.rs"),
        "\n",
        include_str!("spawn_pane_vt.rs")
    );
    let child = concat!(
        include_str!("child_window.rs"),
        include_str!("child_tabs.rs"),
        include_str!("splitter_input.rs"),
        include_str!("child_window_pointer.rs"),
        include_str!("child_window_redraw.rs")
    );
    assert!(!source.contains("pty_burst_gen") && !child.contains("pty_burst_gen"));
    assert!(source.contains("output_generation: pane.output_generation.clone()"));
    assert!(child.contains("super::spawn_pane::spawn_pane_workers("));
    let wrapper = source.find("fn process_pane_vt_batch_and_publish<").unwrap();
    assert!(source[wrapper..].contains("publish_pane_vt_batch_with("));
    let publisher = source.find("fn publish_pane_vt_batch_with<").unwrap();
    let parse = source[publisher..].find("process_pane_vt_batch_with(").unwrap();
    let publish = source[publisher..]
        .find("handles.output_generation.fetch_add(1, Ordering::Release)")
        .unwrap();
    assert!(parse < publish);
}

/// Accepted writes stay separate events in order, then one payload-free summary follows; a batch of
/// 1,000 reads writes nothing and sends exactly one summary, all naming the worker's pane.
#[test]
fn osc52_batch_keeps_each_write_and_sends_one_summary() {
    use base64::Engine as _;
    let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let two = format!("\x1b]52;c;{}\x07\x1b]52;c;{}\x07", encode("one"), encode("two"));
    let accepted = ClipAttempts { total: 2, accepted: 2, refused: 0, latest: None };
    assert_eq!(
        osc52_batch(two.as_bytes()),
        [
            UserEvent::ClipboardWrite { pane_id: 7, text: "one".into() },
            UserEvent::ClipboardWrite { pane_id: 7, text: "two".into() },
            UserEvent::ClipboardAttempts { pane_id: 7, attempts: accepted },
        ]
    );
    let reads = "\x1b]52;c;?\x07".repeat(1_000);
    let refused = ClipAttempts {
        total: 1_000,
        accepted: 0,
        refused: 1_000,
        latest: Some(Osc52Refusal::Read),
    };
    assert_eq!(
        osc52_batch(reads.as_bytes()),
        [UserEvent::ClipboardAttempts { pane_id: 7, attempts: refused }]
    );
    assert!(osc52_batch(b"plain output").is_empty(), "no OSC 52, no summary");
}

/// The output counter adds each received chunk's length only while `out` turned counting on.
#[test]
fn output_bytes_count_only_while_counting_is_on() {
    use std::sync::atomic::Ordering;
    let parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(10, 2));
    let pane = PaneState::new(Arc::new(Mutex::new(parser)), None);
    let worker = PaneVtHandles::from_pane_state(&pane);
    worker.count_output(100);
    assert_eq!(pane.output_bytes.load(Ordering::Relaxed), 0);
    pane.output_counting.store(true, Ordering::Relaxed);
    worker.count_output(100);
    worker.count_output(23);
    assert_eq!(pane.output_bytes.load(Ordering::Relaxed), 123);
}

#[test]
fn a_batch_reports_its_last_progress_and_clears_only_what_it_showed() {
    // A batch sends its latest progress once; a command end or RIS clears it only when this pane
    // showed progress, so ordinary commands send nothing.
    let (_pane, handles) = pane_and_worker_handles();
    let normal = |percent| {
        Some(sonicterm_vt::vt::ProgressReport {
            state: sonicterm_vt::vt::ProgressState::Normal,
            percent,
        })
    };
    let first = program_batch(&handles, "\x1b]9;4;1;10\x07\x1b]9;4;1;20\x07");
    assert_eq!(progress_reports(&first), vec![normal(20)], "only the batch's last value");
    let ended = program_batch(&handles, "\x1b]133;D;0\x07");
    assert_eq!(progress_reports(&ended), vec![None], "a finished command clears the bar");
    assert!(progress_reports(&program_batch(&handles, "\x1b]133;D;0\x07")).is_empty());
    program_batch(&handles, "\x1b]9;4;3\x07");
    assert_eq!(progress_reports(&program_batch(&handles, "\x1bc")), vec![None], "RIS clears it");
}

#[test]
fn a_batch_of_notifications_is_one_event_with_a_count() {
    // A burst in one batch reaches the App as its first notification and how many followed.
    let (_pane, handles) = pane_and_worker_handles();
    let events = program_batch(&handles, "\x1b]9;one\x07\x1b]9;two\x07\x1b]777;notify;T;three\x07");
    let notices: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            UserEvent::ProgramNotify { title, body, more, .. } => {
                Some((title.clone(), body.clone(), *more))
            }
            _ => None,
        })
        .collect();
    assert_eq!(notices, vec![(None, "one".to_owned(), 2)]);
}

/// OSC 9;4 progress and OSC 9 / OSC 777 notifications a real ConPTY child writes reach the pane as
/// typed events: ConPTY forwards them, and the worker turns them into the App's events. The child is
/// this test binary re-run under a marker; the read is bounded by a 30 s deadline.
#[cfg(windows)]
#[test]
fn conpty_child_program_oscs_reach_the_pane() {
    use crate::app::pty_test_support::isolated;
    use sonicterm_io::pty::{CommandSpawn, PtyHandle, ShellSpawnOpts};
    use sonicterm_vt::vt::{ProgressReport, ProgressState};
    let name = std::thread::current().name().expect("named unit test").to_owned();
    if std::env::var(PROGRAM_OSC_CHILD).is_ok_and(|value| value == name) {
        // This process is the ConPTY child: it writes the sequences and ends.
        write_program_oscs();
        return;
    }
    if isolated() {
        return;
    }
    // Only this isolated process runs, so setting its environment races no other test.
    std::env::set_var(PROGRAM_OSC_CHILD, &name);
    let spawn = CommandSpawn {
        program: std::env::current_exe().expect("test binary path").into_os_string(),
        args: [name.as_str(), "--exact", "--nocapture", "--test-threads=1"]
            .map(std::ffi::OsString::from)
            .into(),
        cwd: std::env::temp_dir(),
    };
    let pty = PtyHandle::spawn_command(&spawn, 120, 40, &ShellSpawnOpts::default())
        .expect("spawn the ConPTY child");
    let (_pane, handles) = pane_and_worker_handles();
    let replies = pty.reply_sender();
    let mut latch = SyncLatch::default();
    let mut progress = Vec::new();
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    // The clear is written last, so once it arrives every notification has too.
    while progress.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(chunk) = pty.out_rx.recv_timeout(remaining) else {
            panic!("no clear within 30 s: progress {progress:?}, notifications {notices:?}");
        };
        process_pane_vt_batch_with(
            &handles,
            &chunk,
            None,
            &mut None,
            &mut latch,
            |_| None,
            |event| match event {
                UserEvent::ProgramProgress { report, .. } => progress.push(report),
                UserEvent::ProgramNotify { title, body, more, .. } => {
                    notices.push((title, body, more));
                }
                // Output and other host events are not what this test waits for.
                _ => {}
            },
            Instant::now,
            |reply| {
                let _ = replies.send(reply);
            },
        );
    }
    let set = Some(ProgressReport { state: ProgressState::Normal, percent: 40 });
    assert_eq!(progress, vec![set, None]);
    let total: u32 = notices.iter().map(|(_, _, more)| more + 1).sum();
    assert_eq!(total, 2, "both notifications arrived: {notices:?}");
    assert_eq!(notices.first().map(|notice| notice.1.as_str()), Some("build done"));
    let ci_seen = notices.iter().any(|(title, ..)| title.as_deref() == Some("CI"));
    assert!(
        ci_seen || notices.len() == 1,
        "the OSC 777 title arrives unless coalesced: {notices:?}"
    );
}

/// A batch's first notification is the first one with text left after sanitizing: a zero-width
/// OSC 9 before `hello` must not hide `hello`, and a blank one is never counted.
#[test]
fn a_batch_keeps_the_first_notification_that_sanitizes_to_text() {
    let mut notices = ProgramNotices::default();
    notices.push(None, "\u{200B}".to_owned());
    notices.push(Some("\u{1b}".to_owned()), " \u{2028} ".to_owned());
    notices.push(None, "hello".to_owned());
    notices.push(None, "again".to_owned());
    assert_eq!(notices.into_parts(), Some((None, "hello".to_owned(), 1)));
    let mut blank = ProgramNotices::default();
    blank.push(None, "\u{200B}".to_owned());
    assert_eq!(blank.into_parts(), None, "a batch of blank notifications sends no event");
}
