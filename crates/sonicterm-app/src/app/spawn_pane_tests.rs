use sonicterm_grid::grid::Grid;
use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::{CaptureStagingPool, MediaProtocol};

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
        process_pane_vt_batch(&handles, b"\x1b[6n", &mut None, None, |bytes| {
            assert!(
                handles.parser.try_lock().is_some(),
                "reply writes must release the parser first"
            );
            let actual = bytes.clone();
            replies.send(bytes).expect("production reply spool must accept cursor reply");
            submitted.push(actual);
        });
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

/// A pane and its worker handles that stage captures and trim media in private
/// pools, so a batch that opens a capture is admitted whatever sibling tests hold.
fn pane_and_worker_handles() -> (PaneState, PaneVtHandles) {
    let parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    let pane = PaneState::new_with_media_pool(
        Arc::new(Mutex::new(parser)),
        None,
        &crate::app::media::InlineMediaPool::new(),
    );
    let worker = PaneVtHandles::from_pane_state(&pane);
    (pane, worker)
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
        &mut command_started,
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
        &mut command_started,
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
    process_pane_vt_batch_and_publish(&handles, b"x\x1b[6n", &mut None, None, |_| {
        replies += 1;
        assert_eq!(generation.load(Ordering::Acquire), 0, "publication must follow reply dispatch");
        assert!(handles.parser.try_lock().is_some());
        assert!(handles.inline_images.try_lock().is_some());
    });
    assert_eq!(replies, 1);
    assert_eq!(generation.load(Ordering::Acquire), 1);
    assert!(handles.parser.try_lock().is_some());
    assert!(handles.inline_images.try_lock().is_some());
    process_pane_vt_batch_and_publish(&handles, b"", &mut None, None, |_| {});
    assert_eq!(generation.load(Ordering::Acquire), 1, "empty input is not output publication");
    process_pane_vt_batch_and_publish(&handles, b"y", &mut None, None, |_| {});
    assert_eq!(generation.load(Ordering::Acquire), 2);
}

/// Main and child spawn the same pane-owned worker, and publication precedes its coalesced redraw dispatch.
#[test]
fn worker_spawn_roles_publish_their_own_pane_not_an_app_global() {
    let source = include_str!("spawn_pane.rs");
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
    let parse = source[wrapper..].find("process_pane_vt_batch(handles, bytes,").unwrap();
    let publish = source[wrapper..]
        .find("handles.output_generation.fetch_add(1, Ordering::Release)")
        .unwrap();
    assert!(parse < publish);
}

/// A pane worker whose pane counts into `stats`.
fn counting_worker_handles(
    stats: &Arc<crate::app::frame_counters::VtFrameStats>,
) -> (PaneState, PaneVtHandles) {
    let parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    let mut pane = PaneState::new_with_media_pool(
        Arc::new(Mutex::new(parser)),
        None,
        &crate::app::media::InlineMediaPool::new(),
    );
    pane.frame_counters =
        Some(crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(stats)));
    let worker = PaneVtHandles::from_pane_state(&pane);
    (pane, worker)
}

/// A counting worker reads its clock once before `lock()` and three times under the guard, and
/// records wait, parse and hold from those instants once the guard has dropped.
#[test]
fn counting_worker_times_each_parser_section_from_four_clock_reads() {
    use std::sync::atomic::Ordering::Relaxed;
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let start = Instant::now();
    let mut clock_reads = 0_u64;
    let clock = || {
        clock_reads += 1;
        start + Duration::from_micros(clock_reads * 10)
    };
    process_pane_vt_batch_with(&handles, b"hello", &mut None, |_| None, |_| {}, clock, |_| {});
    assert_eq!(clock_reads, 4, "one read before lock() and three under the guard");
    let (wait, parse, hold) = (
        stats.parser_lock_wait.snapshot(),
        stats.parse.snapshot(),
        stats.parser_lock_hold.snapshot(),
    );
    assert_eq!((wait.sum_us(), parse.sum_us(), hold.sum_us()), (10, 10, 20));
    assert_eq!((stats.batches.load(Relaxed), stats.parse_bytes.load(Relaxed)), (1, 5));
}

/// With its App's gate off a worker reads no clock for a plain batch.
#[test]
fn worker_without_counters_reads_no_clock() {
    let (_pane, handles) = pane_and_worker_handles();
    assert!(handles.frame_counters.is_none());
    let mut clock_reads = 0_u32;
    let clock = || {
        clock_reads += 1;
        Instant::now()
    };
    process_pane_vt_batch_with(&handles, b"hello", &mut None, |_| None, |_| {}, clock, |_| {});
    assert_eq!(clock_reads, 0);
}

