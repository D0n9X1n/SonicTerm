//! Tear-out destination rollback and native custody contracts.

use super::super::tear_out_tests::*;
use super::*;

#[test]
fn refused_charged_transfer_cannot_reap_the_source_window() {
    // A charged final tab stays attached if the destination has no accounting owner.
    let mut app = app_with_tabs(&["destination"]);
    let target = app.main_window_id.unwrap();
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    let source = app.__test_seed_child_window(&["source"]);
    let pane_id = app.windows[&source].tab_states[0].active_pane;
    prepare_non_vacuous_source(&mut app, source, pane_id);
    let source_owner = app.windows[&source].owner.as_ref().unwrap().id();
    let window = app.windows.get_mut(&target).unwrap();
    for pane in window.panes.values_mut() {
        pane.charges.clear();
        drop(pane.owner.take());
    }
    drop(window.owner.take());
    let before = source_snapshot(&app, source);

    assert!(app.transfer_tab(Some(source), 0, None, 0).is_err());

    assert_eq!(source_snapshot(&app, source), before);
    assert_eq!(
        app.governor.snapshot(source_owner).unwrap().owner_state,
        sonicterm_types::OwnerState::Open
    );
}

/// Every renderer origin and failure stage maps to one explicit cleanup policy.
#[test]
fn destination_failure_disposition_is_total() {
    assert_eq!(
        destination_disposition(ChildRendererOrigin::Fresh, TearOutStage::CreateWindow),
        DestinationDisposition::Nothing
    );
    for stage in [TearOutStage::RendererInit, TearOutStage::RendererConfigure] {
        assert_eq!(
            destination_disposition(ChildRendererOrigin::Fresh, stage),
            DestinationDisposition::DropFresh
        );
    }
    for stage in [TearOutStage::CreateWindow, TearOutStage::RendererInit] {
        assert_eq!(
            destination_disposition(ChildRendererOrigin::WarmPool, stage),
            DestinationDisposition::ReturnWarm
        );
    }
    assert_eq!(
        destination_disposition(ChildRendererOrigin::WarmPool, TearOutStage::RendererConfigure),
        DestinationDisposition::RetireWarm
    );
    for origin in [ChildRendererOrigin::Fresh, ChildRendererOrigin::WarmPool] {
        assert_eq!(
            destination_disposition(origin, TearOutStage::DeviceStopped),
            DestinationDisposition::Nothing
        );
    }
}

/// A committed OS handoff retains the reducer accounting that formerly ran
/// before every route, while failed native setup performs none.
#[test]
fn committed_handoff_accounts_only_after_the_sink_accepts() {
    let mut app = app_with_reducer_counts(3, 2);
    app.__test_seed_tab("handoff");
    app.__test_set_os_drag_sink(Arc::new(AcceptedSink));
    let before = app.machine.state();
    let before = (before.tab_count, before.live_window_count);

    assert!(app.tear_out_tab_with_installer(0, |_, _, _, _| {
        panic!("accepted handoff must not enter native installation")
    }));

    let after = app.machine.state();
    assert_eq!(after.tab_count, before.0 - 1);
    assert_eq!(after.live_window_count, before.1 + 1);
    assert!(app.pending_new_window.is_none(), "the committed route creates its own destination");
}

/// Native event-loop wrappers delegate all orchestration to the route helpers
/// exercised by the rollback matrix.
#[test]
fn native_wrappers_delegate_to_the_tested_route_helpers() {
    const TEAR_OUT: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let main = &TEAR_OUT[TEAR_OUT.find("fn tear_out_tab(").expect("main wrapper")
        ..TEAR_OUT.find("fn tear_out_tab_with_installer").expect("main helper")];
    assert!(main.contains("tear_out_tab_with_installer"));
    assert!(main.contains("install_torn_out_window"));
    assert!(!main.contains("detach_for_tear_out"));

    let child = &TEAR_OUT[TEAR_OUT.find("fn tear_out_from_child(").expect("child wrapper")
        ..TEAR_OUT.find("fn tear_out_from_child_with_installer").expect("child helper")];
    assert!(child.contains("tear_out_from_child_with_installer"));
    assert!(child.contains("install_torn_out_window"));
    assert!(!child.contains("detach_for_tear_out"));

    const MISC: &str = concat!(
        include_str!("misc.rs"),
        "\n",
        include_str!("misc_clipboard.rs"),
        "\n",
        include_str!("misc_window_create.rs")
    );
    let deferred = &MISC[MISC.find("fn drain_pending_tear_out(").expect("deferred wrapper")
        ..MISC.find("fn drain_pending_tear_out_with_installer").expect("deferred helper")];
    assert!(deferred.contains("drain_pending_tear_out_with_installer"));
    assert!(deferred.contains("install_torn_out_window"));
    assert!(!deferred.contains("detach_for_tear_out"));
}

