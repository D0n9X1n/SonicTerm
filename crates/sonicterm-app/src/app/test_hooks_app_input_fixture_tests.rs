//! Native input submission observations and source-window fixtures. regression contracts.

use super::*;

/// Moving a live pane keeps its task reservation and never schedules teardown before an actual close.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_transfer_preserves_native_teardown_reservation() {
    if isolated() {
        return;
    }
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(media::InlineMediaPool::new());
    let pane = app.__test_seed_tab("source");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["destination"]);
    attach_idle_pty(&mut app, main, pane);
    app.reconcile_pane_owners();
    let pid = app.windows[&main].panes[&pane].pty.as_ref().unwrap().pid().unwrap();
    assert!(app.__test_move_pane_between_windows(main, child, pane));
    assert!(app.windows[&child].panes[&pane].reap_slot.is_some());
    assert_eq!(app.windows[&child].panes[&pane].pty.as_ref().unwrap().pid(), Some(pid));
    assert_eq!(app.pty_reaper.path_counts(), (0, 0));
    assert!(app.finish_session());
    assert_eq!(app.pty_reaper.path_counts(), (1, 0));
    record_process(pid, false);
}

/// Delivery assertions wait out a real queue refusal instead of mistaking writer lag for lost input.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_input_queue_wait_allows_delivery_after_refusal() {
    if isolated() {
        return;
    }
    let (mut app, _, pane) = input_test_app();
    let pty = app.pane_by_id(pane).unwrap().pty.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match pty.send_input_nonblocking(b"q".to_vec()) {
            Ok(()) => assert!(Instant::now() < deadline, "input queue never refused a write"),
            Err(sonicterm_io::pty::PtyInputError::QueueFull(_)) => break,
            Err(error) => panic!("unexpected input refusal: {error}"),
        }
    }
    app.wait_for_input_queues();
    assert_eq!(
        pty.input_diagnostics().queued_messages,
        0,
        "queue must drain before delivery assertions"
    );
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"next".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"next".to_vec())]);
}

/// Only successful queue admission is evidence; an oversized real-PTY write contributes nothing.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_excludes_failed_input() {
    if isolated() {
        return;
    }
    let (mut app, _, pane) = input_test_app();
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"accepted".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"accepted".to_vec())]);
    let oversized = vec![b'x'; sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    assert!(!app.write_to_pane(pane, oversized, PtyInputSource::Paste));
    assert!(submitted.take().is_empty());
    app.wait_for_input_queues();
    let pty = app.pane_by_id(pane).unwrap().pty.as_ref().unwrap();
    assert!(App::queue_pty_input(None, pty, pane, PtyInputSource::ScriptDraft, b"draft".to_vec()));
    assert_eq!(submitted.take(), vec![(pane, b"draft".to_vec())]);
    assert!(!App::queue_pty_input(
        None,
        pty,
        pane,
        PtyInputSource::ScriptDraft,
        vec![b'x'; sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1],
    ));
    assert!(submitted.take().is_empty());
}

/// Pending motion is not submission, even though staging returns accepted ownership to the caller.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_excludes_staged_motion() {
    if isolated() {
        return;
    }
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"control".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"control".to_vec())]);
    assert!(app.write_to_pane(pane, b"\x1b[<35;2;3M".to_vec(), PtyInputSource::PointerMotion));
    assert_ne!(app.pane_by_id(pane).unwrap().pending_pointer_motion.len, 0);
    assert!(submitted.take().is_empty());
}

/// A motion-only flush records exactly the newest coalesced report that the real PTY accepted.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_records_flushed_motion() {
    if isolated() {
        return;
    }
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    for report in [b"\x1b[<35;1;3M", b"\x1b[<35;2;3M"] {
        assert!(app.write_to_pane(pane, report.to_vec(), PtyInputSource::PointerMotion));
    }
    assert!(submitted.take().is_empty());
    assert_eq!(app.flush_pointer_motion(Instant::now()), None);
    assert_eq!(submitted.take(), vec![(pane, b"\x1b[<35;2;3M".to_vec())]);
    assert_eq!(app.pane_by_id(pane).unwrap().pending_pointer_motion.len, 0);
}

/// Motion followed by a discrete input records their actual combined queue payload once, in order.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_records_motion_with_discrete_input() {
    if isolated() {
        return;
    }
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"\x1b[<35;2;3M".to_vec(), PtyInputSource::PointerMotion));
    assert!(submitted.take().is_empty());
    assert!(app.write_to_pane(pane, b"key".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"\x1b[<35;2;3Mkey".to_vec())]);
    assert_eq!(app.flush_pointer_motion(Instant::now()), None);
    assert!(submitted.take().is_empty());
}

/// New AllTabs keyboard fan-out excludes a READONLY receiver but still admits the source and its other peer.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_alltabs_keyboard_excludes_readonly_receivers() {
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        for read_only in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (_, source) = windows[source_index];
            let (protected_window, protected) = windows[(source_index + 1) % 3];
            let (_, peer) = windows[(source_index + 2) % 3];
            app.windows.get_mut(&protected_window).unwrap().copy_mode = Some(if read_only {
                CopyModeState::read_only_at((0, 0))
            } else {
                CopyModeState::new_at((0, 0))
            });
            app.broadcast =
                BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            let writes = app
                .terminal_key_targets(source)
                .into_iter()
                .map(|pane| {
                    (
                        pane,
                        keyboard_protocol::EncodedKey {
                            bytes: b"k".to_vec(),
                            held: HeldKey::Legacy,
                        },
                    )
                })
                .collect();
            input_phase(source, "write-begin");
            let delivered = app.dispatch_terminal_key_writes(writes);
            input_phase(source, "write-end");
            let expected = if read_only {
                BTreeSet::from([source, peer])
            } else {
                BTreeSet::from([source, protected, peer])
            };
            let actual = submitted.take();
            assert_eq!(
                actual.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(),
                expected,
                "source={source_index} read_only={read_only}"
            );
            assert!(actual.iter().all(|(_, bytes)| bytes == b"k"));
            assert_eq!(delivered.keys().copied().collect::<BTreeSet<_>>(), expected);
            assert_eq!(app.broadcast_participants(), expected);
            assert_eq!(
                app.broadcast_receivers(),
                expected.difference(&BTreeSet::from([source])).copied().collect()
            );
            drop(app);
        }
    }
}