/// A targeted flush is published before its output event is sent, a second one before the
/// redraw only coalesces, an untargeted one stores nothing, and without counters the send is
/// unchanged. The token is cleared between sends, as servicing each event would.
#[test]
fn output_redraw_publishes_the_flush_before_sending() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let target = Mutex::new(Some(7_u32));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    send_output_redraw(&target, &outstanding, Some(&counters), |window| {
        assert_ne!(counters.pending_flush.load(Acquire), 0, "published before the send");
        sent.push(window);
        true
    });
    outstanding.store(false, Relaxed);
    send_output_redraw(&target, &outstanding, Some(&counters), |window| {
        sent.push(window);
        true
    });
    assert_eq!((stats.flushes.load(Relaxed), stats.flushes_coalesced.load(Relaxed)), (2, 1));
    assert_ne!(counters.pending_flush.swap(0, Acquire), 0);
    let untargeted = Mutex::new(None::<u32>);
    outstanding.store(false, Relaxed);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), |window| {
        sent.push(window);
        true
    });
    assert_eq!(counters.pending_flush.load(Relaxed), 0, "nothing stored without a target");
    assert_eq!(stats.flushes_untargeted.load(Relaxed), 1);
    send_output_redraw(&target, &outstanding, None, |window| {
        sent.push(window);
        true
    });
    assert_eq!(sent, [7, 7, 7]);
}

/// N targeted flushes before their event is serviced send one event and count N-1 as
/// suppressed while every flush still counts; the oldest pending timestamp stays; an untargeted
/// flush counts as untargeted, leaves the token alone and is never counted as suppressed.
#[test]
fn outstanding_output_event_coalesces_later_flushes() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let target = Mutex::new(Some(7_u32));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    let flush_count = 5_u64;
    let mut first_published = 0;
    for flush in 0..flush_count {
        send_output_redraw(&target, &outstanding, Some(&counters), |window| {
            sent.push(window);
            true
        });
        if flush == 0 {
            first_published = counters.pending_flush.load(Acquire);
        }
    }
    assert_eq!(sent, [7], "one event while it is outstanding");
    assert!(outstanding.load(Acquire), "the token stays set until the event is serviced");
    assert_eq!(stats.flushes.load(Relaxed), flush_count);
    assert_eq!(stats.flushes_suppressed.load(Relaxed), flush_count - 1);
    assert_eq!(counters.pending_flush.load(Acquire), first_published, "oldest flush kept");

    let untargeted = Mutex::new(None::<u32>);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), |window| {
        sent.push(window);
        true
    });
    assert_eq!(stats.flushes_untargeted.load(Relaxed), 1);
    assert_eq!(
        stats.flushes_suppressed.load(Relaxed),
        flush_count - 1,
        "untargeted is not suppressed"
    );
    assert!(outstanding.load(Acquire), "an untargeted flush leaves the token");
    outstanding.store(false, Relaxed);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), |window| {
        sent.push(window);
        true
    });
    assert!(!outstanding.load(Acquire), "an untargeted flush never sets the token");
    assert_eq!(sent, [7]);
}

/// A send the event loop refuses (it has gone) clears the token and is not counted as
/// suppressed, so the next flush sends again; a delivered send leaves the token set.
#[test]
fn failed_output_send_clears_the_token_and_the_next_flush_sends() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let target = Mutex::new(Some(7_u32));
    let outstanding = AtomicBool::new(false);
    let mut attempts = Vec::new();
    send_output_redraw(&target, &outstanding, Some(&counters), |window| {
        attempts.push(window);
        false
    });
    assert!(!outstanding.load(Acquire), "a failed send releases the token");
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 0, "a failed send is not suppressed");
    send_output_redraw(&target, &outstanding, Some(&counters), |window| {
        attempts.push(window);
        true
    });
    assert_eq!(attempts, [7, 7], "the next flush sends");
    assert!(outstanding.load(Acquire), "a delivered send holds the token");
    send_output_redraw(&target, &outstanding, Some(&counters), |window| {
        attempts.push(window);
        true
    });
    assert_eq!(attempts, [7, 7]);
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 1);
}