/// Main, child, and deferred production routes restore exact state at every
/// typed destination failure stage.
#[test]
fn every_tear_out_route_rolls_back_every_destination_failure_stage() {
    for route in [FailureRoute::Main, FailureRoute::Child, FailureRoute::DeferredMain] {
        for stage in [
            TearOutStage::CreateWindow,
            TearOutStage::RendererInit,
            TearOutStage::RendererConfigure,
            TearOutStage::DeviceStopped,
        ] {
            run_failed_route(route, stage);
        }
    }
    run_failed_route(FailureRoute::DeferredChild, TearOutStage::RendererConfigure);
}

/// Failure-time source cleanup never overrides the active tab after exact
/// rollback in main, child, or deferred orchestration.
#[test]
fn rollback_skips_source_cleanup_for_every_route() {
    for route in [
        FailureRoute::Main,
        FailureRoute::Child,
        FailureRoute::DeferredMain,
        FailureRoute::DeferredChild,
    ] {
        let mut app = app_with_reducer_counts(3, 2);
        let source = match route {
            FailureRoute::Main | FailureRoute::DeferredMain => {
                for title in ["a", "b", "c"] {
                    app.__test_seed_tab(title);
                }
                let source = app.__test_main_window_id().expect("synthetic main");
                assert!(app.__test_invoke_activate_main_tab(2));
                source
            }
            FailureRoute::Child | FailureRoute::DeferredChild => {
                let source = app.__test_seed_child_window(&["a", "b", "c"]);
                assert!(app.__test_invoke_activate_tab_in_child(source, 2));
                source
            }
        };
        let tab_id = app.tab_id_at(source, 1).expect("inactive source tab");
        let before = source_snapshot(&app, source);

        match route {
            FailureRoute::Main => {
                assert!(app.tear_out_tab_with_installer(1, fail_without_destination));
            }
            FailureRoute::Child => {
                assert!(app.tear_out_from_child_with_installer(
                    source,
                    1,
                    fail_without_destination,
                ));
            }
            FailureRoute::DeferredMain | FailureRoute::DeferredChild => {
                app.drain_pending_tear_out_with_installer(
                    crate::app::PendingTearOut {
                        source_window: source,
                        source_tab_idx: 1,
                        source_tab_id: Some(tab_id),
                        drop_screen_pos: None,
                    },
                    fail_without_destination,
                );
            }
        }

        assert_eq!(source_snapshot(&app, source), before);
    }
}

/// An invalid main tear-out does not invoke installation or mutate reducer
/// accounting for a tab that never left.
#[test]
fn out_of_range_tear_out_is_a_complete_no_op() {
    let mut app = app_with_reducer_counts(3, 2);
    app.__test_seed_tab("source");
    let source = app.__test_main_window_id().expect("synthetic main");
    let before = source_snapshot(&app, source);
    let reducer = app.machine.state();
    let reducer = (reducer.tab_count, reducer.live_window_count, reducer.active_tab_idx);

    assert!(app.tear_out_tab_with_installer(99, |_, _, _, _| {
        panic!("an invalid source must not reach destination installation")
    }));

    assert_eq!(source_snapshot(&app, source), before);
    let after = app.machine.state();
    assert_eq!((after.tab_count, after.live_window_count, after.active_tab_idx), reducer);
}

/// Rollback restores the exact middle slot without stealing focus from either
/// side of the torn inactive tab.
#[test]
fn rollback_preserves_five_tab_order_and_active_identity_on_both_sides() {
    for (active_index, torn_index) in [(0, 3), (4, 1)] {
        let mut app = app_with_tabs(&["a", "b", "c", "d", "e"]);
        let window = app.__test_main_window_id().expect("synthetic main");
        assert!(app.__test_invoke_activate_main_tab(active_index));
        let before = source_snapshot(&app, window);

        assert!(app.tear_out_tab_with_installer(torn_index, |app, transaction, _, _| {
            app.tear_out_with_destination(transaction, |_app| {
                Err(DestinationFailure::probe(
                    TearOutStage::CreateWindow,
                    DestinationDisposition::Nothing,
                    Box::new(|_| {}),
                ))
            })
        }));

        assert_eq!(source_snapshot(&app, window), before);
    }
}

