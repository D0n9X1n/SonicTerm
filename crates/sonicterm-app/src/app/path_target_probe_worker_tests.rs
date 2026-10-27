//! Preserved probe worker path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

/// A window's newer request replaces only its own waiting request while one wake is pending.
#[test]
fn probe_mailbox_replaces_only_the_same_windows_waiting_request() {
    let (mailbox, wakes) = PathProbeMailbox::new();
    let mut state = PathProbeState::default();
    let first = state.request(probe_key("/work/first", 20)).unwrap();
    state.invalidate();
    let latest = state.request(probe_key("/work/latest", 20)).unwrap();

    mailbox.submit(first).unwrap();
    mailbox.submit(latest.clone()).unwrap();
    assert_eq!(wakes.len(), 1, "only one worker wake is queued");
    assert_eq!(mailbox.waiting_len(), 1, "one window keeps one waiting request");
    wakes.recv().unwrap();
    assert_eq!(mailbox.take_next(), Some(latest));
    assert_eq!(mailbox.take_next(), None);
}

/// A newer request from another window never displaces a waiting one;
/// both windows reach a terminal result.
#[test]
fn probe_mailbox_keeps_waiting_requests_from_distinct_windows() {
    let worker = HeldProbeWorker::spawn();
    let (mut blocker, mut first, mut second) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let blocker_key = window_probe_key(3, "/work/blocker");
    let first_key = window_probe_key(1, "/work/first");
    let second_key = window_probe_key(2, "/work/second");
    worker.mailbox.submit(blocker.request(blocker_key.clone()).unwrap()).unwrap();
    worker.expect_started("/work/blocker");

    // The first window queues behind the held execution, then a second window submits.
    worker.mailbox.submit(first.request(first_key.clone()).unwrap()).unwrap();
    worker.mailbox.submit(second.request(second_key.clone()).unwrap()).unwrap();
    // The first window's key is in flight, so its unchanged hover never asks again.
    assert!(first.request(first_key.clone()).is_none());

    assert!(blocker.accept(&worker.finish(), Some(&blocker_key)));
    let first_result = worker.serve("/work/first");
    let second_result = worker.serve("/work/second");
    worker.assert_idle();
    assert!(first.accept(&first_result, Some(&first_key)));
    assert!(first.authorized(&first_key, true));
    assert!(second.accept(&second_result, Some(&second_key)));
    assert!(second.authorized(&second_key, true));
    // A result carries its window identity, so it can never complete another window's key.
    assert!(!second.accept(&first_result, Some(&second_key)));
    assert!(second.authorized(&second_key, true));
}

/// Same-window churn A→B→A replaces only that window's waiting key, keeps its turn,
/// and runs only the newest epoch.
#[test]
fn probe_mailbox_replaces_a_windows_own_waiting_key_in_place() {
    let worker = HeldProbeWorker::spawn();
    let (mut blocker, mut churn, mut peer) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let key_a = window_probe_key(1, "/work/a");
    let key_b = window_probe_key(1, "/work/b");
    let peer_key = window_probe_key(2, "/work/peer");
    worker.mailbox.submit(blocker.request(window_probe_key(3, "/work/blocker")).unwrap()).unwrap();
    worker.expect_started("/work/blocker");

    let stale = churn.request(key_a.clone()).unwrap();
    worker.mailbox.submit(stale.clone()).unwrap();
    worker.mailbox.submit(peer.request(peer_key.clone()).unwrap()).unwrap();
    worker.mailbox.submit(churn.request(key_b).unwrap()).unwrap();
    worker.mailbox.submit(churn.request(key_a.clone()).unwrap()).unwrap();
    assert_eq!(worker.mailbox.waiting_len(), 2, "each window keeps one waiting key");

    worker.finish();
    // The churning window keeps its first-queued turn ahead of the peer
    // and runs only its newest epoch.
    let served = worker.serve("/work/a");
    assert_ne!(served.request.epoch, stale.epoch);
    let peer_result = worker.serve("/work/peer");
    worker.assert_idle();
    // The displaced epoch can never complete, even though it names the same key.
    let displaced = PathProbeResult { request: stale, ..served.clone() };
    assert!(!churn.accept(&displaced, Some(&key_a)));
    assert!(churn.accept(&served, Some(&key_a)));
    assert!(churn.authorized(&key_a, true));
    assert!(peer.accept(&peer_result, Some(&peer_key)));
}