/// With the App's gate off a worker still coalesces through the token, but reads no flush clock;
/// with it on, every targeted flush reads the clock once, sent or suppressed.
#[test]
fn gate_off_output_events_still_coalesce_without_reading_the_flush_clock() {
    use crate::app::frame_counters::flush_clock_reads;
    let target = Mutex::new(Some(7_u32));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    let before = flush_clock_reads();
    for _ in 0..3 {
        send_output_redraw(&target, &outstanding, None, |window| {
            sent.push(window);
            true
        });
    }
    assert_eq!(sent, [7], "the token coalesces with the gate off too");
    assert_eq!(flush_clock_reads(), before, "no clock read with the gate off");

    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let gate_on = AtomicBool::new(false);
    let before = flush_clock_reads();
    for _ in 0..3 {
        send_output_redraw(&target, &gate_on, Some(&counters), |_| true);
    }
    assert_eq!(flush_clock_reads(), before + 3, "one read per targeted flush with the gate on");
}

/// One interior section boundary of a worker batch: the bytes still unparsed, and whether
/// the parser's mutex was free when the boundary hook ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionBoundary {
    remaining_bytes: usize,
    parser_unlocked: bool,
}

/// Run `parse` with a boundary hook that `try_lock`s `parser` on this thread and records each
/// interior boundary; the hook is removed again before returning.
fn record_section_boundaries(
    parser: &Arc<Mutex<Parser>>,
    parse: impl FnOnce(),
) -> Vec<SectionBoundary> {
    let boundaries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = std::rc::Rc::clone(&boundaries);
    let observed_parser = Arc::clone(parser);
    set_section_boundary_hook(Some(Box::new(move |remaining_bytes| {
        let parser_unlocked = observed_parser.try_lock().is_some();
        recorded.borrow_mut().push(SectionBoundary { remaining_bytes, parser_unlocked });
    })));
    parse();
    set_section_boundary_hook(None);
    let boundaries = boundaries.borrow().clone();
    boundaries
}

/// Printable ASCII rows ending in CR LF, cut to exactly `batch_bytes` bytes.
fn plain_text_batch(batch_bytes: usize) -> Vec<u8> {
    let mut batch = Vec::with_capacity(batch_bytes + 80);
    let mut row_index = 0_usize;
    while batch.len() < batch_bytes {
        batch.extend_from_slice(format!("row {row_index:05} {}\r\n", "x".repeat(60)).as_bytes());
        row_index += 1;
    }
    batch.truncate(batch_bytes);
    batch
}

/// A long reply-free batch is parsed in sections of at most `PARSER_SECTION_BYTES`, so no
/// single parser lock hold covers the whole batch.
#[test]
fn long_batch_is_parsed_in_bounded_sections() {
    use std::sync::atomic::Ordering::Relaxed;
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let batch_bytes = 20 * 1024;
    let batch = plain_text_batch(batch_bytes);
    let boundaries = record_section_boundaries(&handles.parser, || {
        process_pane_vt_batch_with(
            &handles,
            &batch,
            &mut None,
            |_| None,
            |_| {},
            Instant::now,
            |_| {},
        );
    });
    // Section lengths are the drops in remaining bytes from the batch start to its end.
    let mut section_starts = vec![batch_bytes];
    section_starts.extend(boundaries.iter().map(|boundary| boundary.remaining_bytes));
    section_starts.push(0);
    let section_lengths: Vec<usize> =
        section_starts.windows(2).map(|pair| pair[0] - pair[1]).collect();
    assert!(section_lengths.len() >= 5, "sections: {section_lengths:?}");
    assert!(
        section_lengths.iter().all(|length| *length <= PARSER_SECTION_BYTES),
        "every section is bounded: {section_lengths:?}"
    );
    let sections_counted = stats.parse.snapshot().count();
    assert_eq!(sections_counted, section_lengths.len() as u64);
    assert_eq!(stats.parse_bytes.load(Relaxed), batch_bytes as u64);
    assert_eq!(stats.batches.load(Relaxed), 1, "one batch however many sections it takes");
}