/// Production preparation binds every failure stage to owned cleanup while all
/// visibility and registration mutations remain in commit.
#[test]
fn production_failure_arms_own_partial_destinations_before_commit() {
    const SOURCE: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let unwind = &SOURCE[SOURCE.find("fn drop_fresh").expect("fresh unwind")
        ..SOURCE.find("fn warm(").expect("warm unwind")];
    let drop_renderer = unwind.find("drop(renderer)").expect("renderer drop");
    let drop_window = unwind.find("drop(window)").expect("window drop");
    assert!(drop_renderer < drop_window);

    let prepare = source_method(SOURCE, "prepare_tear_out_destination");
    for cleanup in [
        "DestinationUnwind::nothing()",
        "DestinationUnwind::drop_fresh(window.clone(), None)",
        "DestinationUnwind::drop_fresh(window, Some(renderer))",
        "DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm)",
    ] {
        assert!(prepare.contains(cleanup), "missing owned cleanup: {cleanup}");
    }
    assert!(prepare.contains(".with_visible(false)"));
    for forbidden in [
        "set_visible(true)",
        "insert_window_registered",
        "request_redraw()",
        "request_native_redraw(",
    ] {
        assert!(!prepare.contains(forbidden), "preparation performed commit action: {forbidden}");
    }

    let commit = source_method(SOURCE, "commit_torn_out_window");
    for required in [
        "insert_window_registered",
        "resize_visible_panes_in_child",
        "set_visible(true)",
        "request_native_redraw(&destination.window)",
    ] {
        assert!(commit.contains(required), "commit omitted action: {required}");
    }
}

/// A failed tear-out keeps the same shell process and accepts input without a
/// source-side PTY resize.
#[test]
fn a_rolled_back_tear_out_keeps_the_same_live_shell_usable() {
    #[cfg(unix)]
    let (command, args, baseline_command, rollback_command) = (
        "/bin/sh",
        Vec::<String>::new(),
        b"printf 'SONICTERM_BASELINE_OK\\n'\n".to_vec(),
        b"printf 'SONICTERM_ROLLBACK_OK\\n'\n".to_vec(),
    );
    #[cfg(windows)]
    let (command, args, baseline_command, rollback_command) = (
        "cmd.exe",
        vec!["/D".to_owned(), "/Q".to_owned()],
        b"echo SONICTERM_BASELINE_OK\r\n".to_vec(),
        b"echo SONICTERM_ROLLBACK_OK\r\n".to_vec(),
    );
    let mut pty = sonicterm_io::pty::PtyHandle::spawn_with_args(command, &args, 73, 19)
        .expect("spawn long-lived shell");
    let pid = pty.pid().expect("live shell pid");
    let exit_probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    assert_pty_marker(&pty, baseline_command, b"SONICTERM_BASELINE_OK");
    let resize_calls = Arc::new(AtomicUsize::new(0));
    let counter = resize_calls.clone();
    let resize = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
    pty.resize = Box::new(move |cols, rows| {
        counter.fetch_add(1, Ordering::Relaxed);
        resize(cols, rows)
    });

    let mut app = app_with_reducer_counts(1, 1);
    let pane_id = app.__test_seed_tab("live shell");
    let window = app.__test_main_window_id().expect("synthetic main");
    assert!(app.__test_set_pane_pty(pane_id, Some(pty)));
    prepare_non_vacuous_source(&mut app, window, pane_id);

    assert!(app.tear_out_tab_with_installer(0, |app, transaction, _, _| {
        app.tear_out_with_destination(transaction, |_app| {
            Err(DestinationFailure::probe(
                TearOutStage::RendererConfigure,
                DestinationDisposition::DropFresh,
                Box::new(|_| {}),
            ))
        })
    }));

    let pane = app.main().expect("main window").panes.get(&pane_id).expect("restored pane");
    let restored = pane.pty.as_ref().expect("restored PTY");
    assert_eq!(restored.pid(), Some(pid));
    assert!(!exit_probe.has_exited().expect("probe live shell"));
    assert_eq!(resize_calls.load(Ordering::Relaxed), 0);
    assert_pty_marker(restored, rollback_command, b"SONICTERM_ROLLBACK_OK");

    drop(app);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !exit_probe.has_exited().expect("probe shell cleanup") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(exit_probe.has_exited().expect("shell cleaned up after app drop"));
}