/// Closing a window drops only its waiting request; an executing request still
/// completes for its own window.
#[test]
fn probe_mailbox_close_drops_only_that_windows_waiting_request() {
    let worker = HeldProbeWorker::spawn();
    let (mut running, mut closing, mut survivor) =
        (PathProbeState::default(), PathProbeState::default(), PathProbeState::default());
    let survivor_key = window_probe_key(3, "/work/survivor");
    worker.mailbox.submit(running.request(window_probe_key(1, "/work/running")).unwrap()).unwrap();
    worker.expect_started("/work/running");
    worker.mailbox.submit(closing.request(window_probe_key(2, "/work/closing")).unwrap()).unwrap();
    worker.mailbox.submit(survivor.request(survivor_key.clone()).unwrap()).unwrap();

    assert!(worker.mailbox.cancel_window(WindowId::from(2_u64)));
    assert!(!worker.mailbox.cancel_window(WindowId::from(2_u64)), "nothing else waits for it");
    // An executing request is not waiting, so closing its window leaves it to finish.
    assert!(!worker.mailbox.cancel_window(WindowId::from(1_u64)));
    assert_eq!(worker.mailbox.waiting_len(), 1);
    assert_eq!(worker.finish().request.key.window_id, WindowId::from(1_u64));
    let survivor_result = worker.serve("/work/survivor");
    worker.assert_idle();
    assert!(survivor.accept(&survivor_result, Some(&survivor_key)));
}

/// A peer that re-requests every frame cannot starve a quiet window's single waiting request.
#[test]
fn probe_mailbox_keeps_fair_progress_under_a_churning_peer() {
    let worker = HeldProbeWorker::spawn();
    let (mut quiet, mut churn) = (PathProbeState::default(), PathProbeState::default());
    let quiet_key = window_probe_key(1, "/work/quiet");
    let frame = |index: usize| window_probe_key(2, &format!("/work/frame-{index}"));
    worker.mailbox.submit(churn.request(frame(0)).unwrap()).unwrap();
    worker.expect_started("/work/frame-0");
    worker.mailbox.submit(quiet.request(quiet_key.clone()).unwrap()).unwrap();
    for index in 1..=32 {
        worker.mailbox.submit(churn.request(frame(index)).unwrap()).unwrap();
        assert_eq!(worker.mailbox.waiting_len(), 2, "churn replaces its own waiting slot");
    }

    worker.finish();
    worker.expect_started("/work/quiet");
    // Churn while the quiet request executes still collapses into one waiting request.
    for index in 33..=64 {
        worker.mailbox.submit(churn.request(frame(index)).unwrap()).unwrap();
        assert_eq!(worker.mailbox.waiting_len(), 1);
    }
    let quiet_result = worker.finish();
    let churn_result = worker.serve("/work/frame-64");
    worker.assert_idle();
    assert!(quiet.accept(&quiet_result, Some(&quiet_key)));
    assert!(quiet.authorized(&quiet_key, true));
    assert!(churn.accept(&churn_result, Some(&frame(64))));
}

/// Production child teardown cancels a closed window's waiting probe and discards
/// a late result for it.
#[test]
fn closing_a_child_window_cancels_its_probe_and_discards_its_late_result() {
    let worker = HeldProbeWorker::spawn();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    install_held_worker(&mut app, &worker);
    let running = app.__test_seed_child_window(&["running"]);
    let closing = app.__test_seed_child_window(&["closing"]);
    let survivor = app.__test_seed_child_window(&["survivor"]);
    let survivor_key = seeded_probe_key(survivor, "/work/survivor");
    hover_probe_key(&mut app, running, &seeded_probe_key(running, "/work/running"));
    worker.expect_started("/work/running");
    hover_probe_key(&mut app, closing, &seeded_probe_key(closing, "/work/closing"));
    hover_probe_key(&mut app, survivor, &survivor_key);
    assert_eq!(worker.mailbox.waiting_len(), 2);

    assert!(app.close_child_window(closing));
    assert_eq!(worker.mailbox.waiting_len(), 1, "closing drops only its own waiting probe");
    assert!(app.close_child_window(running));
    // The running probe completes after its window closed, so no owner receives it.
    app.handle_path_probe_finished(worker.finish());
    assert!(!app.windows.contains_key(&running));
    app.handle_path_probe_finished(worker.serve("/work/survivor"));
    worker.assert_idle();
    hover_probe_key(&mut app, survivor, &survivor_key);
    assert!(app.windows[&survivor].path_probe.authorized(&survivor_key, true));
}