/// At every interior section boundary of a long batch the worker has dropped the parser
/// guard while input remains, so another thread's `try_lock` could take the parser there.
#[test]
fn parser_guard_is_released_between_sections_while_input_remains() {
    let (_pane, handles) = pane_and_worker_handles();
    let batch_bytes = 20 * 1024;
    let batch = plain_text_batch(batch_bytes);
    let boundaries = record_section_boundaries(&handles.parser, || {
        process_pane_vt_batch_with(
            &handles,
            &batch,
            &mut None,
            |_| None,
            |_| {},
            Instant::now,
            |_| {},
        );
    });
    // A reply-free batch is cut only by the bound, at each multiple of the section size.
    let expected: Vec<SectionBoundary> = (1..batch_bytes.div_ceil(PARSER_SECTION_BYTES))
        .map(|sections_done| SectionBoundary {
            remaining_bytes: batch_bytes - sections_done * PARSER_SECTION_BYTES,
            parser_unlocked: true,
        })
        .collect();
    assert_eq!(boundaries, expected);
    assert!(boundaries.iter().all(|boundary| boundary.remaining_bytes > 0));
}

/// Cursor-position queries spread across many sections, several cut mid-sequence by the
/// bound, are each answered once, in query order, in one reply submission after the batch.
#[test]
fn replies_staged_across_bounded_sections_are_sent_once_in_order() {
    let (_pane, handles) = pane_and_worker_handles();
    let mut batch = Vec::new();
    let mut expected_replies = Vec::new();
    let mut query_ranges = Vec::new();
    // A reply ends a section, so the next section starts right after each query; filler of
    // `PARSER_SECTION_BYTES - cut_offset` puts the next bound `cut_offset` bytes into the query.
    for (query_index, cut_offset) in (1..=9_usize).enumerate() {
        batch.extend(plain_text_batch(PARSER_SECTION_BYTES - cut_offset));
        let (row, col) = (query_index + 1, query_index + 2);
        let query = format!("\x1b[{row};{col}H\x1b[6n");
        query_ranges.push(batch.len()..batch.len() + query.len());
        batch.extend_from_slice(query.as_bytes());
        expected_replies.extend_from_slice(format!("\x1b[{row};{col}R").as_bytes());
    }
    batch.extend_from_slice(b"done");
    let mut submissions = Vec::new();
    let boundaries = record_section_boundaries(&handles.parser, || {
        process_pane_vt_batch_with(
            &handles,
            &batch,
            &mut None,
            |_| None,
            |_| {},
            Instant::now,
            |reply| submissions.push(reply),
        );
    });
    // Every query has an interior boundary strictly inside it, which only the bound makes.
    for query_range in &query_ranges {
        assert!(
            boundaries.iter().any(|boundary| {
                let cut_at = batch.len() - boundary.remaining_bytes;
                query_range.start < cut_at && cut_at < query_range.end
            }),
            "no section bound inside the query at {query_range:?}"
        );
    }
    assert_eq!(submissions, vec![expected_replies], "one submission, every reply once, in order");
}

/// How a test batch reaches the worker's parse loop.
#[derive(Clone, Copy)]
enum SectionBound {
    /// The production worker path, `process_pane_vt_batch_with`.
    Production,
    /// One section per reply, the unbounded parse the bound must reproduce.
    Unbounded,
}

/// The rendered form of a hyperlink: its client id and URI, independent of the interned id.
type ResolvedLink = (Option<String>, String);

/// A cell with its hyperlink id replaced by the link it resolves to.
type ResolvedCell = (sonicterm_grid::grid::Cell, Option<ResolvedLink>);

/// Everything a worker batch leaves behind or emits, comparable across two parsers whose
/// hyperlink ids differ because ids are allocated process-wide.
#[derive(Debug, PartialEq)]
struct ParseOutcome {
    visible_rows: Vec<(Vec<ResolvedCell>, bool)>,
    scrollback_rows: Vec<(Vec<ResolvedCell>, bool)>,
    cursor: sonicterm_grid::grid::Pos,
    pending_wrap: bool,
    autowrap: bool,
    alternate_screen: bool,
    prompts: Vec<sonicterm_grid::grid::PromptRegion>,
    title: Option<String>,
    cwd: Option<String>,
    osc7_cwd: Option<sonicterm_vt::vt::Osc7Cwd>,
    cwd_revision: u64,
    hyperlink_count: usize,
    current_hyperlink: Option<ResolvedLink>,
    bracketed_paste: bool,
    keyboard_input: u64,
    pointer_input: u8,
    published_cursor_visible: bool,
    published_keyboard_input: u64,
    published_pointer_input: u8,
    emitted_events: Vec<UserEvent>,
    command_events: Vec<(CommandEvent, Duration, Option<Duration>)>,
    media_events: Vec<MediaEvent>,
    reply_submissions: Vec<Vec<u8>>,
}