/// Failed destination setup restores the exact inactive main tab after running
/// destination cleanup while that tab is still detached.
#[test]
fn failed_main_tear_out_restores_order_focus_and_live_pane_state() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    assert!(app.__test_invoke_activate_main_tab(2));
    let torn = app.tab_id_at(window, 1).expect("middle tab");
    let active = app.tab_id_at(window, 2).expect("active tab");
    let pane = app.main_tab_states().expect("main tab states")[1].active_pane;
    {
        let pane_state = app.main_mut().expect("main window").panes.get_mut(&pane).expect("pane");
        pane_state.parser.lock().grid_mut().resize(73, 19);
        *pane_state.redraw_target.lock() = Some(window);
    }
    app.__test_reconcile_pane_owners();
    app.__test_charge_pane_owners();
    let owner = app.__test_pane_owner(window, pane).expect("pane owner");
    let charges = app.__test_pane_charges(window, pane).expect("pane charges");
    assert!(charges.values().any(|amount| !amount.is_zero()), "precondition: nonzero charge");
    let order: Vec<_> =
        app.main_tabs().expect("main tabs").tabs().iter().map(|tab| tab.id).collect();
    let cleanup_ran = std::rc::Rc::new(Cell::new(false));
    let cleanup_observed_detached = std::rc::Rc::new(Cell::new(false));
    let ran = cleanup_ran.clone();
    let observed = cleanup_observed_detached.clone();

    let handled = app.tear_out_tab_with_installer(1, move |app, transaction, _, _| {
        app.tear_out_with_destination(transaction, |_app| {
            let ran = ran.clone();
            let observed = observed.clone();
            Err(DestinationFailure::probe(
                TearOutStage::RendererConfigure,
                DestinationDisposition::DropFresh,
                Box::new(move |app| {
                    ran.set(true);
                    observed.set(app.tab_index_of_id(window, torn).is_none());
                }),
            ))
        })
    });

    assert!(handled);
    assert!(cleanup_ran.get());
    assert!(cleanup_observed_detached.get());
    assert_eq!(
        app.main_tabs().expect("main tabs").tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
        order
    );
    assert_eq!(app.main_tabs().and_then(|tabs| tabs.active()).map(|tab| tab.id), Some(active));
    assert_eq!(app.__test_pane_grid_size(pane), Some((73, 19)));
    assert_eq!(*app.__test_pane_redraw_target(pane).expect("redraw target").lock(), Some(window));
    assert_eq!(app.__test_pane_owner(window, pane), Some(owner));
    assert_eq!(app.__test_pane_charges(window, pane), Some(charges));
}

/// A tab closing at a lower index must not redirect a queued tear-out.
///
/// This is the whole defect: the recorded index stays *in range* when a tab
/// below it closes, so a bounds check passes while the slot now holds a
/// different tab. Asserting both halves — that the stale index misnames the
/// tab, and that the id still finds it — is what makes this a test of the fix
/// rather than of `position`.
#[test]
fn a_lower_tab_closing_moves_the_index_but_not_the_identity() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");

    // The user grabs the middle tab.
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");
    let last = app.tab_id_at(window, 2).expect("tab at index 2");
    assert_ne!(grabbed, last, "test setup: three distinct tabs");

    // Its neighbour below closes mid-gesture — a shell exiting, now that a
    // background thread can close a tab with no user action to serialise
    // against the drag.
    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, grabbed),
        Some(0),
        "the grabbed tab moved down one slot and must still be findable there"
    );
    // The half that shows why the id is needed: the recorded index survives
    // the close and now names the wrong tab.
    assert_eq!(
        app.tab_id_at(window, 1),
        Some(last),
        "the recorded index 1 now holds the tab that was at 2 — a build trusting the index \
         would tear out this one instead of the one the user grabbed"
    );
}

/// A tab that closed entirely must fail the tear-out, not promote a neighbour.
#[test]
fn a_tab_that_closed_resolves_to_nothing() {
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let doomed = app.tab_id_at(window, 0).expect("tab at index 0");

    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, doomed),
        None,
        "a closed tab must resolve to nothing so the tear-out fails, rather than moving \
         whichever tab inherited its slot"
    );
    // And the surviving tab is still reachable by its own id, so the failure
    // above is specific rather than the lookup being broken outright.
    let survivor = app.tab_id_at(window, 0).expect("survivor at index 0");
    assert_eq!(app.tab_index_of_id(window, survivor), Some(0));
}

/// The drain must resolve through the id, not the recorded index.
///
/// This drives `resolve_tear_out_source_index` — the method the drain calls —
/// rather than the lookup helpers underneath it. That distinction is the point
/// of the test: an earlier version exercised only the helpers, and a build that
/// ignored the recorded id entirely and trusted the stale index passed it.
#[test]
fn a_queued_tear_out_follows_its_tab_when_a_lower_tab_closes() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");

    // The request as the gesture recorded it.
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    // A shell exits in the tab below and closes it mid-gesture.
    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(0),
        "the drop must move the tab the user grabbed, which is now at index 0 — resolving to \
         the recorded index 1 would tear out the tab that inherited the slot"
    );
}

/// A tear-out whose tab closed must fail rather than move a neighbour.
#[test]
fn a_queued_tear_out_fails_when_its_own_tab_closed() {
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 0).expect("tab at index 0");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 0,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        None,
        "the grabbed tab is gone, so the tear-out must fail; index 0 still resolves to a live \
         tab and a build trusting it would tear out the wrong one"
    );
}

/// A request with no recorded id keeps the old index behaviour.
#[test]
fn a_tear_out_without_an_id_falls_back_to_its_index() {
    let app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: None,
        drop_screen_pos: None,
    };

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(1),
        "a request built without an id must still resolve, or the fallback path is dead"
    );
}