/// The byte fan-out shares the keyboard receiver filter without treating ordinary copy mode as READONLY.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_alltabs_byte_fanout_excludes_readonly_receivers() {
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        let (mut app, windows) = input_test_windows();
        let (_, source) = windows[source_index];
        let (protected_window, _) = windows[(source_index + 1) % 3];
        let (_, peer) = windows[(source_index + 2) % 3];
        app.windows.get_mut(&protected_window).unwrap().copy_mode =
            Some(CopyModeState::read_only_at((0, 0)));
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.wait_for_input_queues();
        let submitted = PtySubmissions::start();
        app.broadcast_from(source, b"fanout".to_vec(), PtyInputSource::Ime);
        assert_eq!(submitted.take(), vec![(peer, b"fanout".to_vec())]);
    }
}

/// Explicit IME and paste intents admit only their live destination, and READONLY never redirects to an unprotected peer.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_intent_ime_and_paste_respect_readonly_admission() {
    if isolated() {
        return;
    }
    use sonicterm_app_core::AppIntent;
    for target_index in 0..3 {
        for copy_kind in ["none", "copy", "readonly"] {
            for ime in [false, true] {
                let (mut app, windows) = input_test_windows();
                let (target, pane) = windows[target_index];
                app.frontmost_window = Some(windows[(target_index + 1) % 3].0);
                app.windows.get_mut(&target).unwrap().copy_mode = match copy_kind {
                    "copy" => Some(CopyModeState::new_at((0, 0))),
                    "readonly" => Some(CopyModeState::read_only_at((0, 0))),
                    _ => None,
                };
                let window = app.window_key(target).unwrap();
                let intent = if ime {
                    AppIntent::ImeCommit { window, text: "intent".into() }
                } else {
                    AppIntent::Paste { window, text: "intent".into(), bracketed: false }
                };
                app.wait_for_input_queues();
                let submitted = PtySubmissions::start();
                app.dispatch_intent(intent);
                assert_eq!(
                    submitted.take(),
                    if copy_kind == "readonly" {
                        Vec::new()
                    } else {
                        vec![(pane, b"intent".to_vec())]
                    },
                    "target={target_index} copy={copy_kind} ime={ime}"
                );
            }
        }
    }
}

/// AppKit reports physical size at its current backing scale, even when the stored event scale is older.
#[cfg(target_os = "macos")]
#[test]
fn macos_dpi_transition_preserves_current_native_size() {
    let logical = winit::dpi::LogicalSize::new(800.0, 600.0);
    for (stored, native, destination) in
        [(1.0, 2.0, 2.0), (2.0, 1.0, 1.0), (1.0, 1.5, 1.5), (1.5, 2.0, 1.5), (2.0, 2.0, 2.0)]
    {
        let current = logical.to_physical::<u32>(native);
        let target = dpi_transition_inner_size(
            current,
            dpi_transition_size_scale(stored, native),
            destination,
            winit::dpi::PhysicalSize::new(100, 100),
            winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX),
        );
        assert_eq!(
            target,
            logical.to_physical::<u32>(destination),
            "stored={stored} native={native} destination={destination}"
        );
    }
}

/// Repeated AppKit scale changes preserve the same logical extent instead of compounding native width and height.
#[cfg(target_os = "macos")]
#[test]
fn macos_dpi_transition_does_not_compound_round_trips() {
    let mut logical = winit::dpi::LogicalSize::new(800.0, 600.0);
    let initial = logical;
    let mut stored = 1.0;
    for native in [2.0, 1.0, 2.0, 1.0] {
        let target = dpi_transition_inner_size(
            logical.to_physical::<u32>(native),
            dpi_transition_size_scale(stored, native),
            native,
            winit::dpi::PhysicalSize::new(100, 100),
            winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX),
        );
        logical = target.to_logical(native);
        assert_eq!(logical, initial);
        stored = native;
    }
}

/// Physical-size source domains do not bypass terminal minimums or destination work-area caps.
#[test]
fn dpi_transition_preserves_bounds_in_both_size_domains() {
    let minimum = winit::dpi::PhysicalSize::new(700, 500);
    let available = winit::dpi::PhysicalSize::new(1800, 1000);
    for source in [1.0, 2.0] {
        let undersized = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(50, 40),
            source,
            2.0,
            minimum,
            available,
        );
        assert_eq!(undersized, minimum);
        let oversized = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(3000, 2400),
            source,
            2.0,
            minimum,
            available,
        );
        assert_eq!(oversized, available);
        let insufficient = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(50, 40),
            source,
            2.0,
            minimum,
            winit::dpi::PhysicalSize::new(600, 400),
        );
        assert_eq!(insufficient, minimum);
    }
}