/// Resolve `link_id` through `parser`'s registry.
fn resolve_link(parser: &Parser, link_id: sonicterm_types::HyperlinkId) -> Option<ResolvedLink> {
    parser
        .hyperlinks()
        .lookup(link_id)
        .map(|link| (link.id.as_deref().map(str::to_owned), link.uri.to_string()))
}

/// Copy one row's cells with each hyperlink resolved, and its soft-wrap provenance.
fn resolve_row(parser: &Parser, row: &sonicterm_grid::grid::Row) -> (Vec<ResolvedCell>, bool) {
    let cells = row
        .iter()
        .map(|cell| {
            let link = cell.hyperlink().and_then(|link_id| resolve_link(parser, link_id));
            let mut unlinked = cell.clone();
            unlinked.set_hyperlink(None);
            (unlinked, link)
        })
        .collect();
    (cells, row.soft_wrapped_from_previous())
}

/// Parse `batch` on a fresh private-pool pane through `bound`, with a fake clock, and return
/// its outcome and interior section boundaries.
fn parse_through_worker(batch: &[u8], bound: SectionBound) -> (ParseOutcome, Vec<SectionBoundary>) {
    let (_pane, handles) = pane_and_worker_handles();
    let clock_base = Instant::now();
    let mut clock_ticks = 0_u64;
    let fake_clock = || {
        clock_ticks += 1;
        clock_base + Duration::from_millis(clock_ticks)
    };
    let mut emitted_events = Vec::new();
    let mut media_events = Vec::new();
    let mut reply_submissions = Vec::new();
    let boundaries = record_section_boundaries(&handles.parser, || {
        let decode_media = |media: &MediaEvent| {
            media_events.push(media.clone());
            None
        };
        let emit_event = |event| emitted_events.push(event);
        let send_reply = |reply| reply_submissions.push(reply);
        match bound {
            SectionBound::Production => process_pane_vt_batch_with(
                &handles,
                batch,
                &mut None,
                decode_media,
                emit_event,
                fake_clock,
                send_reply,
            ),
            SectionBound::Unbounded => process_pane_vt_batch_in_sections(
                &handles,
                batch,
                usize::MAX,
                &mut None,
                decode_media,
                emit_event,
                fake_clock,
                send_reply,
            ),
        }
    });
    let parser = handles.parser.lock();
    let grid = parser.grid();
    let outcome = ParseOutcome {
        visible_rows: grid.rows_iter().map(|row| resolve_row(&parser, row)).collect(),
        scrollback_rows: grid.scrollback_iter().map(|row| resolve_row(&parser, row)).collect(),
        cursor: grid.cursor,
        pending_wrap: grid.pending_wrap(),
        autowrap: grid.autowrap(),
        alternate_screen: grid.is_alt(),
        prompts: grid.prompts().copied().collect(),
        title: parser.title().map(str::to_owned),
        cwd: parser.cwd().map(str::to_owned),
        osc7_cwd: parser.osc7_cwd().cloned(),
        cwd_revision: parser.cwd_revision(),
        hyperlink_count: parser.hyperlinks().len(),
        current_hyperlink: parser
            .current_hyperlink()
            .and_then(|link_id| resolve_link(&parser, link_id)),
        bracketed_paste: parser.bracketed_paste_enabled(),
        keyboard_input: parser.keyboard_input_snapshot(),
        pointer_input: parser.pointer_input_snapshot(),
        published_cursor_visible: handles.cursor_visible.load(Ordering::Relaxed),
        published_keyboard_input: handles.keyboard_input.load(Ordering::Relaxed),
        published_pointer_input: handles.pointer_input.load(Ordering::Relaxed),
        emitted_events,
        command_events: handles
            .command_events
            .lock()
            .iter()
            .map(|command| (command.event, command.at - clock_base, command.duration))
            .collect(),
        media_events,
        reply_submissions,
    };
    (outcome, boundaries)
}

/// One sequence the bound must be able to cut anywhere, and a check that it took effect.
struct BoundaryFixture {
    name: &'static str,
    sequence: Vec<u8>,
    took_effect: fn(&ParseOutcome) -> bool,
}