/// A stationary hover keeps its feedback while a second window submits a new probe on every
/// redraw, through the real worker loop and filesystem classifier.
///
/// App-level coverage with the real filesystem: it drives hover probes through test helpers,
/// so it does not exercise native presentation or PTY output.
///
/// Only the streaming window's first probe is held, standing in for a slow filesystem
/// call, so the quiet window's single request queues behind it as it would on a busy host.
#[test]
fn quiet_hover_keeps_first_window_feedback_while_a_second_window_streams() {
    let root = native_test_root().join(format!(
        "sonicterm-quiet-hover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let quiet_path = root.join("quiet.txt");
    std::fs::write(&quiet_path, b"quiet").unwrap();
    let frame_path = |index: usize| root.join(format!("frame-{index}.txt"));
    let gate = frame_path(0);
    std::fs::write(&gate, b"frame").unwrap();

    let (mailbox, wake) = PathProbeMailbox::new();
    let queue = Arc::clone(&mailbox.queue);
    let (gate_started_tx, gate_started) = crossbeam_channel::unbounded::<()>();
    let (gate_release, gate_release_rx) = crossbeam_channel::unbounded::<()>();
    let (results_tx, results) = crossbeam_channel::unbounded();
    let (worker_stopped_tx, worker_stopped) = crossbeam_channel::bounded::<()>(1);
    std::thread::spawn(move || {
        run_probe_worker(
            &wake,
            &queue,
            |path| {
                if path == gate.as_path() {
                    // Hold only the streaming window's first probe so later work queues behind it.
                    let _ = gate_started_tx.send(());
                    let _ = gate_release_rx.recv();
                }
                classify_local_target(path)
            },
            |result| results_tx.send(result).is_ok(),
            None,
        );
        let _ = worker_stopped_tx.send(());
    });
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers {
        probe: mailbox,
        open,
        busy: Arc::new(crate::app::jobs::PathBusy::new()),
    });
    let quiet = app.__test_seed_child_window(&["quiet"]);
    let peer = app.__test_seed_child_window(&["peer"]);
    app.frontmost_window = Some(quiet);
    app.windows.get_mut(&quiet).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let quiet_key = seeded_probe_key(quiet, quiet_path.to_str().unwrap());
    let peer_key = |index: usize| seeded_probe_key(peer, frame_path(index).to_str().unwrap());
    let assert_quiet_feedback = |app: &App, moment: &str| {
        let window = &app.windows[&quiet];
        assert!(
            window.path_probe.authorized(&quiet_key, true),
            "{moment}: the quiet target is authorized"
        );
        assert!(
            window.hovered_url.as_ref().is_some_and(|hover| hover.active()),
            "{moment}: the quiet underline stays active"
        );
        assert!(window.hover_link, "{moment}: the quiet pointer cursor stays active");
    };

    hover_probe_key(&mut app, peer, &peer_key(0));
    gate_started.recv_timeout(HELD_PROBE_TIMEOUT).expect("the streaming probe is held");
    for index in 1..=16 {
        // Each redraw refreshes both windows; only the streaming window's content changes.
        hover_probe_key(&mut app, quiet, &quiet_key);
        hover_probe_key(&mut app, peer, &peer_key(index));
    }
    gate_release.send(()).unwrap();
    for _ in 0..3 {
        let result =
            results.recv_timeout(HELD_PROBE_TIMEOUT).expect("both windows' queued probes complete");
        app.handle_path_probe_finished(result);
        hover_probe_key(&mut app, quiet, &quiet_key);
    }
    assert_quiet_feedback(&app, "after the queued probes drain");
    for index in 17..=24 {
        hover_probe_key(&mut app, peer, &peer_key(index));
        hover_probe_key(&mut app, quiet, &quiet_key);
        assert_quiet_feedback(&app, "while the peer keeps streaming");
    }
    // A running Windows probe holds each part of its path open without delete sharing, so stop
    // the worker before removing `root`: dropping the app closes its mailbox, and the worker ends
    // once its queued probes finish.
    drop(app);
    drop(gate_release);
    worker_stopped.recv_timeout(HELD_PROBE_TIMEOUT).expect("the probe worker stops");
    std::fs::remove_dir_all(root).unwrap();
}

/// Pending and displaced probe work never authorizes activation; a current result opens only
/// after the click validates it against the key the click computes for its own cell.
#[cfg(unix)]
#[test]
fn only_a_validated_current_probe_authorizes_activation() {
    let root = native_test_root().join(format!(
        "sonicterm-pending-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), b"notes").unwrap();
    // No worker thread runs: the test takes queued requests itself, so they stay pending.
    let (mailbox, _wake) = PathProbeMailbox::new();
    let (open, opens) = crossbeam_channel::bounded(1);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.path_workers = Some(PathWorkers {
        probe: mailbox.clone(),
        open,
        busy: Arc::new(crate::app::jobs::PathBusy::new()),
    });
    let window = app.__test_seed_child_window(&["activation"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let output = format!("\x1b]7;file://{}\x1b\\./notes.txt", root.display());
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let hover = |app: &mut App| {
        let target = app.cell_target_at(window, pane, 0, 3).expect("explicit path target");
        assert!(matches!(target.target, ResolvedCellTarget::Path(_)), "the path needs a probe");
        app.apply_target_hover(window, Some(target));
    };
    // Each click starts unarmed so a pending-failure click never becomes a copy confirmation.
    let click = |app: &mut App| {
        app.windows.get_mut(&window).unwrap().path_probe.failed_click = None;
        app.activate_target_at(window, pane, 0, 3)
    };
    let serve = |request: PathProbeRequest| {
        let outcome = probe_candidates(&request.key.candidates, classify_local_target);
        PathProbeResult {
            failure: outcome.as_ref().err().copied(),
            selection: outcome.ok(),
            request,
        }
    };

    hover(&mut app);
    click(&mut app);
    assert!(opens.try_recv().is_err(), "a queued request never authorizes an open");

    // A newer hover epoch displaces the probe the worker already took for this window.
    let displaced = mailbox.take_next().expect("hover queued a probe");
    app.clear_target_hover(window);
    hover(&mut app);
    app.handle_path_probe_finished(serve(displaced));
    hover(&mut app);
    click(&mut app);
    assert!(opens.try_recv().is_err(), "a displaced completion never authorizes an open");

    // The click validates the current completion against the key it computes for its own cell.
    app.handle_path_probe_finished(serve(mailbox.take_next().expect("newer epoch queued")));
    assert!(click(&mut app));
    let request = opens.try_recv().expect("the validated current result dispatches one open");
    assert!(request.path.ends_with("notes.txt"));
    assert!(opens.try_recv().is_err());
    std::fs::remove_dir_all(root).unwrap();
}

/// A finished check that no hover has applied yet still authorizes a click on the same cell,
/// because the click accepts it against the key the click computes itself. Once the viewport
/// has moved, the click's key differs and the waiting result authorizes nothing.
#[cfg(unix)]
#[test]
fn clicks_accept_a_finished_check_only_for_their_own_key() {
    for viewport_moves in [false, true] {
        let root = native_test_root().join(format!(
            "sonicterm-waiting-open-{}-{viewport_moves}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), b"notes").unwrap();
        // No worker thread runs: the test serves the queued request itself.
        let (mailbox, _wake) = PathProbeMailbox::new();
        let (open, opens) = crossbeam_channel::bounded(1);
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.path_workers = Some(PathWorkers {
            probe: mailbox.clone(),
            open,
            busy: Arc::new(crate::app::jobs::PathBusy::new()),
        });
        let window = app.__test_seed_child_window(&["waiting"]);
        let pane = app.__test_child_pane_ids(window).unwrap()[0];
        app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(40, 3);
        // The path sits on the second row, so two more lines scroll it to the top row.
        let output = format!("\x1b]7;file://{}\x1b\\\r\n./notes.txt", root.display());
        assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        let target = app.cell_target_at(window, pane, 1, 3).expect("explicit path target");
        app.apply_target_hover(window, Some(target));
        let request = mailbox.take_next().expect("hover queued a probe");
        let outcome = probe_candidates(&request.key.candidates, classify_local_target);
        let expected = outcome.as_ref().ok().map(|selection| selection.decision);
        app.handle_path_probe_finished(PathProbeResult {
            failure: outcome.as_ref().err().copied(),
            selection: outcome.ok(),
            request,
        });
        let row = if viewport_moves {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\r\n\r\n"));
            0
        } else {
            1
        };
        // No hover refresh ran, so the finished result still waits when the click arrives.
        assert!(app.windows[&window].path_probe.pending_result.is_some());
        assert!(app.activate_target_at(window, pane, row, 3));
        if viewport_moves {
            assert!(opens.try_recv().is_err(), "a moved viewport never uses the waiting result");
        } else {
            let request = opens.try_recv().expect("the click accepts the waiting result");
            assert!(request.path.ends_with("notes.txt"));
            assert_eq!(Some(request.expected_decision), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