/// UTF-8, CSI, OSC 8, OSC 7, DECRQSS, Kitty, iTerm2, OSC 52 and OSC 133 sequences.
fn boundary_fixtures() -> Vec<BoundaryFixture> {
    let image_payload = base64::engine::general_purpose::STANDARD.encode([7_u8; 24]);
    let clipboard_payload = base64::engine::general_purpose::STANDARD.encode("copied");
    vec![
        BoundaryFixture {
            name: "utf8",
            sequence: "é€😀".as_bytes().to_vec(),
            took_effect: |outcome| {
                outcome.visible_rows.iter().flat_map(|row| &row.0).any(|cell| cell.0.ch == '😀')
            },
        },
        BoundaryFixture {
            name: "csi",
            sequence: b"\x1b[38;2;10;20;30;1;4m\x1b[?2004h\x1b[5;7H".to_vec(),
            // CUP lands on row 4; the trailing CR LF writes the 18-byte tail on row 5 in the SGR style.
            took_effect: |outcome| {
                let tail_cell = &outcome.visible_rows[5].0[0].0;
                outcome.bracketed_paste
                    && (outcome.cursor.row, outcome.cursor.col) == (5, 18)
                    && tail_cell.ch == 'a'
                    && tail_cell.fg != outcome.visible_rows[5].0[40].0.fg
            },
        },
        BoundaryFixture {
            name: "osc8",
            sequence: b"\x1b]8;id=fixture;https://example.com/a\x1b\\linked\x1b]8;;\x1b\\".to_vec(),
            took_effect: |outcome| {
                outcome.visible_rows.iter().flat_map(|row| &row.0).any(|cell| {
                    cell.1.as_ref().is_some_and(|link| link.1 == "https://example.com/a")
                })
            },
        },
        BoundaryFixture {
            name: "osc7",
            sequence: b"\x1b]7;file://builder.example/home/user/work%20dir\x07".to_vec(),
            took_effect: |outcome| {
                outcome.osc7_cwd.as_ref().is_some_and(|cwd| {
                    cwd.authority == "builder.example" && cwd.path == "/home/user/work dir"
                })
            },
        },
        BoundaryFixture {
            name: "decrqss",
            sequence: b"\x1b[4:3m\x1bP$qm\x1b\\".to_vec(),
            took_effect: |outcome| !outcome.reply_submissions.is_empty(),
        },
        BoundaryFixture {
            name: "kitty",
            sequence: format!("\x1b_Gf=100,a=T;{image_payload}\x1b\\").into_bytes(),
            took_effect: |outcome| {
                outcome.media_events.iter().any(|media| media.protocol == MediaProtocol::Kitty)
            },
        },
        BoundaryFixture {
            name: "iterm2",
            sequence: format!("\x1b]1337;File=inline=1:{image_payload}\x07").into_bytes(),
            took_effect: |outcome| {
                outcome.media_events.iter().any(|media| media.protocol == MediaProtocol::Iterm2File)
            },
        },
        BoundaryFixture {
            name: "osc52-osc133",
            sequence: format!("\x1b]52;c;{clipboard_payload}\x07\x1b]133;C\x07\x1b]133;D;0\x07")
                .into_bytes(),
            took_effect: |outcome| {
                outcome.emitted_events.len() == 1 && outcome.command_events.len() == 2
            },
        },
    ]
}

/// A bounded parse through the production worker path equals one unbounded parse of the
/// same bytes, whichever byte of each sequence the section bound falls before.
#[test]
fn bounded_sections_parse_like_one_unbounded_section_at_every_cut() {
    for fixture in boundary_fixtures() {
        for cut_offset in 0..fixture.sequence.len() {
            // The first bound falls `cut_offset` bytes into the sequence.
            let mut batch = plain_text_batch(PARSER_SECTION_BYTES - cut_offset);
            batch.extend_from_slice(&fixture.sequence);
            batch.extend_from_slice(b"\r\nafter the sequence");
            let (bounded, bounded_boundaries) =
                parse_through_worker(&batch, SectionBound::Production);
            let (unbounded, _) = parse_through_worker(&batch, SectionBound::Unbounded);
            let first_cut_remaining = batch.len() - PARSER_SECTION_BYTES;
            assert!(
                bounded_boundaries
                    .iter()
                    .any(|boundary| boundary.remaining_bytes == first_cut_remaining),
                "{} offset {cut_offset}: no section bound inside the sequence",
                fixture.name
            );
            assert!(
                (fixture.took_effect)(&unbounded),
                "{} offset {cut_offset}: the fixture had no effect",
                fixture.name
            );
            assert!(
                bounded == unbounded,
                "{} offset {cut_offset}: bounded {bounded:#?}\nunbounded {unbounded:#?}",
                fixture.name
            );
        }
    }
}
